//! Orchestration of the preflight check sequence.
//!
//! This is the decision layer: which paths to check, in what order, under
//! which trust root, and which optional checks get promoted. It owns the
//! ordering and the policy→check-name mapping. The *judgment* of a single
//! path (is this safe?) lives in `path_safety::Checker`; this module only
//! decides the sequence and feeds it the right inputs.
//!
//! It returns raw `Vec<CompatibilityFailure>` — the report *view* is built
//! downstream by the caller, so this layer has no dependency on the
//! presentation type.

use std::path::Path;

use super::observation::{FilesystemFacts, HostPreflightFacts};
use super::path_safety::{check_normalized_absolute, systemd_config_override_line, Checker, TrustedRoot};
use super::trust_placement::trust_placement;
use super::{CompatibilityFailure, InstallerPathPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InstallerPathRequirement {
    pub(super) path: &'static str,
    pub(super) policy: InstallerPathPolicy,
}

const fn requirement(path: &'static str, policy: InstallerPathPolicy) -> InstallerPathRequirement {
    InstallerPathRequirement { path, policy }
}

const SYSTEM_DESKTOP_ENTRY_PATHS: &[&str] = &[
    "/usr/share/applications/io.github.staphylococcus.LGBuddy.desktop",
    "/usr/share/applications/LG_Buddy_Brightness.desktop",
];

const USER_DESKTOP_ENTRY_PATHS: &[&str] = &[
    "Desktop/io.github.staphylococcus.LGBuddy.desktop",
    "Desktop/LG_Buddy_Brightness.desktop",
];

const SYSTEM_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[
    requirement("/usr/bin/lg-buddy", InstallerPathPolicy::ReplaceExecutable),
    requirement(
        "/etc/systemd/system/LG_Buddy.service",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/systemd/system/LG_Buddy.service.d/config.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/systemd/system/LG_Buddy_lifecycle.service",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/tmpfiles.d/lg_buddy.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle",
        InstallerPathPolicy::ReplaceExecutable,
    ),
    requirement("/usr/bin", InstallerPathPolicy::MutateDirectory),
    requirement("/etc/systemd/system", InstallerPathPolicy::MutateDirectory),
    requirement(
        "/etc/systemd/system/LG_Buddy.service.d",
        InstallerPathPolicy::ExactDropInDirectory {
            expected_entry: "config.conf",
        },
    ),
    requirement(
        "/etc/systemd/system/LG_Buddy_lifecycle.service.d",
        InstallerPathPolicy::ExactDropInDirectory {
            expected_entry: "config.conf",
        },
    ),
    requirement("/etc/tmpfiles.d", InstallerPathPolicy::MutateDirectory),
    requirement(
        "/etc/NetworkManager/dispatcher.d/pre-down.d",
        InstallerPathPolicy::MutateDirectory,
    ),
    requirement(
        "/usr/share/applications",
        InstallerPathPolicy::MutateDirectory,
    ),
];

const OPTIONAL_SYSTEM_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[
    requirement(
        "/usr/bin/lg-buddy-gui",
        InstallerPathPolicy::ReplaceExecutable,
    ),
    requirement("/usr/share/icons", InstallerPathPolicy::MutateDirectory),
    requirement(
        "/usr/share/icons/hicolor",
        InstallerPathPolicy::MutateDirectory,
    ),
    requirement(
        "/usr/share/icons/hicolor/scalable",
        InstallerPathPolicy::MutateDirectory,
    ),
    requirement(
        "/usr/share/icons/hicolor/scalable/apps",
        InstallerPathPolicy::MutateDirectory,
    ),
    requirement(
        "/usr/share/icons/hicolor/scalable/apps/io.github.staphylococcus.LGBuddy.svg",
        InstallerPathPolicy::ReplaceFile,
    ),
];

// Older releases do not have the setup helper yet. Its destinations must be
// safe to create when absent and safe to replace when already installed.
const SETUP_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[
    requirement("/usr/lib/lg-buddy", InstallerPathPolicy::MutateDirectory),
    requirement(
        "/usr/lib/lg-buddy/setup",
        InstallerPathPolicy::MutateDirectory,
    ),
    requirement(
        "/usr/lib/lg-buddy/setup/systemd",
        InstallerPathPolicy::MutateDirectory,
    ),
    requirement(
        "/usr/lib/lg-buddy/setup-services",
        InstallerPathPolicy::ReplaceExecutable,
    ),
    requirement(
        "/usr/lib/lg-buddy/setup/systemd/LG_Buddy.service",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/usr/lib/lg-buddy/setup/systemd/LG_Buddy_lifecycle.service",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "/usr/lib/lg-buddy/setup/systemd/lg_buddy.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement("/usr/share/polkit-1", InstallerPathPolicy::MutateDirectory),
    requirement(
        "/usr/share/polkit-1/actions",
        InstallerPathPolicy::MutateDirectory,
    ),
    requirement(
        "/usr/share/polkit-1/actions/io.github.staphylococcus.LGBuddy.setup.policy",
        InstallerPathPolicy::ReplaceFile,
    ),
];

const LEGACY_ENV_REMOVAL_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[requirement(
    "/usr/bin/LG_Buddy_PIP",
    InstallerPathPolicy::RecursiveClear,
)];

const LEGACY_SYSTEM_PATHS: &[&str] = &[
    "/usr/bin/LG_Buddy_Startup",
    "/usr/bin/LG_Buddy_Shutdown",
    "/usr/bin/LG_Buddy_Screen_On",
    "/usr/bin/LG_Buddy_Screen_Off",
    "/usr/bin/LG_Buddy_Screen_Monitor",
    "/usr/bin/LG_Buddy_sleep_pre",
    "/usr/bin/LG_Buddy_Brightness",
    "/usr/lib/lg-buddy/common.sh",
    "/usr/lib/systemd/system-sleep/LG_Buddy_sleep_hook",
    "/etc/systemd/system/LG_Buddy_wake.service",
    "/etc/systemd/system/LG_Buddy_wake.service.d",
    "/etc/systemd/system/LG_Buddy_sleep.service",
    "/etc/systemd/system/LG_Buddy_sleep.service.d",
    "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_sleep",
];

const USER_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[
    requirement("LG_Buddy_screen.service", InstallerPathPolicy::ReplaceFile),
    requirement(
        "LG_Buddy_screen.service.d/config.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "LG_Buddy_update_check.service",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "LG_Buddy_update_check.service.d/config.conf",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement(
        "LG_Buddy_update_check.timer",
        InstallerPathPolicy::ReplaceFile,
    ),
    requirement("", InstallerPathPolicy::MutateDirectory),
    requirement(
        "LG_Buddy_screen.service.d",
        InstallerPathPolicy::ExactDropInDirectory {
            expected_entry: "config.conf",
        },
    ),
    requirement(
        "LG_Buddy_update_check.service.d",
        InstallerPathPolicy::ExactDropInDirectory {
            expected_entry: "config.conf",
        },
    ),
];

// These are the inputs consumed by the non-interactive `install.sh --upgrade`
// contract. Configuration and pairing scripts are deliberately not upgrade inputs.
const CANDIDATE_PATH_REQUIREMENTS: &[InstallerPathRequirement] = &[
    requirement("", InstallerPathPolicy::InputDirectory),
    requirement("systemd", InstallerPathPolicy::InputDirectory),
    requirement("release-manifest.json", InstallerPathPolicy::ReadableInput),
    requirement("install.sh", InstallerPathPolicy::ExecutableInput),
    requirement("lg-buddy", InstallerPathPolicy::ExecutableInput),
    requirement("docs/setup-services.sh", InstallerPathPolicy::ReadableInput),
    requirement(
        "docs/io.github.staphylococcus.LGBuddy.setup.policy",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "docs/lg-buddy-gui-x86_64-unknown-linux-gnu",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "docs/io.github.staphylococcus.LGBuddy.svg",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "LG_Buddy_Brightness.desktop",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy.service",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy_lifecycle.service",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy_screen.service",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy_update_check.service",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement(
        "systemd/LG_Buddy_update_check.timer",
        InstallerPathPolicy::ReadableInput,
    ),
    requirement("systemd/lg_buddy.conf", InstallerPathPolicy::ReadableInput),
];


/// Run the full installed-state preflight and return the accumulated failures.
///
/// `executable_check` / `executable_remedy` name the expected running binary
/// (the CLI or the GUI process); `remove_legacy_env` gates the legacy-venv
/// wipe; `require_gui` promotes the GUI binary from optional to mandatory.
pub(super) fn evaluate_installed_state(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
    expected_running_executable: &Path,
    executable_check: &'static str,
    executable_remedy: &'static str,
    remove_legacy_env: bool,
    require_gui: bool,
) -> Vec<CompatibilityFailure> {
    let mut checker = Checker::new(filesystem);
    let roots = trust_placement(&facts.layout, facts.system_owner_uid, facts.user_owner_uid);
    let system_trust = roots.system_trust;
    let user_trust = roots.user_trust;
    let user_units_trust = roots.user_units_trust;

    if facts.effective_uid == 0 {
        checker.failures.push(CompatibilityFailure {
            check: "invoking-user",
            path: None,
            detail: "the updater is running as root".to_string(),
            remedy: "run LG Buddy as the installed user; it will request sudo only for the mutation step".to_string(),
        });
    }
    check_normalized_absolute(&mut checker.failures, "system-root", &facts.layout.system_root);
    check_normalized_absolute(&mut checker.failures, "user-home", &facts.layout.user_home);
    check_normalized_absolute(
        &mut checker.failures,
        "user-config-home",
        &facts.layout.user_config_home,
    );

    if facts.running_executable != expected_running_executable {
        checker.failures.push(CompatibilityFailure {
            check: executable_check,
            path: Some(facts.running_executable.clone()),
            detail: format!(
                "running executable is not the expected runtime at {}",
                expected_running_executable.display()
            ),
            remedy: executable_remedy.to_string(),
        });
    }

    for requirement in SYSTEM_PATH_REQUIREMENTS {
        let check = match requirement.policy {
            InstallerPathPolicy::MutateDirectory | InstallerPathPolicy::RecursiveClear => {
                "mutable-installation"
            }
            InstallerPathPolicy::ExactDropInDirectory { .. } => "integration-config",
            _ => "installed-layout",
        };
        checker.check_requirement(
            &facts.layout.system_path(requirement.path),
            facts.system_owner_uid,
            Some(system_trust),
            requirement.policy,
            check,
        );
    }
    checker.check_replace_file_alternatives(
        &SYSTEM_DESKTOP_ENTRY_PATHS
            .iter()
            .map(|path| facts.layout.system_path(path))
            .collect::<Vec<_>>(),
        facts.system_owner_uid,
        Some(system_trust),
        "installed-layout",
    );
    for requirement in OPTIONAL_SYSTEM_PATH_REQUIREMENTS {
        let path = facts.layout.system_path(requirement.path);
        if require_gui && requirement.path == "/usr/bin/lg-buddy-gui" {
            checker.check_requirement(
                &path,
                facts.system_owner_uid,
                Some(system_trust),
                requirement.policy,
                "installed-layout",
            );
        } else {
            checker.check_optional_requirement(
                &path,
                facts.system_owner_uid,
                Some(system_trust),
                requirement.policy,
                "installed-layout",
            );
        }
    }
    for requirement in SETUP_PATH_REQUIREMENTS {
        checker.check_install_destination(
            &facts.layout.system_path(requirement.path),
            facts.system_owner_uid,
            Some(system_trust),
            requirement.policy,
            "setup-installation",
        );
    }
    if remove_legacy_env {
        for requirement in LEGACY_ENV_REMOVAL_PATH_REQUIREMENTS {
            checker.check_requirement(
                &facts.layout.system_path(requirement.path),
                facts.system_owner_uid,
                Some(system_trust),
                requirement.policy,
                "legacy-environment-removal",
            );
        }
    }
    for requirement in USER_PATH_REQUIREMENTS {
        let check = match requirement.policy {
            InstallerPathPolicy::MutateDirectory => "mutable-user-integration",
            InstallerPathPolicy::ExactDropInDirectory { .. } => "integration-config",
            _ => "user-integration",
        };
        checker.check_requirement(
            &facts.layout.user_systemd_path(requirement.path),
            facts.user_owner_uid,
            Some(user_units_trust),
            requirement.policy,
            check,
        );
    }
    for path in USER_DESKTOP_ENTRY_PATHS {
        checker.check_optional_requirement(
            &facts.layout.user_home.join(path),
            facts.user_owner_uid,
            Some(user_trust),
            InstallerPathPolicy::ReplaceFile,
            "user-desktop",
        );
    }

    for path in LEGACY_SYSTEM_PATHS {
        checker.check_absent(&facts.layout.system_path(path));
    }

    let config_path = checker.read_config_pointer(
        &facts.layout.config_pointer(),
        facts.system_owner_uid,
        &facts.layout.system_root,
    );
    if let Some(config_path) = config_path {
        let config_marker = systemd_config_override_line(&config_path);
        for path in [
            facts.layout.system_path("/etc/systemd/system/LG_Buddy.service.d/config.conf"),
            facts.layout.system_path("/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf"),
            facts.layout.user_systemd_path("LG_Buddy_screen.service.d/config.conf"),
            facts.layout.user_systemd_path("LG_Buddy_update_check.service.d/config.conf"),
        ] {
            checker.check_integration_override(&path, &config_marker);
        }
        checker.check_config_tree(&config_path, facts.user_owner_uid);
    }

    checker.check_capability(
        "system-service-manager",
        &facts.service_managers.system,
        "make the system systemd manager available before upgrading",
    );
    checker.check_capability(
        "user-service-manager",
        &facts.service_managers.user,
        "run the upgrade from a user session with a reachable systemd user manager",
    );

    checker.failures
}

/// Validate the candidate release bundle before upgrade: absolute-path guard
/// then a walk of `CANDIDATE_PATH_REQUIREMENTS`. Returns the accumulated
/// failures (empty when the bundle is complete and untampered).
pub(super) fn evaluate_candidate_preflight(
    filesystem: &impl FilesystemFacts,
    candidate_root: &Path,
    user_owner_uid: u32,
) -> Vec<CompatibilityFailure> {
    let mut checker = Checker::new(filesystem);
    if !check_normalized_absolute(&mut checker.failures, "candidate-root", candidate_root) {
        return checker.failures;
    }

    let candidate_trust = TrustedRoot::candidate(candidate_root, user_owner_uid);
    for requirement in CANDIDATE_PATH_REQUIREMENTS {
        checker.check_requirement(
            &candidate_root.join(requirement.path),
            user_owner_uid,
            Some(candidate_trust),
            requirement.policy,
            "candidate-layout",
        );
    }
    checker.failures
}

// ---------------------------------------------------------------------------
// Fixture materialization for the L2 preflight integration tests.
//
// The requirement tables above are the preflight module's own data. When the
// parent's test suite builds a filesystem fixture, it asks *these* operations
// to lay files down — it never reads the tables directly. Keeping the
// materialization here means the parent stays ignorant of the table contents.
// ---------------------------------------------------------------------------
#[cfg(test)]
pub(super) mod fixture_ops {
    use super::*;
    use super::super::observation::InstalledLayout;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn write_fixture_file(path: &Path, executable: bool) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"fixture\n").unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).unwrap();
    }

    /// Lay down the system-tree requirement files and directories under the
    /// layout's system root (the state a working installation presents).
    /// Includes the always-present brightness desktop launcher.
    pub fn materialize_system_requirements(layout: &InstalledLayout) {
        for requirement in SYSTEM_PATH_REQUIREMENTS {
            if requirement.policy.expects_directory() {
                fs::create_dir_all(layout.system_path(requirement.path)).unwrap();
            }
        }
        for requirement in SYSTEM_PATH_REQUIREMENTS {
            if requirement.policy.expects_file() {
                write_fixture_file(
                    &layout.system_path(requirement.path),
                    matches!(requirement.policy, InstallerPathPolicy::ReplaceExecutable),
                );
            }
        }
        write_fixture_file(
            &layout.system_path("/usr/share/applications/LG_Buddy_Brightness.desktop"),
            false,
        );
    }

    /// Lay down the requirement files for the user systemd tree rooted at
    /// `base` (typically `layout.user_systemd_path("")`).
    pub fn materialize_user_requirements(base: &Path) {
        for requirement in USER_PATH_REQUIREMENTS {
            if requirement.policy.expects_directory() {
                fs::create_dir_all(base.join(requirement.path)).unwrap();
            }
        }
        for requirement in USER_PATH_REQUIREMENTS {
            if requirement.policy.expects_file() {
                write_fixture_file(
                    &base.join(requirement.path),
                    matches!(requirement.policy, InstallerPathPolicy::ExecutableInput),
                );
            }
        }
    }

    /// Lay down the candidate bundle requirement files rooted at `base`.
    pub fn materialize_candidate_requirements(base: &Path) {
        for requirement in CANDIDATE_PATH_REQUIREMENTS {
            if requirement.policy.expects_directory() {
                fs::create_dir_all(base.join(requirement.path)).unwrap();
            }
        }
        for requirement in CANDIDATE_PATH_REQUIREMENTS {
            if requirement.policy.expects_file() {
                write_fixture_file(
                    &base.join(requirement.path),
                    matches!(requirement.policy, InstallerPathPolicy::ExecutableInput),
                );
            }
        }
    }

    /// Lay down the setup-tree requirement files under the layout's system
    /// root (a working installation has them; an old one does not).
    pub fn materialize_setup_requirements(layout: &InstalledLayout) {
        for requirement in SETUP_PATH_REQUIREMENTS {
            let path = layout.system_path(requirement.path);
            if requirement.policy.expects_directory() {
                fs::create_dir_all(&path).unwrap();
            } else {
                write_fixture_file(
                    &path,
                    requirement.policy == InstallerPathPolicy::ReplaceExecutable,
                );
            }
        }
    }

    /// The setup-tree requirement paths (relative to the system root). The
    /// parent's tests resolve each against their own fixture layout and turn
    /// it into a dangling symlink.
    pub fn setup_requirement_paths() -> Vec<&'static str> {
        SETUP_PATH_REQUIREMENTS.iter().map(|r| r.path).collect()
    }

    /// The candidate-bundle requirement paths, each flagged as directory or
    /// file, so the parent's tests can remove them one input at a time.
    pub fn candidate_input_paths() -> Vec<(&'static str, bool)> {
        CANDIDATE_PATH_REQUIREMENTS
            .iter()
            .filter(|r| !r.path.is_empty())
            .map(|r| (r.path, r.policy.expects_directory()))
            .collect()
    }
}


