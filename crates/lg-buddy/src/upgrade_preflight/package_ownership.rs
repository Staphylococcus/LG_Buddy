// Package-ownership probe (#133 / #301).
//
// Answers "who owns an installed file, if anyone" so the clobber guard
// (#304), the provisioning changes (#302), and the updater can tell a
// package-managed install from a plain release-bundle install. This module
// is pure *observation*: it records what the host package database says.
// Deciding how to *refuse* a mutation of a package-owned path is the
// guard's job, not this module's — mirroring the preflight split between
// observation and judgment.
//
// The probe is fail-closed by construction: `owner_of` returns
// `Result<PathOwnership, PackageDatabaseError>`, never `Option`. A missing
// database, an unparseable result, or a tool failure is an `Err` — it is
// NOT "definitely unowned". The clobber guard treats `Err` and
// `Conflicting` as "cannot safely proceed", so a probe that cannot run can
// never be mistaken for a clean slate.
//
// #301 ships probes for the three host package databases LG_Buddy runs on:
// `dpkg-query` (Debian/Ubuntu), `rpm` (Fedora/RHEL-family), and `pacman`
// (Arch). On a host with none of them, the probe fails closed rather than
// guessing; #135 adds the *package-managed* RPM delivery and its
// family-specific update logic on top of the `Rpm` family already probed
// here.
//
// Two questions, deliberately kept separate. The *general* path-level
// answer (arbitrary path) does not reveal the *installation-level* answer
// (is the whole install package-managed?); that is answered by probing the
// installed *executable* and gating the result on the installed layout —
// see `installation_ownership`. "Unowned" alone is NOT `Bundle`: an
// install is a bundle only when the executable is unowned *and* the
// installed layout satisfies the release-bundle contract.

use std::path::Path;
use std::process::Command;

/// Which package-manager family an owner was reported by. Family is *data*
/// on the owner, not a dispatch: consumers (and the future
/// package-version serialization in #306/#134/#135) match on the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFamily {
    /// A `dpkg`-managed install (Debian/Ubuntu).
    Dpkg,
    /// An `rpm`-managed install (Fedora/RHEL-family). Probed here; #135
    /// adds the package-managed delivery and family-specific update logic.
    Rpm,
    /// A `pacman`-managed install (Arch Linux).
    Pacman,
}

impl PackageFamily {
    /// Stable, family-specific identity string. Consumed by the future
    /// package-version serialization (#306); not a full version encoding
    /// here.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dpkg => "dpkg",
            Self::Rpm => "rpm",
            Self::Pacman => "pacman",
        }
    }
}

/// A single package that owns a path, with the family that reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageOwner {
    pub family: PackageFamily,
    /// The owning package id — the package name, optionally
    /// architecture-qualified (e.g. `lg-buddy`, `libc6:amd64`).
    pub name: String,
    /// The owning package version string, when the tool reported one.
    pub version: Option<String>,
}

/// The *path-level* ownership answer. `Conflicting` is a first-class result,
/// not an error: two packages both claiming the same file is a real host
/// state, and the clobber guard must refuse it — distinct from "the probe
/// itself failed" (`Err`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOwnership {
    /// No package claims this path. The app may own/mutate it.
    Unowned,
    /// Exactly one package claims this path.
    Owned(PackageOwner),
    /// More than one package claims this path. The guard must refuse.
    Conflicting(Vec<PackageOwner>),
}

impl PathOwnership {
    /// Fail-closed predicate for the clobber guard (#304): `Owned` or
    /// `Conflicting` blocks mutation of the path. (`Err` blocks too, but
    /// that is handled by the `Result`, not this value.)
    pub fn blocks_mutation(&self) -> bool {
        matches!(self, Self::Owned(_) | Self::Conflicting(_))
    }
}

/// Why the probe could not answer a question. One variant for now; the
/// fail-closed contract is "any `Err` blocks" — the guard lumps every cause
/// (missing db, invalid output, exec failure) as "cannot proceed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageDatabaseError {
    /// The package database could not answer (tool absent, exec failure, a
    /// non-zero non-"unowned" exit, or unparseable output).
    ProbeFailed(String),
}

impl std::fmt::Display for PackageDatabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProbeFailed(reason) => write!(f, "package database probe failed: {reason}"),
        }
    }
}

impl std::error::Error for PackageDatabaseError {}

/// The raw result of asking *one* package database about a path. Kept
/// separate from `PathOwnership` so the *judgment* (exit-code/output
/// semantics, per family) stays testable and the *observation* (what the
/// tool printed) stays swappable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The tool ran. `code` is its exit status; `stdout` / `stderr` carry
    /// the output (both are kept: some tools report the unowned case on
    /// stderr, e.g. `pacman -Qq --owns`).
    Ran {
        code: i32,
        stdout: String,
        stderr: String,
    },
    /// This database is genuinely *absent* on the host (the executable was
    /// not found, so it is not part of the answer). The other databases
    /// still answer; only a host with *no* databases at all fails closed.
    Unavailable,
    /// The database is present but the probe *failed to run* it (exec
    /// error, permission denied, …) — a failure, NOT "absent". Aggregation
    /// treats this as an error, never as a clean "unowned": a db that
    /// exists but can't execute must not let a secondary db's "unowned"
    /// stand on its own.
    Failed(String),
}

