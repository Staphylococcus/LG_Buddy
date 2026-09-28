// Path-safety judgment: whether a host path is safe for the upgrade to
// read, replace, or mutate, against a trust boundary — owned, ordinary
// directories only (no symlink / hard-link / mount-point / group-other-
// write tricks). The shared check engine used by both the installed-state
// and candidate-bundle evaluations. Moved verbatim from
// upgrade_preflight.rs; the coordinator and its integration tests
// construct `Checker` and drive it, so the methods they call are
// promoted to `pub(super)`.
use std::collections::BTreeSet;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::observation::{FilesystemFacts, PathFacts, PathKind, SystemdManagerObservation};
use super::{CompatibilityReport, InstallerPathPolicy};

pub(super) const MAX_CONFIG_TREE_ENTRIES: usize = 256;

#[derive(Debug, Clone, Copy)]
pub(super) struct TrustedRoot<'a> {
    path: &'a Path,
    owner_uid: u32,
    reject_other_writes: bool,
    protect_external_ancestors: bool,
}

impl<'a> TrustedRoot<'a> {
    pub(super) fn strict(path: &'a Path, owner_uid: u32) -> Self {
        Self {
            path,
            owner_uid,
            reject_other_writes: true,
            protect_external_ancestors: false,
        }
    }

    pub(super) fn candidate(path: &'a Path, owner_uid: u32) -> Self {
        Self {
            path,
            owner_uid,
            reject_other_writes: true,
            protect_external_ancestors: true,
        }
    }

    pub(super) fn owned(path: &'a Path, owner_uid: u32) -> Self {
        Self {
            path,
            owner_uid,
            reject_other_writes: false,
            protect_external_ancestors: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum AncestorPolicy {
    Trusted {
        owner_uid: u32,
        reject_other_writes: bool,
    },
    CandidateExternal {
        user_owner_uid: u32,
    },
}

pub(super) struct Checker<'a, F> {
    filesystem: &'a F,
    pub(super) report: CompatibilityReport,
    checked_ancestors: BTreeSet<(PathBuf, Option<AncestorPolicy>)>,
}

impl<'a, F: FilesystemFacts> Checker<'a, F> {
    pub(super) fn new(filesystem: &'a F) -> Self {
        Self {
            filesystem,
            report: CompatibilityReport::default(),
            checked_ancestors: BTreeSet::new(),
        }
    }

    pub(super) fn check_requirement(
        &mut self,
        path: &Path,
        owner_uid: u32,
        trusted_root: Option<TrustedRoot<'_>>,
        policy: InstallerPathPolicy,
        check: &'static str,
    ) {
        self.check_ancestors(path, trusted_root);
        let facts = match self.filesystem.path_facts(path) {
            Ok(facts) => facts,
            Err(err)
                if policy == InstallerPathPolicy::RecursiveClear
                    && err.kind() == io::ErrorKind::NotFound =>
            {
                return;
            }
            Err(err) => {
                self.report.refuse(
                    check,
                    Some(path.to_path_buf()),
                    if err.kind() == io::ErrorKind::NotFound {
                        "required path is missing".to_string()
                    } else {
                        format!("could not inspect required path: {err}")
                    },
                    "restore this path from a current release-bundle installation",
                );
                return;
            }
        };
        if policy.expects_file() && facts.kind != PathKind::File {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("expected a regular file, found {:?}", facts.kind),
                "restore this path from a current release-bundle installation",
            );
            return;
        }
        if policy.expects_directory() && facts.kind != PathKind::Directory {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("expected a directory, found {:?}", facts.kind),
                "restore this directory as part of a current release-bundle installation",
            );
            return;
        }
        self.check_owner(path, &facts, owner_uid, check);
        if trusted_root.is_some_and(|root| root.reject_other_writes) {
            self.check_not_writable_by_others(path, &facts);
        }

        match policy {
            InstallerPathPolicy::ReplaceFile => self.check_replace_file(path, &facts, check),
            InstallerPathPolicy::ReplaceExecutable => {
                self.check_replace_file(path, &facts, check);
                self.check_permissions(
                    path,
                    &facts,
                    0o111,
                    check,
                    "installed executable has no execute permission",
                );
            }
            InstallerPathPolicy::MutateDirectory => {
                self.check_mutable_directory(path, &facts, owner_uid, check, 0o300);
            }
            InstallerPathPolicy::RecursiveClear => {
                self.check_mutable_directory(path, &facts, owner_uid, check, 0o300);
                self.check_recursive_clear_mounts(path, check);
            }
            InstallerPathPolicy::ExactDropInDirectory { expected_entry } => {
                self.check_mutable_directory(path, &facts, owner_uid, check, 0o700);
                self.check_exact_directory(path, expected_entry, check);
            }
            InstallerPathPolicy::ReadableInput => self.check_permissions(
                path,
                &facts,
                0o400,
                check,
                "input is not readable by its owner",
            ),
            InstallerPathPolicy::SystemReadableInput => self.check_permissions(
                path,
                &facts,
                0o404,
                check,
                "system input is not readable by its owner and the invoking user",
            ),
            InstallerPathPolicy::ExecutableInput => self.check_permissions(
                path,
                &facts,
                0o500,
                check,
                "input is not readable and executable by its owner",
            ),
            InstallerPathPolicy::InputDirectory => self.check_permissions(
                path,
                &facts,
                0o500,
                check,
                "input directory is not readable and searchable by its owner",
            ),
        }
    }