// ---------------------------------------------------------------------------
// L1 tests for the orchestrator's decision rules: which checks fire under
// which flags and trust roots, driven by an in-memory filesystem (the same
// pattern as path_safety's tests) so no check touches the real host.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::super::observation::{
        FilesystemFacts, HostPreflightFacts, InstalledLayout, PathFacts, PathKind,
        ServiceManagerFacts,
    };
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io;
    use std::path::{Path, PathBuf};

    struct FakeFacts {
        entries: RefCell<HashMap<PathBuf, PathFacts>>,
    }

    fn dir(uid: u32, mode: u32) -> PathFacts {
        PathFacts {
            kind: PathKind::Directory,
            owner_uid: uid,
            mode,
            link_count: 1,
            read_only_filesystem: false,
            mount_point: false,
        }
    }

    fn file(uid: u32, mode: u32) -> PathFacts {
        PathFacts {
            kind: PathKind::File,
            owner_uid: uid,
            mode,
            link_count: 1,
            read_only_filesystem: false,
            mount_point: false,
        }
    }

    /// A host where every *default* decision passes: GUI and legacy state
    /// absent, correct owners, owner-writable directories. A test that flips
    /// one gate or one trust root therefore sees exactly that failure.
    fn base_entries() -> HashMap<PathBuf, PathFacts> {
        let mut e = HashMap::new();
        // Ancestors the strict system trust root walks down from /, and the
        // system destination directories. Owner 0, owner-writable, no
        // group/other write (the strict root rejects other writes).
        for p in [
            "/",
            "/usr",
            "/usr/bin",
            "/usr/lib",
            "/usr/lib/lg-buddy",
            "/usr/share",
            "/usr/share/applications",
            "/etc",
            "/etc/systemd",
            "/etc/systemd/system",
            "/etc/systemd/system/LG_Buddy.service.d",
            "/etc/systemd/system/LG_Buddy_lifecycle.service.d",
            "/etc/tmpfiles.d",
            "/etc/NetworkManager",
            "/etc/NetworkManager/dispatcher.d",
            "/etc/NetworkManager/dispatcher.d/pre-down.d",
        ] {
            e.insert(PathBuf::from(p), dir(0, 0o755));
        }
        for p in [
            "/usr/bin/lg-buddy",
            "/etc/systemd/system/LG_Buddy.service",
            "/etc/systemd/system/LG_Buddy_lifecycle.service",
            "/etc/systemd/system/LG_Buddy.service.d/config.conf",
            "/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf",
            "/etc/tmpfiles.d/lg_buddy.conf",
            "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle",
            "/usr/share/applications/io.github.staphylococcus.LGBuddy.desktop",
        ] {
            e.insert(PathBuf::from(p), file(0, 0o755));
        }
        // Config pointer: SystemReadableInput (0o404).
        e.insert(
            PathBuf::from("/usr/lib/lg-buddy/config-path"),
            file(0, 0o644),
        );
        // User tree. The home root is the owned trust root for the home; the
        // config home lives inside it, so the unit tree is trusted as home.
        e.insert(PathBuf::from("/home"), dir(0, 0o755));
        e.insert(PathBuf::from("/home/u"), dir(1000, 0o755));
        for p in [
            "/home/u/.config",
            "/home/u/.config/systemd",
            "/home/u/.config/systemd/user",
            "/home/u/.config/systemd/user/LG_Buddy_screen.service.d",
            "/home/u/.config/systemd/user/LG_Buddy_update_check.service.d",
            "/home/u/.lg-buddy",
            "/home/u/Desktop",
        ] {
            e.insert(PathBuf::from(p), dir(1000, 0o755));
        }
        for p in [
            "/home/u/.config/systemd/user/LG_Buddy_screen.service",
            "/home/u/.config/systemd/user/LG_Buddy_screen.service.d/config.conf",
            "/home/u/.config/systemd/user/LG_Buddy_update_check.service",
            "/home/u/.config/systemd/user/LG_Buddy_update_check.service.d/config.conf",
            "/home/u/.config/systemd/user/LG_Buddy_update_check.timer",
            "/home/u/Desktop/io.github.staphylococcus.LGBuddy.desktop",
            "/home/u/Desktop/LG_Buddy_Brightness.desktop",
            "/home/u/.lg-buddy/config.env",
        ] {
            e.insert(PathBuf::from(p), file(1000, 0o644));
        }
        e
    }

    fn fake(entries: HashMap<PathBuf, PathFacts>) -> FakeFacts {
        FakeFacts {
            entries: RefCell::new(entries),
        }
    }

    impl FilesystemFacts for FakeFacts {
        fn path_facts(&self, path: &Path) -> io::Result<PathFacts> {
            self.entries
                .borrow()
                .get(path)
                .cloned()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("missing {}", path.display()),
                    )
                })
        }
        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            match path.to_str() {
                Some(p) if p.ends_with("config-path") => {
                    Ok("/home/u/.lg-buddy/config.env\n".to_string())
                }
                Some(p) if p.ends_with("config.conf") => Ok(
                    "Environment=\"LG_BUDDY_CONFIG=/home/u/.lg-buddy/config.env\"\n".to_string(),
                ),
                _ => Ok(String::new()),
            }
        }
        fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            Ok(if path
                .to_str()
                .is_some_and(|p| p.contains(".service.d"))
            {
                vec![path.join("config.conf")]
            } else {
                Vec::new()
            })
        }
        fn mount_points(&self) -> io::Result<Vec<PathBuf>> {
            Ok(Vec::new())
        }
    }

    fn host_facts() -> HostPreflightFacts {
        HostPreflightFacts {
            layout: InstalledLayout {
                system_root: PathBuf::from("/"),
                user_home: PathBuf::from("/home/u"),
                user_config_home: PathBuf::from("/home/u/.config"),
            },
            running_executable: PathBuf::from("/usr/bin/lg-buddy"),
            effective_uid: 1000,
            system_owner_uid: 0,
            user_owner_uid: 1000,
            service_managers: ServiceManagerFacts::skipped(),
        }
    }

    fn check_paths<'a>(
        failures: &'a [CompatibilityFailure],
        check: &str,
    ) -> Vec<&'a Path> {
        failures
            .iter()
            .filter(|f| f.check == check)
            .map(|f| f.path.as_deref().unwrap_or_else(|| Path::new("")))
            .collect()
    }

    fn evaluate(
        fs: &FakeFacts,
        facts: &HostPreflightFacts,
        remove_legacy_env: bool,
        require_gui: bool,
    ) -> Vec<CompatibilityFailure> {
        evaluate_installed_state(
            fs,
            facts,
            Path::new("/usr/bin/lg-buddy"),
            "running-executable",
            "restart the CLI runtime",
            remove_legacy_env,
            require_gui,
        )
    }

    #[test]
    fn base_fixture_is_compatible_with_default_flags() {
        let fs = fake(base_entries());
        let failures = evaluate(&fs, &host_facts(), false, false);
        assert!(
            failures.is_empty(),
            "expected a clean preflight, got: {failures:?}"
        );
    }

    #[test]
    fn require_gui_makes_the_gui_binary_mandatory() {
        // The GUI binary is absent. With require_gui off a missing GUI is
        // tolerated; with it on the missing binary is refused.
        let fs = fake(base_entries());
        let expected = Path::new("/usr/bin/lg-buddy-gui");

        assert!(
            check_paths(&evaluate(&fs, &host_facts(), false, false), "installed-layout")
                .is_empty(),
            "a missing GUI binary is tolerated when require_gui is off"
        );
        assert_eq!(
            check_paths(&evaluate(&fs, &host_facts(), false, true), "installed-layout"),
            vec![expected],
            "require_gui must refuse the missing GUI binary"
        );
    }

    #[test]
    fn remove_legacy_env_gates_the_virtualenv_clear_check() {
        // The virtualenv directory is present but owned by the invoking user,
        // not root: it fails the owner check. The gate is whether that check
        // runs at all.
        let mut entries = base_entries();
        entries.insert(PathBuf::from("/usr/bin/LG_Buddy_PIP"), dir(1000, 0o755));
        let fs = fake(entries);
        let expected = Path::new("/usr/bin/LG_Buddy_PIP");

        assert!(
            check_paths(
                &evaluate(&fs, &host_facts(), false, false),
                "legacy-environment-removal"
            )
            .is_empty(),
            "the legacy wipe must be skipped when remove_legacy_env is off"
        );
        assert_eq!(
            check_paths(&evaluate(&fs, &host_facts(), true, false), "legacy-environment-removal"),
            vec![expected],
            "remove_legacy_env must check the virtualenv when enabled"
        );
    }

    #[test]
    fn a_misowned_config_file_breaks_the_config_integrity_sequence() {
        let mut entries = base_entries();
        // The config tree is walked under the user's ownership; a config.env
        // owned by another uid is refused by the config-state sequence even
        // though the pointer and the integration overrides are all valid.
        entries.insert(PathBuf::from("/home/u/.lg-buddy/config.env"), file(999, 0o644));
        let fs = fake(entries);
        let failures = evaluate(&fs, &host_facts(), false, false);
        assert_eq!(
            check_paths(&failures, "config-state"),
            vec![Path::new("/home/u/.lg-buddy/config.env")],
            "the config tree is owner-checked as part of the integrity sequence"
        );
        assert!(
            check_paths(&failures, "integration-config").is_empty(),
            "the overrides still reference the correct config path"
        );
    }

    #[test]
    fn candidate_preflight_walks_the_bundle_under_a_candidate_trust_root() {
        let mut entries = HashMap::new();
        // External ancestors of the candidate root must be root- or
        // user-owned and not group/other-writable without sticky protection.
        entries.insert(PathBuf::from("/"), dir(0, 0o755));
        entries.insert(PathBuf::from("/home"), dir(0, 0o755));
        entries.insert(PathBuf::from("/home/u"), dir(1000, 0o755));
        entries.insert(PathBuf::from("/home/u/releases"), dir(1000, 0o1777));
        let root = PathBuf::from("/home/u/releases/candidate");
        for (relative, facts) in [
            ("", dir(1000, 0o755)),
            ("systemd", dir(1000, 0o755)),
            ("docs", dir(1000, 0o755)),
            ("release-manifest.json", file(1000, 0o644)),
            ("install.sh", file(1000, 0o755)),
            ("lg-buddy", file(1000, 0o755)),
            ("docs/setup-services.sh", file(1000, 0o644)),
            (
                "docs/io.github.staphylococcus.LGBuddy.setup.policy",
                file(1000, 0o644),
            ),
            (
                "docs/lg-buddy-gui-x86_64-unknown-linux-gnu",
                file(1000, 0o644),
            ),
            ("docs/io.github.staphylococcus.LGBuddy.svg", file(1000, 0o644)),
            ("LG_Buddy_Brightness.desktop", file(1000, 0o644)),
            ("systemd/LG_Buddy.service", file(1000, 0o644)),
            ("systemd/LG_Buddy_lifecycle.service", file(1000, 0o644)),
            ("systemd/LG_Buddy_screen.service", file(1000, 0o644)),
            ("systemd/LG_Buddy_update_check.service", file(1000, 0o644)),
            (
                "systemd/LG_Buddy_update_check.timer",
                file(1000, 0o644),
            ),
            ("systemd/lg_buddy.conf", file(1000, 0o644)),
        ] {
            entries.insert(root.join(relative), facts);
        }
        let fs = fake(entries);
        let failures = evaluate_candidate_preflight(&fs, &root, 1000);
        assert!(
            failures.is_empty(),
            "a complete, untampered bundle must pass, got: {failures:?}"
        );
    }

    #[test]
    fn a_relative_candidate_root_is_refused_before_the_walk() {
        let fs = fake(base_entries());
        let failures = evaluate_candidate_preflight(&fs, Path::new("candidate"), 1000);
        assert_eq!(
            check_paths(&failures, "candidate-root"),
            vec![Path::new("candidate")],
            "a non-absolute candidate root is refused before the walk"
        );
    }
}