/// A single database entry: an absolute executable path (never
/// `PATH`-resolved — see `system`), its arguments, the family it belongs
/// to, and environment variables to remove before running (the knobs that
/// could redirect which database is queried: `DPKG_ROOT` / `DPKG_ADMINDIR`
/// for dpkg; `HOME` / `XDG_CONFIG_HOME` / `RPM_CONFIGDIR` for rpm, which
/// would otherwise load a per-user macro layer and re-point `%_dbpath`).
#[derive(Debug, Clone)]
pub struct Database {
    pub name: &'static str,
    pub exec: &'static str,
    pub args: &'static [&'static str],
    pub family: PackageFamily,
    pub env_remove: &'static [&'static str],
}

/// The raw probe runner: ask one database whether a path is owned. Kept
/// as a named alias so the struct/`with_probe` signatures stay short
/// (clippy: "very complex type").
pub type ProbeFn = Box<dyn Fn(&Database, &Path) -> ProbeOutcome + Send + Sync>;

/// A probe over the *host package databases*. At #301 all three families
/// (`dpkg`, `rpm`, `pacman`) are probed; the probe is injectable so tests
/// can supply fixtures without the packaged binaries present.
pub struct PackageDatabase {
    databases: Vec<Database>,
    /// The raw probe: ask one database (by reference) whether a path is
    /// owned. Injectable so tests don't need the real binaries.
    probe: ProbeFn,
}

impl std::fmt::Debug for PackageDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackageDatabase")
            .field("databases", &self.databases.len())
            .finish_non_exhaustive()
    }
}