    pub(super) fn check_optional_requirement(
        &mut self,
        path: &Path,
        owner_uid: u32,
        trusted_root: Option<TrustedRoot<'_>>,
        policy: InstallerPathPolicy,
        check: &'static str,
    ) {
        match self.filesystem.path_facts(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            _ => self.check_requirement(path, owner_uid, trusted_root, policy, check),
        }
    }

    pub(super) fn check_install_destination(
        &mut self,
        path: &Path,
        owner_uid: u32,
        trusted_root: Option<TrustedRoot<'_>>,
        policy: InstallerPathPolicy,
        check: &'static str,
    ) {
        match self.filesystem.path_facts(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // install -d can create missing ancestors. Validate the nearest
                // existing one, including a dangling symlink, before trusting it.
                for ancestor in path.ancestors().skip(1) {
                    if matches!(self.filesystem.path_facts(ancestor), Err(err) if err.kind() == io::ErrorKind::NotFound)
                    {
                        continue;
                    }
                    self.check_requirement(
                        ancestor,
                        owner_uid,
                        trusted_root,
                        InstallerPathPolicy::MutateDirectory,
                        check,
                    );
                    break;
                }
            }
            _ => self.check_requirement(path, owner_uid, trusted_root, policy, check),
        }
    }

    pub(super) fn check_replace_file_alternatives(
        &mut self,
        paths: &[PathBuf],
        owner_uid: u32,
        trusted_root: Option<TrustedRoot<'_>>,
        check: &'static str,
    ) {
        let mut found = false;
        for path in paths {
            match self.filesystem.path_facts(path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                _ => {
                    found = true;
                    self.check_requirement(
                        path,
                        owner_uid,
                        trusted_root,
                        InstallerPathPolicy::ReplaceFile,
                        check,
                    );
                }
            }
        }
        if !found {
            self.report.refuse(
                check,
                paths.first().cloned(),
                "no supported LG Buddy desktop entry is installed",
                "restore the desktop entry from a current release-bundle installation",
            );
        }
    }

    fn check_replace_file(&mut self, path: &Path, facts: &PathFacts, check: &'static str) {
        if facts.read_only_filesystem {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "file is on a read-only filesystem",
                "use the host's native package manager or make this installation file mutable",
            );
        }
        if facts.link_count != 1 {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!(
                    "file has {} hard links, expected exactly one",
                    facts.link_count
                ),
                "replace the path with an independent regular file before upgrading",
            );
        }
        if facts.mount_point {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "file is a mount point",
                "replace the mounted path with an ordinary installation file before upgrading",
            );
        }
        self.check_permissions(
            path,
            facts,
            0o200,
            check,
            "file is not writable by its owner",
        );
    }

    fn check_mutable_directory(
        &mut self,
        path: &Path,
        facts: &PathFacts,
        owner_uid: u32,
        check: &'static str,
        required_permissions: u32,
    ) {
        if facts.read_only_filesystem {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "directory is on a read-only filesystem",
                "use the host's native package manager or make the installed release-bundle paths mutable",
            );
        }
        if facts.mount_point {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                "directory is a mount point",
                "replace the mounted path with an ordinary installation directory before upgrading",
            );
        }
        let required_permissions = if owner_uid == 0 && facts.owner_uid == 0 {
            required_permissions & !0o200
        } else {
            required_permissions
        };
        self.check_permissions(
            path,
            facts,
            required_permissions,
            check,
            match required_permissions {
                0o100 => "directory is not searchable by its owner",
                0o500 => "directory is not readable and searchable by its owner",
                0o700 => "directory is not readable, writable, and searchable by its owner",
                _ => "directory is not writable and searchable by its owner",
            },
        );
    }

    fn check_recursive_clear_mounts(&mut self, path: &Path, check: &'static str) {
        match self.filesystem.mount_points() {
            Ok(mount_points) => {
                for mount_point in mount_points {
                    if mount_point != path && mount_point.starts_with(path) {
                        self.report.refuse(
                            check,
                            Some(mount_point),
                            "recursively cleared directory contains a nested mount point",
                            "unmount nested filesystems from the managed virtualenv before upgrading",
                        );
                    }
                }
            }
            Err(err) => self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("could not inspect nested mount points: {err}"),
                "make mount information available before upgrading",
            ),
        }
    }

    fn check_exact_directory(&mut self, path: &Path, expected_entry: &str, check: &'static str) {
        let expected_path = path.join(expected_entry);
        match self.filesystem.read_directory(path) {
            Ok(entries) => {
                if !entries.iter().any(|entry| entry == &expected_path) {
                    self.report.refuse(
                        check,
                        Some(expected_path.clone()),
                        "required drop-in entry is missing",
                        "restore the exact drop-in directory from a current release bundle",
                    );
                }
                for entry in entries {
                    if entry != expected_path {
                        self.report.refuse(
                            check,
                            Some(entry),
                            "drop-in directory contains an unexpected entry",
                            "remove unexpected drop-ins before upgrading",
                        );
                    }
                }
            }
            Err(err) => self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!("could not inspect drop-in directory: {err}"),
                "make the drop-in directory readable before upgrading",
            ),
        }
    }

    fn check_permissions(
        &mut self,
        path: &Path,
        facts: &PathFacts,
        required: u32,
        check: &'static str,
        detail: &'static str,
    ) {
        if facts.mode & required != required {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                detail,
                "restore the path permissions from a current release bundle",
            );
        }
    }

    pub(super) fn check_integration_override(&mut self, path: &Path, expected: &str) {
        if self
            .report
            .failures
            .iter()
            .any(|failure| failure.path.as_deref() == Some(path))
        {
            return;
        }
        match self.filesystem.read_to_string(path) {
            Ok(contents) => {
                let directives: Vec<_> = contents
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.starts_with('#') && !line.starts_with(';'))
                    .filter(|line| line.contains("LG_BUDDY_CONFIG"))
                    .collect();
                if directives.len() != 1 || directives[0] != expected {
                    self.report.refuse(
                        "integration-config",
                        Some(path.to_path_buf()),
                        format!("integration does not reference exactly {expected}"),
                        "restore the sole integration override for the discovered config path",
                    );
                }
            }
            Err(err) => self.report.refuse(
                "integration-config",
                Some(path.to_path_buf()),
                format!("could not read the integration override: {err}"),
                "restore a readable integration override",
            ),
        }
    }

    pub(super) fn check_absent(&mut self, path: &Path) {
        match self.filesystem.path_facts(path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => self.report.refuse(
                "legacy-layout",
                Some(path.to_path_buf()),
                format!("could not determine whether a legacy path exists: {err}"),
                "inspect and remove the legacy integration before upgrading",
            ),
            Ok(_) => self.report.refuse(
                "legacy-layout",
                Some(path.to_path_buf()),
                "legacy installation state is present",
                "reinstall the current release bundle cleanly; the updater does not migrate legacy layouts",
            ),
        }
    }

    pub(super) fn read_config_pointer(
        &mut self,
        path: &Path,
        owner_uid: u32,
        system_root: &Path,
    ) -> Option<PathBuf> {
        self.check_requirement(
            path,
            owner_uid,
            Some(TrustedRoot::strict(system_root, owner_uid)),
            InstallerPathPolicy::SystemReadableInput,
            "config-discovery",
        );
        if self
            .report
            .failures
            .iter()
            .any(|failure| failure.path.as_deref() == Some(path))
        {
            return None;
        }
        let contents = match self.filesystem.read_to_string(path) {
            Ok(contents) => contents,
            Err(err) => {
                self.report.refuse(
                    "config-discovery",
                    Some(path.to_path_buf()),
                    format!("could not read the installed config pointer: {err}"),
                    "restore the config pointer from a current release-bundle installation",
                );
                return None;
            }
        };
        let lines: Vec<_> = contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.len() != 1 {
            self.report.refuse(
                "config-discovery",
                Some(path.to_path_buf()),
                "config pointer must contain exactly one non-empty path",
                "rewrite the pointer with the absolute path to config.env",
            );
            return None;
        }
        let config_path = PathBuf::from(lines[0]);
        if !check_normalized_absolute(&mut self.report, "config-discovery", &config_path) {
            return None;
        }
        Some(config_path)
    }

    pub(super) fn check_config_tree(&mut self, config_path: &Path, owner_uid: u32) {
        let Some(config_directory) = config_path.parent() else {
            self.report.refuse(
                "config-state",
                Some(config_path.to_path_buf()),
                "config path has no parent directory",
                "place config.env in a user-owned configuration directory",
            );
            return;
        };
        let config_trust = TrustedRoot::owned(config_directory, owner_uid);
        self.check_requirement(
            config_directory,
            owner_uid,
            Some(config_trust),
            InstallerPathPolicy::InputDirectory,
            "config-state",
        );
        self.check_requirement(
            config_path,
            owner_uid,
            Some(config_trust),
            InstallerPathPolicy::ReadableInput,
            "config-state",
        );
        if self
            .report
            .failures
            .iter()
            .any(|failure| failure.path.as_deref() == Some(config_directory))
        {
            return;
        }

        let mut pending = vec![config_directory.to_path_buf()];
        let mut seen = 0;
        while let Some(directory) = pending.pop() {
            let entries = match self.filesystem.read_directory(&directory) {
                Ok(entries) => entries,
                Err(err) => {
                    self.report.refuse(
                        "config-state",
                        Some(directory),
                        format!("could not inspect the config directory: {err}"),
                        "make the LG Buddy config directory readable by the installed user",
                    );
                    return;
                }
            };
            for entry in entries {
                seen += 1;
                if seen > MAX_CONFIG_TREE_ENTRIES {
                    self.report.refuse(
                        "config-state",
                        Some(config_directory.to_path_buf()),
                        format!("config tree exceeds {MAX_CONFIG_TREE_ENTRIES} entries"),
                        "remove unrelated files from the LG Buddy config directory",
                    );
                    return;
                }
                let facts = match self.path_facts(&entry, "config-state") {
                    Some(facts) => facts,
                    None => continue,
                };
                self.check_owner(&entry, &facts, owner_uid, "config-state");
                match facts.kind {
                    PathKind::Directory => {
                        self.check_permissions(
                            &entry,
                            &facts,
                            0o500,
                            "config-state",
                            "config directory is not readable and searchable by its owner",
                        );
                        pending.push(entry);
                    }
                    PathKind::File => self.check_permissions(
                        &entry,
                        &facts,
                        0o400,
                        "config-state",
                        "config file is not readable by its owner",
                    ),
                    PathKind::Symlink | PathKind::Other => self.report.refuse(
                        "config-state",
                        Some(entry),
                        format!("config tree contains an unsafe {:?} entry", facts.kind),
                        "replace the entry with a user-owned regular file or directory",
                    ),
                }
            }
        }
    }

    pub(super) fn check_capability(
        &mut self,
        check: &'static str,
        observation: &SystemdManagerObservation,
        remedy: &'static str,
    ) {
        let Some(reason) = service_manager_refusal(observation) else {
            return;
        };
        self.report.refuse(check, None, reason, remedy.to_string());
    }

    fn check_not_writable_by_others(&mut self, path: &Path, facts: &PathFacts) {
        if facts.mode & 0o022 != 0 {
            self.report.refuse(
                "path-containment",
                Some(path.to_path_buf()),
                "trusted path is writable by its group or by other users",
                "remove group and other write permission from the trusted path",
            );
        }
    }

    fn check_candidate_external_ancestor(
        &mut self,
        path: &Path,
        facts: &PathFacts,
        user_owner_uid: u32,
    ) {
        if facts.owner_uid != 0 && facts.owner_uid != user_owner_uid {
            self.report.refuse(
                "path-containment",
                Some(path.to_path_buf()),
                format!(
                    "candidate path ancestor is owned by uid {}, expected root or uid {user_owner_uid}",
                    facts.owner_uid
                ),
                "move the verified bundle below a root- or user-owned directory",
            );
            return;
        }
        if facts.mode & 0o022 != 0 && facts.mode & 0o1000 == 0 {
            self.report.refuse(
                "path-containment",
                Some(path.to_path_buf()),
                "candidate path ancestor is writable by its group or by other users without sticky-directory protection",
                "move the verified bundle into a private directory or below a sticky shared directory such as /tmp",
            );
        }
    }

    fn check_ancestors(&mut self, path: &Path, trusted_root: Option<TrustedRoot<'_>>) {
        for ancestor in path.ancestors().skip(1) {
            let ancestor_policy = trusted_root.and_then(|root| {
                if ancestor.starts_with(root.path) {
                    Some(AncestorPolicy::Trusted {
                        owner_uid: root.owner_uid,
                        reject_other_writes: root.reject_other_writes,
                    })
                } else if root.protect_external_ancestors {
                    Some(AncestorPolicy::CandidateExternal {
                        user_owner_uid: root.owner_uid,
                    })
                } else {
                    None
                }
            });
            if !self
                .checked_ancestors
                .insert((ancestor.to_path_buf(), ancestor_policy))
            {
                continue;
            }
            match self.filesystem.path_facts(ancestor) {
                Ok(facts) if facts.kind == PathKind::Directory => match ancestor_policy {
                    Some(AncestorPolicy::Trusted {
                        owner_uid,
                        reject_other_writes,
                    }) => {
                        self.check_owner(ancestor, &facts, owner_uid, "path-containment");
                        if reject_other_writes {
                            self.check_not_writable_by_others(ancestor, &facts);
                        }
                    }
                    Some(AncestorPolicy::CandidateExternal { user_owner_uid }) => {
                        self.check_candidate_external_ancestor(ancestor, &facts, user_owner_uid)
                    }
                    None => {}
                },
                Ok(facts) => self.report.refuse(
                    "path-containment",
                    Some(ancestor.to_path_buf()),
                    format!("path ancestor is {:?}, not a real directory", facts.kind),
                    "replace symlinked or special ancestors with ordinary directories",
                ),
                Err(err) => self.report.refuse(
                    "path-containment",
                    Some(ancestor.to_path_buf()),
                    format!("could not inspect path ancestor: {err}"),
                    "restore the complete installation path",
                ),
            }
        }
    }

    fn path_facts(&mut self, path: &Path, check: &'static str) -> Option<PathFacts> {
        match self.filesystem.path_facts(path) {
            Ok(facts) => Some(facts),
            Err(err) => {
                self.report.refuse(
                    check,
                    Some(path.to_path_buf()),
                    if err.kind() == io::ErrorKind::NotFound {
                        "required path is missing".to_string()
                    } else {
                        format!("could not inspect required path: {err}")
                    },
                    "restore this path from a current release-bundle installation",
                );
                None
            }
        }
    }

    fn check_owner(
        &mut self,
        path: &Path,
        facts: &PathFacts,
        expected_uid: u32,
        check: &'static str,
    ) {
        if facts.owner_uid != expected_uid {
            self.report.refuse(
                check,
                Some(path.to_path_buf()),
                format!(
                    "path is owned by uid {}, expected uid {expected_uid}",
                    facts.owner_uid
                ),
                "restore the expected ownership before upgrading",
            );
        }
    }
}

