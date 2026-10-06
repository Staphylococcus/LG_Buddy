// Package-ownership probe (#133 / #301).
//
// Answers "who owns an installed file, if anyone" so the clobber guard (#304),
// the provisioning changes (#302), and the updater can tell a package-managed
// install from a plain release-bundle install. This module is pure
// *observation*: it records what the host package database says. Deciding how
// to *refuse* a mutation of a package-owned path is the guard's job, not
// this module's — mirroring the preflight split between observation and
// judgment.
//
// The probe is fail-closed by construction: `owner_of` returns
// `Result<PathOwnership, PackageDatabaseError>`, never `Option`. A missing
// database, an unparseable result, or a tool failure is an `Err` — it is NOT
// "definitely unowned". The clobber guard treats `Err` and `Conflicting` as
// "cannot safely proceed", so a probe that cannot run can never be mistaken
// for a clean slate.
//
// #301 ships the `dpkg` probe only. The `Rpm` family is a forward
// placeholder: #135 adds the `rpm -qf` probe *path* that populates it — a
// probe-shape extension, not data alone.
//
// Two questions, deliberately kept separate. The *general* path-level answer
// (arbitrary path) does not reveal the *installation-level* answer (is the
// whole install package-managed?); that is answered by probing the installed
// *executable* and gating the result on the installed layout — see
// `installation_ownership`. "Unowned" alone is NOT `Bundle`: an install is
// a bundle only when the executable is unowned *and* the installed layout
// satisfies the release-bundle contract.

use std::path::Path;
use std::process::Command;

/// Which package-manager family an owner was reported by. Family is *data* on
/// the owner, not a dispatch: consumers (and the future package-version
/// serialization in #306/#134/#135) match on the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFamily {
    /// A `dpkg`-managed install (Debian/Ubuntu).
    Dpkg,
    /// An `rpm`-managed install (Fedora/RHEL-family). Forward data at #301:
    /// the `rpm` probe path is added in #135, not here.
    Rpm,
}

impl PackageFamily {
    /// Stable, family-specific identity string. Consumed by the future
    /// package-version serialization (#306); not a full version encoding here.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dpkg => "dpkg",
            Self::Rpm => "rpm",
        }
    }
}

/// A single package that owns a path, with the family that reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageOwner {
    pub family: PackageFamily,
    /// The owning package name (e.g. `lg-buddy`, `LG_Buddy`).
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
    /// `Conflicting` blocks mutation of the path. (`Err` blocks too, but that
    /// is handled by the `Result`, not this value.)
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

/// The raw result of asking one package database about a path. Kept separate
/// from `PathOwnership` so the *judgment* (exit-code semantics) stays testable
/// and the *observation* (what the tool printed) stays swappable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DpkgProbeOutcome {
    /// The `dpkg` tool ran. `code` is its exit status, `lines` its stdout —
    /// ownership records (`pkg1, pkg2: /path`, comma-separated owner list)
    /// and optional diversion records for the matched files.
    Ran { code: i32, lines: Vec<String> },
    /// The `dpkg` tool could not be run at all (absent or exec failure).
    Absent,
}

/// A probe over one package database. At #301 the only database is `dpkg`;
/// the probe runner is injectable so tests (and #135's later `rpm` path) can
/// supply a fixture without a packaged binary.
pub struct PackageDatabase {
    probe: Box<dyn Fn(&Path) -> DpkgProbeOutcome + Send + Sync>,
}

impl std::fmt::Debug for PackageDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The runner is a closure; name only the type so Debug stays total.
        f.debug_struct("PackageDatabase").finish_non_exhaustive()
    }
}

impl PackageDatabase {
    /// The default probe: a real `dpkg -S` against the host's database.
    ///
    /// `LC_ALL=C.UTF-8` (and clearing `LANGUAGE`) pins the output to the
    /// documented, reproducible C-locale form — the man page recommends this
    /// when machine-parsing, because diversion records are otherwise printed
    /// with *localized* prefixes.
    pub fn system() -> Self {
        Self::with_probe(|path| {
            let output = Command::new("dpkg")
                .arg("-S")
                .arg(path)
                .env("LC_ALL", "C.UTF-8")
                .env_remove("LANGUAGE")
                .output();
            match output {
                Ok(output) => DpkgProbeOutcome::Ran {
                    code: output.status.code().unwrap_or(-1),
                    lines: String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .map(str::to_string)
                        .collect(),
                },
                Err(_) => DpkgProbeOutcome::Absent,
            }
        })
    }