impl PackageDatabase {
    /// The default probe: the three host package databases, addressed by
    /// absolute path.
    ///
    /// Security: the probe result can eventually authorize privileged
    /// mutation, so the probe must not trust the inherited process
    /// environment. Every executable is addressed by absolute path (never
    /// resolved through `PATH`), and per-database environment knobs are
    /// removed — `DPKG_ROOT` / `DPKG_ADMINDIR` for dpkg, and
    /// `HOME` / `XDG_CONFIG_HOME` / `RPM_CONFIGDIR` for rpm (its per-user
    /// macro layer) — so an attacker-controlled environment cannot
    /// redirect which database is queried. A fake `dpkg-query` that exits
    /// 1 must not be able to forge "unowned".
    ///
    /// `LC_ALL=C.UTF-8` (and clearing `LANGUAGE`) pins output to the
    /// documented, reproducible C-locale form — `dpkg`'s man page
    /// recommends this when machine-parsing, because diversion records are
    /// otherwise printed with *localized* prefixes.
    pub fn system() -> Self {
        Self::with_probe(
            vec![
                Database {
                    name: "dpkg-query",
                    exec: "/usr/bin/dpkg-query",
                    args: &["-S"],
                    family: PackageFamily::Dpkg,
                    env_remove: &["DPKG_ROOT", "DPKG_ADMINDIR"],
                },
                Database {
                    name: "rpm",
                    // Query only the owning package NAME(s) — one line per
                    // owner. `rpm -qf`'s default output (the NEVRA, e.g.
                    // `lg-buddy-1.10.0-1.fc42.x86_64`) is not reliable:
                    // package names contain '-', so the name cannot be
                    // split off, and a single file's default output may
                    // omit the release/arch. `%{NAME}` is exact.
                    exec: "/usr/bin/rpm",
                    args: &["-qf", "--qf", "%{NAME}\\n"],
                    family: PackageFamily::Rpm,
                    // rpm loads a per-user macro layer
                    // (`~/.config/rpm/macros`, via `HOME` /
                    // `XDG_CONFIG_HOME`; `RPM_CONFIGDIR` re-points it)
                    // after the vendor/host settings, and `%_dbpath` is a
                    // runtime macro. Removing the user layer neutralizes
                    // that override while leaving `/etc/rpm` +
                    // `/usr/lib/rpm` (and the default db path) intact.
                    env_remove: &["HOME", "XDG_CONFIG_HOME", "RPM_CONFIGDIR"],
                },
                Database {
                    name: "pacman",
                    // `-Qq --owns <path>`: with `--quiet`, `--owns` prints
                    // only the owning package name(s), one per line (no
                    // human-readable sentence to parse).
                    exec: "/usr/bin/pacman",
                    args: &["-Qq", "--owns"],
                    family: PackageFamily::Pacman,
                    env_remove: &[],
                },
            ],
            |db, path| {
                let mut cmd = Command::new(db.exec);
                cmd.args(db.args).arg(path);
                for var in db.env_remove {
                    cmd.env_remove(var);
                }
                cmd.env("LC_ALL", "C.UTF-8").env_remove("LANGUAGE");
                match cmd.output() {
                    Ok(output) => ProbeOutcome::Ran {
                        code: output.status.code().unwrap_or(-1),
                        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    },
                    // Classify the spawn failure: only a genuinely *missing*
                    // executable means "this host has no such database".
                    // Any other error (permission denied, exec format, …)
                    // means the database is present but unrunnable — a probe
                    // *failure*, never "absent" (which would let a secondary
                    // db's "unowned" stand alone).
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        ProbeOutcome::Unavailable
                    }
                    Err(err) => ProbeOutcome::Failed(format!("{0}: {err}", db.exec)),
                }
            },
        )
    }

    /// A probe backed by an injectable runner (a test fixture). The runner
    /// returns the *raw* outcome for each database; `owner_of` applies the
    /// per-family semantics.
    pub fn with_probe(
        databases: Vec<Database>,
        runner: impl Fn(&Database, &Path) -> ProbeOutcome + Send + Sync + 'static,
    ) -> Self {
        Self {
            databases,
            probe: Box::new(runner),
        }
    }

    /// The *path-level* ownership question: which package, if any, owns
    /// `path`? Never returns `None`/ambiguous — a database that cannot
    /// answer is an `Err`, which the guard treats as "cannot proceed".
    ///
    /// Aggregation across databases:
    /// - every database reports *unowned* → `Unowned`;
    /// - exactly one owner across all databases → `Owned`;
    /// - more than one owner (within or across databases) → `Conflicting`;
    /// - a database that is present but cannot answer (exit error,
    ///   unparseable output, or the pacman string did not match either the
    ///   owned or the unowned form) → `Err` (fail closed);
    /// - no database is available on this host → `Err` (no way to answer).
    pub fn owner_of(&self, path: &Path) -> Result<PathOwnership, PackageDatabaseError> {
        let mut all_owners: Vec<PackageOwner> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut saw_available_db = false;
        for db in &self.databases {
            match (self.probe)(db, path) {
                ProbeOutcome::Unavailable => continue,
                ProbeOutcome::Failed(reason) => {
                    // A database that exists but cannot run is a failure —
                    // record it and keep probing; the aggregate is then an
                    // `Err` (fail closed), never a clean "unowned".
                    errors.push(reason);
                    continue;
                }
                ProbeOutcome::Ran {
                    code,
                    stdout,
                    stderr,
                } => {
                    saw_available_db = true;
                    match db.family {
                        PackageFamily::Dpkg => {
                            // `dpkg-query -S`: 0 = owned (parse stdout),
                            // 1 = not found = definitively unowned,
                            // otherwise = db/tool error.
                            if code == 0 {
                                match parse_dpkg_owners(stdout.lines(), path, db.family) {
                                    Ok(owners) => all_owners.extend(owners),
                                    Err(reason) => errors.push(format!(
                                        "dpkg-query: {reason} for {}",
                                        path.display()
                                    )),
                                }
                            } else if code == 1 {
                                // "no path found matching pattern" — a clean
                                // "dpkg does not own this" answer.
                            } else {
                                errors.push(format!(
                                    "dpkg-query exited {code} for {}",
                                    path.display()
                                ));
                            }
                        }
                        PackageFamily::Rpm => {
                            // `rpm -qf --qf '%{NAME}\n'`: 0 = owned (one
                            // package name per line on stdout), 1 = not
                            // owned by any package, 2 = db/error.
                            if code == 0 {
                                match parse_rpm_names(&stdout, db.family) {
                                    Ok(owners) => all_owners.extend(owners),
                                    Err(reason) => {
                                        errors.push(format!("rpm: {reason} for {}", path.display()))
                                    }
                                }
                            } else if code == 1 {
                                // "does not belong to any package".
                            } else {
                                errors.push(format!("rpm exited {code} for {}", path.display()));
                            }
                        }
                        PackageFamily::Pacman => {
                            // `pacman -Qq --owns`: with `--quiet`, `--owns`
                            // prints *only* the owning package name(s), one
                            // per line — no human-readable sentence to
                            // parse, and the queried path is not echoed
                            // back, so there is no "did the record name the
                            // path we asked for?" hole.
                            //
                            // Verdict: exit 0 with names on stdout =
                            // owned. A non-zero exit whose stderr carries
                            // the documented `No package owns` marker =
                            // unowned. Any other non-zero output is
                            // unverified — fail closed (we do not trust a
                            // specific unowned exit code we cannot
                            // confirm here).
                            if code == 0 {
                                match parse_pacman_names(&stdout, db.family) {
                                    Ok(owners) => all_owners.extend(owners),
                                    Err(reason) => errors
                                        .push(format!("pacman: {reason} for {}", path.display())),
                                }
                            } else if stderr.contains("No package owns") {
                                // Documented unowned marker (stderr).
                            } else {
                                errors.push(format!(
                                    "pacman exited {code} with unexpected output for {}",
                                    path.display()
                                ));
                            }
                        }
                    }
                }
            }
        }
        if !saw_available_db {
            return Err(PackageDatabaseError::ProbeFailed(
                "no package database available on this host".into(),
            ));
        }
        if !errors.is_empty() {
            return Err(PackageDatabaseError::ProbeFailed(errors.join("; ")));
        }
        Ok(ownership_from_owners(all_owners))
    }
}