pub(super) fn check_normalized_absolute(
    report: &mut CompatibilityReport,
    check: &'static str,
    path: &Path,
) -> bool {
    let has_dot_component = path
        .as_os_str()
        .as_bytes()
        .split(|byte| *byte == b'/')
        .any(|component| matches!(component, b"." | b".."));
    if !path.is_absolute() || has_dot_component {
        report.refuse(
            check,
            Some(path.to_path_buf()),
            "path is not normalized and absolute",
            "use an absolute path without '.' or '..' components",
        );
        false
    } else {
        true
    }
}

pub(super) fn systemd_config_override_line(config_path: &Path) -> String {
    let escaped = config_path
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("Environment=\"LG_BUDDY_CONFIG={escaped}\"")
}

/// Judgment: which systemd manager observations make an upgrade impossible.
/// `Skipped` is a pass by design: sandboxed execution cannot reach a system
/// manager, and the upgrade's mutation step will have sudo anyway.
pub(super) fn service_manager_refusal(observation: &SystemdManagerObservation) -> Option<String> {
    match observation {
        SystemdManagerObservation::Skipped => None,
        SystemdManagerObservation::ProbeFailed(reason) => Some(reason.clone()),
        SystemdManagerObservation::Reported { state, stderr } => {
            if matches!(state.as_str(), "running" | "degraded") {
                None
            } else if state.is_empty() {
                Some(format!(
                    "systemctl did not report a usable manager state ({stderr})"
                ))
            } else {
                Some(format!("systemd manager state is {state}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Deterministic filesystem: per-path `PathFacts` supplied by the caller,
    /// so none of the checks touch the real host layout.
    struct FakeFacts {
        entries: RefCell<HashMap<PathBuf, PathFacts>>,
        mounts: RefCell<Vec<PathBuf>>,
    }

    fn fake_facts(entries: &[(String, PathFacts)], mounts: &[&str]) -> FakeFacts {
        FakeFacts {
            entries: RefCell::new(
                entries
                    .iter()
                    .map(|(path, f)| (PathBuf::from(path), f.clone()))
                    .collect(),
            ),
            mounts: RefCell::new(mounts.iter().map(|m| PathBuf::from(*m)).collect()),
        }
    }

    fn facts(kind: PathKind, uid: u32, mode: u32) -> PathFacts {
        PathFacts {
            kind,
            owner_uid: uid,
            mode,
            link_count: 1,
            read_only_filesystem: false,
            mount_point: false,
        }
    }

    impl FilesystemFacts for FakeFacts {
        fn path_facts(&self, path: &Path) -> io::Result<PathFacts> {
            self.entries.borrow().get(path).cloned().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("missing {}", path.display()),
                )
            })
        }
        fn read_to_string(&self, _path: &Path) -> io::Result<String> {
            Ok(String::new())
        }
        fn read_directory(&self, _path: &Path) -> io::Result<Vec<PathBuf>> {
            Ok(Vec::new())
        }
        fn mount_points(&self) -> io::Result<Vec<PathBuf>> {
            Ok(self.mounts.borrow().clone())
        }
    }

    fn entry(path: &str, kind: PathKind, uid: u32, mode: u32) -> (String, PathFacts) {
        (path.to_string(), facts(kind, uid, mode))
    }

    fn detail<'a>(checker: &'a Checker<'a, FakeFacts>, check: &str) -> Option<&'a str> {
        checker
            .report
            .failures()
            .iter()
            .find(|failure| failure.check == check)
            .map(|failure| failure.detail.as_str())
    }

    #[test]
    fn normalized_absolute_rejects_relative_and_dot_components() {
        for rejected in ["relative", "a/../b", "a/./b"] {
            let mut report = CompatibilityReport::default();
            assert!(!check_normalized_absolute(
                &mut report,
                "c",
                Path::new(rejected)
            ));
        }
        let mut report = CompatibilityReport::default();
        assert!(check_normalized_absolute(
            &mut report,
            "c",
            Path::new("/usr/bin/lg-buddy")
        ));
        assert!(report.compatible(), "{report}");
    }

    #[test]
    fn owner_mismatch_refuses_with_expected_uid() {
        let fs = fake_facts(
            &[
                entry("/", PathKind::Directory, 0, 0o755),
                entry("/a", PathKind::File, 500, 0o644),
            ],
            &[],
        );
        let mut checker = Checker::new(&fs);
        checker.check_requirement(
            Path::new("/a"),
            0,
            None,
            InstallerPathPolicy::ReadableInput,
            "owner-check",
        );
        assert!(detail(&checker, "owner-check")
            .unwrap()
            .contains("expected uid 0"));
    }

    #[test]
    fn permission_masks_are_enforced_per_policy() {
        // ReplaceExecutable requires owner write (0o200) and exec for all
        // (0o111); 0o755 satisfies both.
        let fs = fake_facts(
            &[
                entry("/", PathKind::Directory, 0, 0o755),
                entry("/bin", PathKind::Directory, 0, 0o755),
                entry("/bin/lg-buddy", PathKind::File, 0, 0o755),
            ],
            &[],
        );
        let mut checker = Checker::new(&fs);
        checker.check_requirement(
            Path::new("/bin/lg-buddy"),
            0,
            None,
            InstallerPathPolicy::ReplaceExecutable,
            "exec-check",
        );
        assert!(checker.report.compatible(), "{}", checker.report.render());

        // SystemReadableInput requires owner+group+other read (0o404); 0o600 fails.
        let fs = fake_facts(
            &[
                entry("/", PathKind::Directory, 0, 0o755),
                entry("/config", PathKind::Directory, 500, 0o755),
                entry("/config.env", PathKind::File, 500, 0o600),
            ],
            &[],
        );
        let mut checker = Checker::new(&fs);
        checker.check_requirement(
            Path::new("/config.env"),
            500,
            None,
            InstallerPathPolicy::SystemReadableInput,
            "perm-check",
        );
        assert!(!checker.report.compatible());
    }

    #[test]
    fn group_or_other_writable_paths_are_refused_in_trusted_roots() {
        let root = TrustedRoot::strict(Path::new("/opt/lg-buddy"), 0);
        let fs = fake_facts(
            &[
                entry("/", PathKind::Directory, 0, 0o755),
                entry("/opt", PathKind::Directory, 0, 0o755),
                entry("/opt/lg-buddy", PathKind::Directory, 0, 0o755),
                entry("/opt/lg-buddy/bin", PathKind::File, 0, 0o775),
            ],
            &[],
        );
        let mut checker = Checker::new(&fs);
        checker.check_install_destination(
            Path::new("/opt/lg-buddy/bin"),
            0,
            Some(root),
            InstallerPathPolicy::ReplaceFile,
            "other-write",
        );
        assert!(!checker.report.compatible(), "{}", checker.report.render());
    }

    #[test]
    fn symlinked_path_is_refused_as_a_file_replacement_target() {
        let fs = fake_facts(
            &[
                entry("/", PathKind::Directory, 0, 0o755),
                entry("/a", PathKind::Symlink, 0, 0o777),
            ],
            &[],
        );
        let mut checker = Checker::new(&fs);
        checker.check_requirement(
            Path::new("/a"),
            0,
            None,
            InstallerPathPolicy::ReplaceFile,
            "symlink-check",
        );
        assert!(!checker.report.compatible());
    }

    #[test]
    fn mount_point_inside_recursive_clear_refuses() {
        let fs = fake_facts(
            &[
                entry("/", PathKind::Directory, 0, 0o755),
                entry("/x", PathKind::Directory, 0, 0o755),
                entry("/x/data", PathKind::Directory, 0, 0o700),
            ],
            &["/x/data/mounted"],
        );
        let mut checker = Checker::new(&fs);
        checker.check_requirement(
            Path::new("/x/data"),
            0,
            None,
            InstallerPathPolicy::RecursiveClear,
            "mount-check",
        );
        assert!(detail(&checker, "mount-check").unwrap().contains("mount"));
    }

    #[test]
    fn missing_required_path_refuses() {
        let fs = fake_facts(&[], &[]);
        let mut checker = Checker::new(&fs);
        checker.check_requirement(
            Path::new("/missing"),
            0,
            None,
            InstallerPathPolicy::ReadableInput,
            "absent-check",
        );
        assert!(detail(&checker, "absent-check")
            .unwrap()
            .contains("missing"));
    }
}