    /// A probe backed by an injectable `dpkg` runner (a test fixture). The
    /// runner returns the *raw* tool outcome; `owner_of` applies the
    /// exit-code semantics. #135's `rpm` path is a new runner shape (its
    /// outcome type, exit codes, and output grammar differ), not a fixture.
    pub fn with_probe(runner: impl Fn(&Path) -> DpkgProbeOutcome + Send + Sync + 'static) -> Self {
        Self {
            probe: Box::new(runner),
        }
    }

    /// The *path-level* ownership question: which package, if any, owns
    /// `path`? Never returns `None`/ambiguous — a database that cannot answer
    /// is an `Err`, which the guard treats as "cannot proceed".
    pub fn owner_of(&self, path: &Path) -> Result<PathOwnership, PackageDatabaseError> {
        match (self.probe)(path) {
            // `dpkg -S` exit 0: the matched files are owned. A successful run
            // with output we cannot parse is *not* "unowned" — it fails
            // closed, matching the contract above.
            DpkgProbeOutcome::Ran { code: 0, lines } => {
                parse_dpkg_owners(lines.iter().map(String::as_str), path)
                    .map_err(|reason| PackageDatabaseError::ProbeFailed(reason))
            }
            // `dpkg -S` exit 1: "no path found matching pattern" — a
            // *definitive* "no dpkg package owns this" answer, i.e. unowned by
            // the dpkg family. This is the app-owned / bundle case.
            DpkgProbeOutcome::Ran { code: 1, .. } => Ok(PathOwnership::Unowned),
            // Any other exit is a database/tool error we must NOT read as
            // "unowned" — fail closed.
            DpkgProbeOutcome::Ran { code, .. } => Err(PackageDatabaseError::ProbeFailed(format!(
                "dpkg -S exited {code} for {}",
                path.display()
            ))),
            // The tool could not be run: fail closed, never assume unowned.
            DpkgProbeOutcome::Absent => Err(PackageDatabaseError::ProbeFailed(format!(
                "dpkg unavailable for {}",
                path.display()
            ))),
        }
    }
}

/// Map an *installation-level* probe result to `InstallationOwnership`.
///
/// Fail-closed, and `Bundle` is *layout-gated*: an unowned executable is a
/// bundle only when the installed layout also satisfies the release-bundle
/// installation contract (`bundle_layout`, computed from the preflight
/// installed-layout observations). An unowned executable in a layout that is
/// NOT a recognizable bundle is `Unknown`, not a blessed `Bundle` — arbitrary
/// or manual installs are not to be mistaken for a bundle the guard can act
/// on. A conflicting report or any probe failure is also `Unknown`.
pub fn installation_ownership(
    executable: Result<PathOwnership, PackageDatabaseError>,
    bundle_layout: bool,
) -> InstallationOwnership {
    match executable {
        // A named owner is a package-managed install under that family.
        Ok(PathOwnership::Owned(owner)) => InstallationOwnership::Package {
            family: owner.family,
        },
        // Unowned is a bundle only under a valid bundle layout; otherwise the
        // install is not one we recognize, so refuse to bless it.
        Ok(PathOwnership::Unowned) => {
            if bundle_layout {
                InstallationOwnership::Bundle
            } else {
                InstallationOwnership::Unknown
            }
        }
        // Conflicting ownership, or a probe that could not answer: fail closed.
        Ok(PathOwnership::Conflicting(_)) | Err(_) => InstallationOwnership::Unknown,
    }
}

/// The *installation-level* ownership answer. `Bundle` (a plain release-bundle
/// install, unowned) and `Package` (managed by a named family) are the two
/// usable states; `Unknown` is fail-closed — the guard refuses to act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallationOwnership {
    /// A plain release-bundle install: the executable is unowned *and* the
    /// installed layout satisfies the release-bundle contract.
    Bundle,
    /// A package-managed install, under the named family.
    Package { family: PackageFamily },
    /// The probe could not produce a definitive answer, or the layout is not a
    /// recognized bundle. Fail-closed: consumers refuse.
    Unknown,
}