/// Map an *installation-level* probe result to `InstallationOwnership`.
///
/// Fail-closed, and `Bundle` is *layout-gated*: an unowned executable is a
/// bundle only when the installed layout also satisfies the release-bundle
/// installation contract (`bundle_layout`, computed from the preflight
/// installed-layout observations). An unowned executable in a layout that
/// is NOT a recognizable bundle is `Unknown`, not a blessed `Bundle` —
/// arbitrary or manual installs are not to be mistaken for a bundle the
/// guard can act on. A conflicting report or any probe failure is also
/// `Unknown`.
pub fn installation_ownership(
    executable: Result<PathOwnership, PackageDatabaseError>,
    bundle_layout: bool,
) -> InstallationOwnership {
    match executable {
        // A named owner is a package-managed install under that family.
        Ok(PathOwnership::Owned(owner)) => InstallationOwnership::Package {
            family: owner.family,
        },
        // Unowned is a bundle only under a valid bundle layout; otherwise
        // the install is not one we recognize, so refuse to bless it.
        Ok(PathOwnership::Unowned) => {
            if bundle_layout {
                InstallationOwnership::Bundle
            } else {
                InstallationOwnership::Unknown
            }
        }
        // Conflicting ownership, or a probe that could not answer: fail
        // closed.
        Ok(PathOwnership::Conflicting(_)) | Err(_) => InstallationOwnership::Unknown,
    }
}

/// The *installation-level* ownership answer. `Bundle` (a plain
/// release-bundle install, unowned) and `Package` (managed by a named
/// family) are the two usable states; `Unknown` is fail-closed — the guard
/// refuses to act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationOwnership {
    /// A plain release-bundle install: the executable is unowned *and* the
    /// installed layout satisfies the release-bundle contract.
    Bundle,
    /// A package-managed install, under the named family.
    Package { family: PackageFamily },
    /// The probe could not produce a definitive answer, or the layout is
    /// not a recognized bundle. Fail-closed: consumers refuse.
    Unknown,
}

impl InstallationOwnership {
    /// Fail-closed predicate: only a confirmed `Bundle` or `Package` is
    /// safe to act on; `Unknown` blocks.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// Parse `dpkg-query -S <path>` stdout (C-locale form, as pinned by the
/// probe) into owners. The documented output is one ownership record —
/// `pkgname1, pkgname2: pathname`, owning packages comma-separated — plus
/// zero or more diversion records. Any other line, a blank line, or an
/// ownership record naming a different path is an error: a successful run
/// with output we cannot read must fail closed, never read as `Unowned`
/// (which only exit code 1 may report).
pub fn parse_dpkg_owners<'a>(
    lines: impl Iterator<Item = &'a str>,
    path: &Path,
    family: PackageFamily,
) -> Result<Vec<PackageOwner>, String> {
    let expected = path.to_string_lossy().into_owned();
    let mut owners: Vec<String> = Vec::new();
    let mut saw_record = false;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            return Err("blank line in successful dpkg -S output".into());
        }
        // Documented diversion records (`diversion by pkg from: …`,
        // `diversion by pkg to: …`, `local diversion from: …`,
        // `local diversion to: …`; C-locale form) carry no ownership.
        if line.starts_with("diversion by ") || line.starts_with("local diversion ") {
            continue;
        }
        // Ownership record: `pkg1, pkg2: pathname`, owners comma-separated.
        // An owner id may carry a single ":" (the architecture qualifier,
        // e.g. `libc6:amd64`) but never ": ", so the first ": " delimits
        // the owner list from the path.
        let (list, record_path) = line
            .split_once(": ")
            .ok_or_else(|| format!("unparseable dpkg -S record: {line}"))?;
        // The record must be the path we queried; a different one means
        // dpkg did not answer our question — not something to accept.
        if record_path != expected {
            return Err(format!(
                "dpkg -S reported {record_path} for query {}",
                path.display()
            ));
        }
        // Owning packages are separated by ", " (comma + space).
        for owner in list.split(", ") {
            let owner = owner.trim();
            if !is_package_id(owner) {
                return Err(format!("invalid package id in dpkg -S record: {owner}"));
            }
            if !owners.iter().any(|existing| existing == owner) {
                owners.push(owner.to_string());
            }
        }
        saw_record = true;
    }
    if !saw_record {
        return Err("no ownership record in successful dpkg -S output".into());
    }
    Ok(owners
        .into_iter()
        .map(|name| PackageOwner {
            family,
            name,
            version: None,
        })
        .collect())
}

/// Parse `rpm -qf --qf '%{NAME}\\n'` stdout (C-locale form, as pinned by
/// the probe) into owners. The query prints one owning package *name* per
/// line (we asked for `%{NAME}` specifically, not the default NEVRA, so a
/// name containing `-` is not mis-split). A single owned file yields a
/// single owner; more than one line is a genuine conflict.
///
/// This is only called after `rpm` reported *success*, so at least one
/// valid name is expected: an empty / all-blank output is an error, not
/// "zero owners" (which would read as `Unowned`). Any line that is not a
/// valid package name is also an error (fail closed).
fn parse_rpm_names(stdout: &str, family: PackageFamily) -> Result<Vec<PackageOwner>, String> {
    let mut owners = Vec::new();
    for line in stdout.lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        if !is_rpm_name(name) {
            return Err(format!("invalid package name in rpm -qf output: {line}"));
        }
        if !owners.iter().any(|existing| existing == name) {
            owners.push(name.to_string());
        }
    }
    if owners.is_empty() {
        return Err("no package name in successful rpm -qf output".into());
    }
    Ok(owners
        .into_iter()
        .map(|name| PackageOwner {
            family,
            name,
            version: None,
        })
        .collect())
}

/// Parse `pacman -Qq --owns` stdout (C-locale form, as pinned by the
/// probe) into owners. With `--quiet`, `--owns` prints one owning package
/// *name* per line (no path, no version, no human-readable sentence). A
/// single owned file yields a single owner; more than one line is a
/// genuine conflict. This is only called after a successful run, so at
/// least one valid name is expected: empty / invalid output is an error
/// (fail closed), never "zero owners".
fn parse_pacman_names(stdout: &str, family: PackageFamily) -> Result<Vec<PackageOwner>, String> {
    let mut owners = Vec::new();
    for line in stdout.lines() {
        let name = line.trim();
        if name.is_empty() {
            continue;
        }
        if !is_pacman_name(name) {
            return Err(format!(
                "invalid package name in pacman -Qq --owns output: {line}"
            ));
        }
        if !owners.iter().any(|existing| existing == name) {
            owners.push(name.to_string());
        }
    }
    if owners.is_empty() {
        return Err("no package name in successful pacman -Qq --owns output".into());
    }
    Ok(owners
        .into_iter()
        .map(|name| PackageOwner {
            family,
            name,
            version: None,
        })
        .collect())
}

/// A pacman package name: ASCII alphanumerics plus `+`, `.`, `-`, `_`
/// (e.g. `mingw-w64-x86_64-ntldd`). Anything else is rejected, so a line
/// that pretends to be an ownership record cannot be accepted.
fn is_pacman_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-' | '_'))
}

/// An rpm package name: ASCII alphanumerics plus `+`, `.`, `-`, `_`
/// (e.g. `mingw-w64-x86_64-ntldd`, `kde-filesystem`).
fn is_rpm_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-' | '_'))
}