impl InstallationOwnership {
    /// Fail-closed predicate: only a confirmed `Bundle` or `Package` is safe to
    /// act on; `Unknown` blocks.
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// Parse `dpkg -S <path>` stdout (C-locale form, as pinned by the probe) into
/// a `PathOwnership`. The documented output is one ownership record —
/// `pkgname1, pkgname2: pathname`, owning packages comma-separated — plus
/// zero or more diversion records. Any other line, a blank line, or an
/// ownership record naming a different path is an error: a successful run
/// with output we cannot read must fail closed, never read as `Unowned`
/// (which only exit code 1 may report).
pub fn parse_dpkg_owners<'a>(
    lines: impl Iterator<Item = &'a str>,
    path: &Path,
) -> Result<PathOwnership, String> {
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
        // Ownership record: `pkg1, pkg2: pathname`. The owner list cannot
        // contain ':', so the first ": " delimits it.
        let (list, record_path) = line
            .split_once(": ")
            .ok_or_else(|| format!("unparseable dpkg -S record: {line}"))?;
        // The record must be the path we queried; a different one means dpkg
        // did not answer our question — not something to accept.
        if record_path != expected {
            return Err(format!(
                "dpkg -S reported {record_path} for query {}",
                path.display()
            ));
        }
        // Owning packages are separated by ", " (comma + space).
        for owner in list.split(", ") {
            let owner = owner.trim();
            if !is_package_name(owner) {
                return Err(format!("invalid package name in dpkg -S record: {owner}"));
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
    Ok(ownership_from_owners(
        owners
            .into_iter()
            .map(|name| PackageOwner {
                family: PackageFamily::Dpkg,
                name,
                version: None,
            })
            .collect(),
    ))
}

/// A dpkg package name: starts with an ASCII alphanumeric, then ASCII
/// alphanumerics plus `+`, `.`, `-`. Anything else is not a package name, so
/// a line that pretends to be an ownership record is rejected.
fn is_package_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// Combine a list of owners into a `PathOwnership`. Empty -> `Unowned`; one ->
/// `Owned`; two or more -> `Conflicting`.
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

    fn dpkg_owner(name: &str) -> PackageOwner {
        PackageOwner {
            family: PackageFamily::Dpkg,
            name: name.to_string(),
            version: None,
        }
    }

    fn fixture(outcome: DpkgProbeOutcome) -> PackageDatabase {
        PackageDatabase::with_probe(move |_| outcome.clone())
    }

    #[test]
    fn parse_dpkg_single_owner() {
        let ownership = parse_dpkg_owners(
            ["lg-buddy: /usr/bin/lg-buddy"].iter().copied(),
            Path::new("/usr/bin/lg-buddy"),
        )
        .unwrap();
        assert_eq!(ownership, PathOwnership::Owned(dpkg_owner("lg-buddy")));
    }

    #[test]
    fn parse_dpkg_comma_separated_multiowner_is_conflicting() {
        // Documented format: `pkg1, pkg2: pathname` — one record carrying
        // several owners, NOT one line per package.
        let ownership = parse_dpkg_owners(
            ["alpha, beta: /usr/bin/x"].iter().copied(),
            Path::new("/usr/bin/x"),
        )
        .unwrap();
        let PathOwnership::Conflicting(owners) = &ownership else {
            panic!("expected conflicting");
        };
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
        let ownership =
            parse_dpkg_owners(lines.iter().copied(), Path::new("/usr/bin/lg-buddy")).unwrap();
        assert_eq!(ownership, PathOwnership::Owned(dpkg_owner("lg-buddy")));
    }

    #[test]
    fn parse_dpkg_deduplicates_repeated_package() {
        // A package appearing on two records is still a single owner.
        let ownership = parse_dpkg_owners(
            ["lg-buddy: /usr/bin/x", "lg-buddy: /usr/bin/x"]
                .iter()
                .copied(),
            Path::new("/usr/bin/x"),
        )
        .unwrap();
        assert!(matches!(ownership, PathOwnership::Owned(_)));
    }

    #[test]
    fn parse_dpkg_unexpected_output_fails_closed() {
        // A successful run with no parsable record is NOT "unowned" — that is
        // exit 1's exclusive job; this must error.
        assert!(parse_dpkg_owners([].iter().copied(), Path::new("/x")).is_err());
        let garbage = ["not a record", "no colon here"];
        assert!(parse_dpkg_owners(garbage.iter().copied(), Path::new("/x")).is_err());
        // A record naming a different path did not answer our query.
        let wrong = ["lg-buddy: /usr/bin/other"];
        assert!(parse_dpkg_owners(wrong.iter().copied(), Path::new("/usr/bin/x")).is_err());
    }

    #[test]
    fn owner_of_exit0_unparseable_fails_closed() {
        // Blocker: successful-but-malformed output must not fail open to
        // Unowned.
        let db = fixture(DpkgProbeOutcome::Ran {
            code: 0,
            lines: vec!["".into()],
        });
        assert!(matches!(
            db.owner_of(Path::new("/usr/bin/lg-buddy")),
            Err(PackageDatabaseError::ProbeFailed(_))
        ));
    }

    #[test]
    fn owner_of_owned() {
        let db = fixture(DpkgProbeOutcome::Ran {
            code: 0,
            lines: vec!["lg-buddy: /usr/bin/lg-buddy".into()],
        });
        let result = db.owner_of(Path::new("/usr/bin/lg-buddy")).unwrap();
        assert_eq!(result, PathOwnership::Owned(dpkg_owner("lg-buddy")));
        assert!(result.blocks_mutation());
    }

    #[test]
    fn owner_of_exit1_is_definitive_unowned() {
        // "no path found matching pattern" is a clean Unowned, not an error.
        let db = fixture(DpkgProbeOutcome::Ran {
            code: 1,
            lines: Vec::new(),
        });
        let result = db.owner_of(Path::new("/usr/bin/lg-buddy")).unwrap();
        assert_eq!(result, PathOwnership::Unowned);
        assert!(!result.blocks_mutation());
    }

    #[test]
    fn owner_of_conflicting() {
        let db = fixture(DpkgProbeOutcome::Ran {
            code: 0,
            lines: vec!["alpha, beta: /usr/bin/x".into()],
        });
        let result = db.owner_of(Path::new("/usr/bin/x")).unwrap();
        assert!(matches!(result, PathOwnership::Conflicting(_)));
        assert!(result.blocks_mutation());
    }

    #[test]
    fn owner_of_db_error_is_fail_closed() {
        // A non-zero, non-"unowned" exit is a db error: Err, never Unowned.
        let db = fixture(DpkgProbeOutcome::Ran {
            code: 2,
            lines: Vec::new(),
        });
        assert!(matches!(
            db.owner_of(Path::new("/usr/bin/lg-buddy")),
            Err(PackageDatabaseError::ProbeFailed(_))
        ));
    }

    #[test]
    fn owner_of_absent_tool_is_fail_closed() {
        // The tool cannot run: Err, never a silent Unowned.
        let db = fixture(DpkgProbeOutcome::Absent);
        assert!(matches!(
            db.owner_of(Path::new("/usr/bin/lg-buddy")),
            Err(PackageDatabaseError::ProbeFailed(_))
        ));
    }

    #[test]
    fn installation_ownership_asserts_all_cases() {
        // bundle: unowned executable + valid bundle layout.
        assert_eq!(
            installation_ownership(Ok(PathOwnership::Unowned), true),
            InstallationOwnership::Bundle
        );
        // dpkg package: a named owner.
        assert_eq!(
            installation_ownership(Ok(PathOwnership::Owned(dpkg_owner("lg-buddy"))), true),
            InstallationOwnership::Package {
                family: PackageFamily::Dpkg
            }
        );
        // rpm package: a named rpm owner (forward data; #135 populates it).
        let rpm_owner = PackageOwner {
            family: PackageFamily::Rpm,
            name: "LG_Buddy".into(),
            version: Some("1.10.0".into()),
        };
        assert_eq!(
            installation_ownership(Ok(PathOwnership::Owned(rpm_owner)), true),
            InstallationOwnership::Package {
                family: PackageFamily::Rpm
            }
        );
        // unknown: unowned but the layout is NOT a recognized bundle.
        assert_eq!(
            installation_ownership(Ok(PathOwnership::Unowned), false),
            InstallationOwnership::Unknown
        );
        // conflicting -> Unknown.
        assert_eq!(
            installation_ownership(Ok(PathOwnership::Conflicting(vec![dpkg_owner("a")])), true),
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
        assert!(PathOwnership::Owned(dpkg_owner("lg-buddy")).blocks_mutation());
        assert!(PathOwnership::Conflicting(vec![]).blocks_mutation());
    }
}