/// A dpkg/rpm *package id*: an ASCII-alphanumeric package name (then
/// alphanumerics plus `+`, `.`, `-`) with an optional `:<architecture>`
/// qualifier (e.g. `libc6:amd64`, `libc6:hurd-i386`). Multi-arch output
/// reports the architecture-qualified id; two owners of the same name on
/// different arches are distinct packages, so they stay separate owners
/// (and a genuine conflict when they claim one file). Anything else is
/// rejected, so a line that pretends to be an ownership record cannot be
/// accepted.
fn is_package_id(owner: &str) -> bool {
    let (name, arch) = match owner.split_once(':') {
        Some((name, arch)) => (name, Some(arch)),
        None => (owner, None),
    };
    let valid_name = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .skip(1)
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    if !valid_name {
        return false;
    }
    // An architecture qualifier, if present, is a lowercase token of
    // alphanumerics and dashes (e.g. `amd64`, `i386`, `arm64`,
    // `hurd-i386`, `kfreebsd-amd64`) with no further ':'.
    match arch {
        Some(arch) => {
            !arch.is_empty()
                && !arch.contains(':')
                && arch
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        None => true,
    }
}

/// Combine a list of owners into a `PathOwnership`. Empty -> `Unowned`;
/// one -> `Owned`; two or more -> `Conflicting`.
fn ownership_from_owners(owners: Vec<PackageOwner>) -> PathOwnership {
    if owners.is_empty() {
        PathOwnership::Unowned
    } else if owners.len() == 1 {
        PathOwnership::Owned(
            owners
                .into_iter()
                .next()
                .expect("ownership_from_owners(len()==1) yields exactly one owner"),
        )
    } else {
        PathOwnership::Conflicting(owners)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- fixtures ------------------------------------------------------

    fn dpkg_db() -> Database {
        Database {
            name: "dpkg-query",
            exec: "/usr/bin/dpkg-query",
            args: &["-S"],
            family: PackageFamily::Dpkg,
            env_remove: &[],
        }
    }
    fn rpm_db() -> Database {
        Database {
            name: "rpm",
            exec: "/usr/bin/rpm",
            args: &["-qf", "--qf", "%{NAME}\\n"],
            family: PackageFamily::Rpm,
            env_remove: &["HOME", "XDG_CONFIG_HOME", "RPM_CONFIGDIR"],
        }
    }
    fn pacman_db() -> Database {
        Database {
            name: "pacman",
            exec: "/usr/bin/pacman",
            args: &["-Qq", "--owns"],
            family: PackageFamily::Pacman,
            env_remove: &[],
        }
    }

    /// A single-database fixture that always reports the given outcome.
    fn fixture(db: Database, outcome: ProbeOutcome) -> PackageDatabase {
        PackageDatabase::with_probe(vec![db], move |_db, _path| outcome.clone())
    }

    /// A multi-database fixture; `outcomes` is indexed by database (matched
    /// by name, so the order of `dbs` and `outcomes` must agree).
    fn multi_fixture(dbs: Vec<Database>, outcomes: Vec<ProbeOutcome>) -> PackageDatabase {
        let names: Vec<String> = dbs.iter().map(|d| d.name.to_string()).collect();
        PackageDatabase::with_probe(dbs, move |db, _path| {
            let idx = names.iter().position(|n| n == db.name).unwrap_or(0);
            outcomes[idx].clone()
        })
    }

    fn ran(code: i32, stdout: &str, stderr: &str) -> ProbeOutcome {
        ProbeOutcome::Ran {
            code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    fn owner(family: PackageFamily, name: &str) -> PackageOwner {
        PackageOwner {
            family,
            name: name.into(),
            version: None,
        }
    }

    // --- dpkg parser (pure) -------------------------------------------

    #[test]
    fn parse_dpkg_single_owner() {
        let owners = parse_dpkg_owners(
            ["lg-buddy: /usr/bin/lg-buddy"].iter().copied(),
            Path::new("/usr/bin/lg-buddy"),
            PackageFamily::Dpkg,
        )
        .unwrap();
        assert_eq!(owners, vec![owner(PackageFamily::Dpkg, "lg-buddy")]);
    }

    #[test]
    fn parse_dpkg_arch_qualified_owner_is_valid() {
        // Multi-arch output reports architecture-qualified ids
        // (`libc6:amd64`); these are valid owners, not parse failures.
        let owners = parse_dpkg_owners(
            ["libc6:amd64: /usr/lib/x86_64-linux-gnu/libc.so.6"]
                .iter()
                .copied(),
            Path::new("/usr/lib/x86_64-linux-gnu/libc.so.6"),
            PackageFamily::Dpkg,
        )
        .unwrap();
        assert_eq!(owners, vec![owner(PackageFamily::Dpkg, "libc6:amd64")]);
    }

    #[test]
    fn parse_dpkg_arch_conflict_is_distinct() {
        // Two owners of the same base name on different arches are distinct
        // packages; one record carrying both is a genuine conflict.
        let owners = parse_dpkg_owners(
            ["libfoo:i386, libfoo:amd64: /usr/bin/x"].iter().copied(),
            Path::new("/usr/bin/x"),
            PackageFamily::Dpkg,
        )
        .unwrap();
        assert_eq!(owners.len(), 2);
        assert_eq!(owners[0].name, "libfoo:i386");
        assert_eq!(owners[1].name, "libfoo:amd64");
    }

    #[test]
    fn is_package_id_accepts_and_rejects_qualifiers() {
        assert!(is_package_id("lg-buddy")); // plain, no arch
        assert!(is_package_id("libc6:amd64")); // valid arch
        assert!(is_package_id("libc6:hurd-i386")); // dash in arch
        assert!(is_package_id("libc6:kfreebsd-amd64"));
        assert!(!is_package_id("libc6:")); // empty arch
        assert!(!is_package_id("libc6:amd64:x")); // extra colon
        assert!(!is_package_id("libc6:AMD64")); // uppercase
    }

    #[test]
    fn parse_dpkg_comma_separated_multiowner() {
        // Documented format: `pkg1, pkg2: pathname` — one record carrying
        // several owners, NOT one line per package.
        let owners = parse_dpkg_owners(
            ["alpha, beta: /usr/bin/x"].iter().copied(),
            Path::new("/usr/bin/x"),
            PackageFamily::Dpkg,
        )
        .unwrap();
        assert_eq!(owners.len(), 2);
        assert_eq!(owners[0].name, "alpha");
        assert_eq!(owners[1].name, "beta");
    }

    #[test]
    fn parse_dpkg_diversion_records_are_ignored() {
        // Documented C-locale diversion records carry no ownership.
        let lines = [
            "lg-buddy: /usr/bin/lg-buddy",
            "diversion by lg-buddy from: /usr/bin/lg-buddy",
            "diversion by lg-buddy to: /usr/lib/diverted",
            "local diversion from: /usr/bin/old",
            "local diversion to: /usr/bin/new",
        ];
        let owners = parse_dpkg_owners(
            lines.iter().copied(),
            Path::new("/usr/bin/lg-buddy"),
            PackageFamily::Dpkg,
        )
        .unwrap();
        assert_eq!(owners, vec![owner(PackageFamily::Dpkg, "lg-buddy")]);
    }

    #[test]
    fn parse_dpkg_deduplicates_repeated_package() {
        // A package appearing on two records is still a single owner.
        let owners = parse_dpkg_owners(
            ["lg-buddy: /usr/bin/x", "lg-buddy: /usr/bin/x"]
                .iter()
                .copied(),
            Path::new("/usr/bin/x"),
            PackageFamily::Dpkg,
        )
        .unwrap();
        assert_eq!(owners.len(), 1);
    }

    #[test]
    fn parse_dpkg_unexpected_output_fails_closed() {
        // A successful run with no parsable record is NOT "unowned" — that
        // is exit 1's exclusive job; this must error.
        assert!(
            parse_dpkg_owners([].iter().copied(), Path::new("/x"), PackageFamily::Dpkg).is_err()
        );
        let garbage = ["not a record", "no colon here"];
        assert!(parse_dpkg_owners(
            garbage.iter().copied(),
            Path::new("/x"),
            PackageFamily::Dpkg
        )
        .is_err());
        // A record naming a different path did not answer our query.
        let wrong = ["lg-buddy: /usr/bin/other"];
        assert!(parse_dpkg_owners(
            wrong.iter().copied(),
            Path::new("/usr/bin/x"),
            PackageFamily::Dpkg
        )
        .is_err());
    }

    // --- rpm / pacman parsers (pure) ----------------------------------

    #[test]
    fn parse_rpm_single_name() {
        // The probe asks for `%{NAME}` only, so the output is one plain
        // package name per line (no version/release/arch to mis-split).
        let owners = parse_rpm_names("lg-buddy\n", PackageFamily::Rpm).unwrap();
        assert_eq!(owners, vec![owner(PackageFamily::Rpm, "lg-buddy")]);
    }

    #[test]
    fn parse_rpm_rejects_garbage() {
        // Empty / all-blank stdout from a *successful* rpm is an error, not
        // "zero owners" (which would read as `Unowned`).
        assert!(parse_rpm_names("", PackageFamily::Rpm).is_err());
        assert!(parse_rpm_names("   \n", PackageFamily::Rpm).is_err());
        // A line that is not a valid package name fails closed.
        assert!(parse_rpm_names("-weird\n", PackageFamily::Rpm).is_err());
        // A dash-leading name is invalid; a plain name with dashes is fine.
        assert!(parse_rpm_names("mingw-w64-x86_64-ntldd\n", PackageFamily::Rpm).is_ok());
    }

    // --- aggregation across databases ---------------------------------

    #[test]
    fn owner_of_single_dpdk_owned() {
        let db = fixture(dpkg_db(), ran(0, "lg-buddy: /usr/bin/lg-buddy\n", ""));
        let result = db.owner_of(Path::new("/usr/bin/lg-buddy")).unwrap();
        assert_eq!(
            result,
            PathOwnership::Owned(owner(PackageFamily::Dpkg, "lg-buddy"))
        );
        assert!(result.blocks_mutation());
    }

    #[test]
    fn owner_of_all_dbs_unowned() {
        // Arch-host case: dpkg-query and rpm are absent; pacman says the
        // file is not owned by any package. Result: a clean Unowned, NOT a
        // ProbeFailed — the aggregate "no database claims this" is the
        // bundle-install signal #304 needs.
        let db = multi_fixture(
            vec![dpkg_db(), rpm_db(), pacman_db()],
            vec![
                ProbeOutcome::Unavailable,
                ProbeOutcome::Unavailable,
                ran(1, "", "error: No package owns /usr/bin/lg-buddy\n"),
            ],
        );
        let result = db.owner_of(Path::new("/usr/bin/lg-buddy")).unwrap();
        assert_eq!(result, PathOwnership::Unowned);
        assert!(!result.blocks_mutation());
    }

    #[test]
    fn owner_of_pacman_owned() {
        // `-Qq --owns` prints only the owning package name (no path, no
        // version).
        let db = fixture(pacman_db(), ran(0, "nano\n", ""));
        let result = db.owner_of(Path::new("/usr/bin/nano")).unwrap();
        let PathOwnership::Owned(o) = &result else {
            panic!("expected Owned, got {result:?}");
        };
        assert_eq!(o.family, PackageFamily::Pacman);
        assert_eq!(o.name, "nano");
        assert_eq!(o.version.as_deref(), None);
    }

    #[test]
    fn owner_of_rpm_owned() {
        // rpm emits one `%{NAME}` per line.
        let db = fixture(rpm_db(), ran(0, "lg-buddy\n", ""));
        let result = db.owner_of(Path::new("/usr/bin/lg-buddy")).unwrap();
        assert_eq!(
            result,
            PathOwnership::Owned(owner(PackageFamily::Rpm, "lg-buddy"))
        );
    }

    #[test]
    fn owner_of_cross_db_conflict() {
        // Two different packages, from two different databases, both claim
        // the same path: a genuine conflict.
        let db = multi_fixture(
            vec![dpkg_db(), rpm_db()],
            vec![
                ran(0, "lg-buddy: /usr/bin/lg-buddy\n", ""),
                ran(0, "kde-filesystem\n", ""),
            ],
        );
        let result = db.owner_of(Path::new("/usr/bin/lg-buddy")).unwrap();
        assert!(matches!(result, PathOwnership::Conflicting(_)));
        assert!(result.blocks_mutation());
    }

    #[test]
    fn owner_of_single_db_owned_conflict_within_db() {
        // A single dpkg record carrying two owners is a conflict even
        // without a second database.
        let db = fixture(dpkg_db(), ran(0, "alpha, beta: /usr/bin/x\n", ""));
        let result = db.owner_of(Path::new("/usr/bin/x")).unwrap();
        assert!(matches!(result, PathOwnership::Conflicting(_)));
    }

    #[test]
    fn owner_of_no_database_available_fails_closed() {
        // A host with none of the three databases: no way to answer, so
        // Err, never a silent Unowned.
        let db = multi_fixture(
            vec![dpkg_db(), rpm_db(), pacman_db()],
            vec![
                ProbeOutcome::Unavailable,
                ProbeOutcome::Unavailable,
                ProbeOutcome::Unavailable,
            ],
        );
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    #[test]
    fn owner_of_db_error_is_fail_closed() {
        // A non-"unowned" exit is a db error: Err, never Unowned.
        let db = fixture(dpkg_db(), ran(2, "", ""));
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
        let db = fixture(rpm_db(), ran(2, "", ""));
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    #[test]
    fn system_probe_neutralizes_user_database_config() {
        // Security regression: the default probe must strip the environment
        // knobs that would let an unprivileged caller point rpm / dpkg at a
        // different database. Without this, a fake user macro file could
        // make `rpm -qf` read an attacker db and forge "unowned".
        let db = PackageDatabase::system();
        let rpm = db.databases.iter().find(|d| d.name == "rpm").unwrap();
        assert_eq!(
            rpm.env_remove,
            &["HOME", "XDG_CONFIG_HOME", "RPM_CONFIGDIR"]
        );
        let dpkg = db
            .databases
            .iter()
            .find(|d| d.name == "dpkg-query")
            .unwrap();
        assert_eq!(dpkg.env_remove, &["DPKG_ROOT", "DPKG_ADMINDIR"]);
    }

    #[test]
    fn owner_of_present_db_that_cannot_run_fails_closed() {
        // A database that exists but fails to *run* (ProbeOutcome::Failed)
        // must not let a secondary db's "unowned" stand alone: it records a
        // probe failure and the aggregate is an Err, never a clean Unowned.
        let db = multi_fixture(
            vec![dpkg_db(), rpm_db(), pacman_db()],
            vec![
                ProbeOutcome::Failed("dpkg: permission denied".into()),
                ProbeOutcome::Unavailable,
                ran(1, "", "error: No package owns /usr/bin/lg-buddy\n"),
            ],
        );
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    #[test]
    fn owner_of_pacman_empty_success_fails_closed() {
        // `pacman -Qq --owns` that exits 0 with no names on stdout is
        // malformed, not "unowned".
        let db = fixture(pacman_db(), ran(0, "", ""));
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    #[test]
    fn owner_of_pacman_unverified_exit_fails_closed() {
        // A non-zero pacman exit that does NOT carry the documented
        // `No package owns` marker is unverified -> fail closed (we do not
        // trust a specific unowned exit code we cannot confirm here).
        let db = fixture(pacman_db(), ran(1, "", "some other error\n"));
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    #[test]
    fn owner_of_rpm_empty_success_fails_closed() {
        // `rpm -qf` that exits 0 with no names on stdout is malformed, not
        // "unowned".
        let db = fixture(rpm_db(), ran(0, "", ""));
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    #[test]
    fn owner_of_exit0_unparseable_fails_closed() {
        // Blocker: successful-but-malformed output must not fail open to
        // Unowned.
        let db = fixture(dpkg_db(), ran(0, "\n", ""));
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    #[test]
    fn owner_of_pacman_unexpected_output_fails_closed() {
        // pacman output that matches neither the owned nor the unowned
        // form is an error, never a guess.
        let db = fixture(pacman_db(), ran(0, "something unexpected\n", ""));
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    #[test]
    fn owner_of_absent_tool_is_fail_closed_when_no_db() {
        // The only database is absent: Err, never a silent Unowned.
        let db = fixture(dpkg_db(), ProbeOutcome::Unavailable);
        assert!(db.owner_of(Path::new("/usr/bin/lg-buddy")).is_err());
    }

    // --- installation_ownership (unchanged contract) ------------------

    #[test]
    fn installation_ownership_asserts_all_cases() {
        // bundle: unowned executable + valid bundle layout.
        assert_eq!(
            installation_ownership(Ok(PathOwnership::Unowned), true),
            InstallationOwnership::Bundle
        );
        // dpkg package: a named owner.
        assert_eq!(
            installation_ownership(
                Ok(PathOwnership::Owned(owner(PackageFamily::Dpkg, "lg-buddy"))),
                true
            ),
            InstallationOwnership::Package {
                family: PackageFamily::Dpkg
            }
        );
        // rpm package: a named rpm owner (#135 populates the delivery side).
        assert_eq!(
            installation_ownership(
                Ok(PathOwnership::Owned(owner(PackageFamily::Rpm, "LG_Buddy"))),
                true
            ),
            InstallationOwnership::Package {
                family: PackageFamily::Rpm
            }
        );
        // pacman package: a named pacman owner (Arch, package-managed).
        assert_eq!(
            installation_ownership(
                Ok(PathOwnership::Owned(owner(
                    PackageFamily::Pacman,
                    "lg-buddy"
                ))),
                true
            ),
            InstallationOwnership::Package {
                family: PackageFamily::Pacman
            }
        );
        // unknown: unowned but the layout is NOT a recognized bundle.
        assert_eq!(
            installation_ownership(Ok(PathOwnership::Unowned), false),
            InstallationOwnership::Unknown
        );
        // conflicting -> Unknown.
        assert_eq!(
            installation_ownership(
                Ok(PathOwnership::Conflicting(vec![owner(
                    PackageFamily::Dpkg,
                    "a"
                )])),
                true
            ),
            InstallationOwnership::Unknown
        );
        // probe failure -> Unknown (fail-closed).
        assert_eq!(
            installation_ownership(Err(PackageDatabaseError::ProbeFailed("no db".into())), true),
            InstallationOwnership::Unknown
        );
    }

    #[test]
    fn installation_ownership_is_known_predicate() {
        assert!(InstallationOwnership::Bundle.is_known());
        assert!(InstallationOwnership::Package {
            family: PackageFamily::Dpkg
        }
        .is_known());
        assert!(!InstallationOwnership::Unknown.is_known());
    }

    #[test]
    fn blocks_mutation_predicate() {
        assert!(!PathOwnership::Unowned.blocks_mutation());
        assert!(PathOwnership::Owned(owner(PackageFamily::Dpkg, "lg-buddy")).blocks_mutation());
        assert!(PathOwnership::Conflicting(vec![]).blocks_mutation());
    }
}
