use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallerPathPolicy {
    ReplaceFile,
    ReplaceExecutable,
    MutateDirectory,
    RecursiveClear,
    ExactDropInDirectory { expected_entry: &'static str },
    ReadableInput,
    SystemReadableInput,
    ExecutableInput,
    InputDirectory,
}

impl InstallerPathPolicy {
    pub(super) fn expects_file(self) -> bool {
        matches!(
            self,
            Self::ReplaceFile
                | Self::ReplaceExecutable
                | Self::ReadableInput
                | Self::SystemReadableInput
                | Self::ExecutableInput
        )
    }

    pub(super) fn expects_directory(self) -> bool {
        matches!(
            self,
            Self::MutateDirectory
                | Self::RecursiveClear
                | Self::ExactDropInDirectory { .. }
                | Self::InputDirectory
        )
    }
}


mod observation;
mod path_safety;
mod preflight;
mod trust_placement;

use observation::observe_process;
use preflight::{
    evaluate_candidate_preflight as candidate_preflight, evaluate_installed_state,
};

pub use observation::{
    FilesystemFacts, HostPreflightFacts, InstalledLayout, ObservationFailure, OsFilesystemFacts,
    PathFacts, PathKind, ServiceManagerFacts, SystemdManagerObservation,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityFailure {
    pub check: &'static str,
    pub path: Option<PathBuf>,
    pub detail: String,
    pub remedy: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompatibilityReport {
    failures: Vec<CompatibilityFailure>,
}

/// Safe advice for the GUI; paths and raw host observations stay in diagnostics.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CompatibilityAdvice {
    pub compatible: bool,
    pub failures: Vec<CompatibilityAdviceItem>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CompatibilityAdviceItem {
    pub check: String,
    pub remedy: String,
}

impl CompatibilityReport {
    /// Build the downstream report view from the orchestrator's raw failures.
    pub fn from_failures(failures: Vec<CompatibilityFailure>) -> Self {
        Self { failures }
    }

    pub(crate) fn advice(&self) -> CompatibilityAdvice {
        CompatibilityAdvice {
            compatible: self.compatible(),
            failures: self
                .failures
                .iter()
                .map(|failure| CompatibilityAdviceItem {
                    check: failure.check.into(),
                    remedy: failure.remedy.clone(),
                })
                .collect(),
        }
    }

    pub fn compatible(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn failures(&self) -> &[CompatibilityFailure] {
        &self.failures
    }

    pub fn render(&self) -> String {
        if self.compatible() {
            return "upgrade preflight: compatible\n".to_string();
        }

        let mut output = String::from("upgrade preflight: refused\n");
        for failure in &self.failures {
            output.push_str("- ");
            output.push_str(failure.check);
            if let Some(path) = &failure.path {
                output.push_str(" (");
                output.push_str(&path.display().to_string());
                output.push(')');
            }
            output.push_str(": ");
            output.push_str(&failure.detail);
            output.push_str(" Remedy: ");
            output.push_str(&failure.remedy);
            output.push('\n');
        }
        output
    }

    fn refuse(
        &mut self,
        check: &'static str,
        path: Option<PathBuf>,
        detail: impl Into<String>,
        remedy: impl Into<String>,
    ) {
        self.failures.push(CompatibilityFailure {
            check,
            path,
            detail: detail.into(),
            remedy: remedy.into(),
        });
    }
}

impl fmt::Display for CompatibilityReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

pub fn current_host_preflight() -> CompatibilityReport {
    let facts = match observe_process() {
        Ok(facts) => facts,
        Err(failure) => return failure_report(&failure),
    };
    evaluate_initial_preflight(&OsFilesystemFacts, &facts)
}

/// Evaluate the installed host from the graphical executable's process.
///
/// The GUI and CLI share the same installation layout and trust checks. The
/// only process-specific differences are the executable that must be running
/// and the fact that the GUI binary is required for a GUI-led upgrade.
pub fn current_gui_host_preflight() -> CompatibilityReport {
    let facts = match observe_process() {
        Ok(facts) => facts,
        Err(failure) => return failure_report(&failure),
    };
    evaluate_gui_initial_preflight(&OsFilesystemFacts, &facts)
}

pub fn candidate_host_preflight(
    candidate_root: &Path,
    remove_legacy_env: bool,
) -> CompatibilityReport {
    let facts = match observe_process() {
        Ok(facts) => facts,
        Err(failure) => return failure_report(&failure),
    };
    evaluate_candidate_host_preflight(
        &OsFilesystemFacts,
        &facts,
        candidate_root,
        remove_legacy_env,
    )
}

fn failure_report(failure: &ObservationFailure) -> CompatibilityReport {
    let (check, path, problem, remedy) = match failure {
        ObservationFailure::RunningExecutable(problem) => (
            "running-executable",
            None,
            problem.clone(),
            "run the installed LG Buddy executable directly".to_string(),
        ),
        ObservationFailure::UserHome => (
            "user-home",
            None,
            "HOME is not available".to_string(),
            "run the updater from the installed user's normal session".to_string(),
        ),
    };
    let mut report = CompatibilityReport::default();
    report.refuse(check, path, problem, remedy);
    report
}

pub fn evaluate_initial_preflight(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
) -> CompatibilityReport {
    evaluate_initial_preflight_for_process(
        filesystem,
        facts,
        &facts.layout.installed_executable(),
        "run the release-bundle installation at /usr/bin/lg-buddy, or use the host's native package manager",
        false,
    )
}

/// Evaluate the installed host for a GUI-led upgrade using the same checks as
/// [`evaluate_initial_preflight`]. The installed GUI binary is mandatory so a
/// successful handoff always has a verified target.
pub fn evaluate_gui_initial_preflight(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
) -> CompatibilityReport {
    evaluate_initial_preflight_for_process(
        filesystem,
        facts,
        &facts.layout.system_path("/usr/bin/lg-buddy-gui"),
        "run the installed graphical executable at /usr/bin/lg-buddy-gui, or use the host's native package manager",
        true,
    )
}

fn evaluate_initial_preflight_for_process(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
    expected_running_executable: &Path,
    executable_remedy: &'static str,
    require_gui: bool,
) -> CompatibilityReport {
    CompatibilityReport::from_failures(evaluate_installed_state(
        filesystem,
        facts,
        expected_running_executable,
        "running-executable",
        executable_remedy,
        false,
        require_gui,
    ))
}

pub fn evaluate_candidate_preflight(
    filesystem: &impl FilesystemFacts,
    candidate_root: &Path,
    user_owner_uid: u32,
) -> CompatibilityReport {
    CompatibilityReport::from_failures(candidate_preflight(
        filesystem,
        candidate_root,
        user_owner_uid,
    ))
}

pub fn evaluate_candidate_host_preflight(
    filesystem: &impl FilesystemFacts,
    facts: &HostPreflightFacts,
    candidate_root: &Path,
    remove_legacy_env: bool,
) -> CompatibilityReport {
    let expected_candidate_executable = candidate_root.join("lg-buddy");
    let mut failures = evaluate_installed_state(
        filesystem,
        facts,
        &expected_candidate_executable,
        "candidate-executable",
        "run the preflight with the verified candidate binary from this bundle",
        remove_legacy_env,
        false,
    );
    failures.extend(candidate_preflight(
        filesystem,
        candidate_root,
        facts.user_owner_uid,
    ));
    CompatibilityReport::from_failures(failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use path_safety::{service_manager_refusal, Checker, TrustedRoot};
    use preflight::fixture_ops;
    use std::env;
    use std::fs;
    use std::io;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn graphical_preflight_advice_excludes_paths_and_raw_observations() {
        let mut report = CompatibilityReport::default();
        report.refuse(
            "installed-layout",
            Some(PathBuf::from("/private/user-config")),
            "raw observation that may contain private data",
            "Restore the installed runtime before retrying.",
        );
        let json = serde_json::to_string(&report.advice()).unwrap();
        assert!(!json.contains("/private"));
        assert!(!json.contains("raw observation"));
        let advice: CompatibilityAdvice = serde_json::from_str(&json).unwrap();
        assert!(!advice.compatible);
        assert_eq!(advice.failures.len(), 1);
        assert_eq!(advice.failures[0].check, "installed-layout");
        assert_eq!(
            advice.failures[0].remedy,
            "Restore the installed runtime before retrying."
        );
    }

    #[test]
    fn supported_release_bundle_layout_passes_initial_and_candidate_preflights() {
        let fixture = InstalledFixture::new("supported");
        let filesystem = RootOwnedFilesystem(OsFilesystemFacts);

        let initial = evaluate_initial_preflight(&filesystem, &fixture.facts);
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");
        let candidate = evaluate_candidate_host_preflight(
            &filesystem,
            &candidate_facts,
            &fixture.candidate_root,
            false,
        );

        assert!(initial.compatible(), "{}", initial.render());
        assert!(candidate.compatible(), "{}", candidate.render());
    }

    #[test]
    fn user_units_follow_absolute_xdg_config_home_with_default_fallbacks() {
        for xdg in [None, Some("".into()), Some("relative/config".into())] {
            let layout = InstalledLayout::new("/", "/home/user", xdg);
            assert_eq!(
                layout.user_systemd_path("service"),
                Path::new("/home/user/.config/systemd/user/service")
            );
        }
        let layout = InstalledLayout::new("/", "/home/user", Some("/custom/config".into()));
        assert_eq!(
            layout.user_systemd_path("service"),
            Path::new("/custom/config/systemd/user/service")
        );
    }

    #[test]
    fn process_observation_uses_xdg_config_home() {
        const CHILD: &str = "LG_BUDDY_PREFLIGHT_ENV_CHILD";
        if env::var_os(CHILD).is_some() {
            let facts = super::observe_process().unwrap();
            assert_eq!(facts.layout.user_config_home, Path::new("/custom/config"));
            return;
        }
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "upgrade_preflight::tests::process_observation_uses_xdg_config_home",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("HOME", "/home/user")
            .env("XDG_CONFIG_HOME", "/custom/config")
            .env("LG_BUDDY_INSTALL_ROOT", "/isolated-root")
            .env("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn custom_user_units_pass_cli_gui_and_candidate_checks() {
        let filesystem = RootOwnedFilesystem(OsFilesystemFacts);
        for external in [false, true] {
            let mut fixture = InstalledFixture::new("xdg-config");
            let config_home = if external {
                fixture.root.join("custom-config")
            } else {
                fixture.facts.layout.user_home.join("custom-config")
            };
            fs::create_dir_all(&config_home).unwrap();
            fs::rename(
                fixture.facts.layout.user_config_home.join("systemd"),
                config_home.join("systemd"),
            )
            .unwrap();
            fixture.facts.layout.user_config_home = config_home;
            write_file(
                &fixture.facts.layout.system_path("/usr/bin/lg-buddy-gui"),
                true,
            );

            let initial = evaluate_initial_preflight(&filesystem, &fixture.facts);
            assert!(initial.compatible(), "{}", initial.render());
            let mut gui_facts = fixture.facts.clone();
            gui_facts.running_executable = gui_facts.layout.system_path("/usr/bin/lg-buddy-gui");
            let gui = evaluate_gui_initial_preflight(&filesystem, &gui_facts);
            assert!(gui.compatible(), "{}", gui.render());
            let mut candidate_facts = fixture.facts.clone();
            candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");
            let candidate = evaluate_candidate_host_preflight(
                &filesystem,
                &candidate_facts,
                &fixture.candidate_root,
                false,
            );
            assert!(candidate.compatible(), "{}", candidate.render());

            let override_path = fixture
                .facts
                .layout
                .user_systemd_path("LG_Buddy_screen.service.d/config.conf");
            fs::write(
                &override_path,
                "[Service]\nEnvironment=\"LG_BUDDY_CONFIG=/wrong/config\"\n",
            )
            .unwrap();
            let mismatch = evaluate_initial_preflight(&filesystem, &fixture.facts);
            assert!(!mismatch.compatible());
            assert!(mismatch
                .failures()
                .iter()
                .any(|failure| failure.path.as_ref() == Some(&override_path)));
        }
    }

    #[test]
    fn default_units_do_not_mask_missing_custom_units() {
        let mut fixture = InstalledFixture::new("missing-xdg-units");
        fixture.facts.layout.user_config_home = fixture.root.join("custom-config");
        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert_failure(
            &report,
            "user-integration",
            &fixture
                .facts
                .layout
                .user_systemd_path("LG_Buddy_screen.service"),
            "missing",
        );
    }

    #[test]
    fn external_config_root_must_be_user_owned() {
        let mut fixture = InstalledFixture::new("xdg-root-owner");
        let config_home = fixture.root.join("external-config");
        fs::create_dir_all(&config_home).unwrap();
        fs::rename(
            fixture.facts.layout.user_config_home.join("systemd"),
            config_home.join("systemd"),
        )
        .unwrap();
        fixture.facts.layout.user_config_home = config_home.clone();
        let baseline = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert!(baseline.compatible(), "{}", baseline.render());
        let filesystem = OverriddenFilesystem {
            path: config_home.clone(),
            owner_uid: Some(fixture.facts.user_owner_uid + 1),
            ..Default::default()
        };
        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);
        assert_failure(&report, "path-containment", &config_home, "owned by uid");
    }

    #[test]
    fn setup_destinations_can_be_created_on_old_installations_or_replaced() {
        let fixture = InstalledFixture::new("setup-upgrade");
        let old = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert!(old.compatible(), "{}", old.render());
        fixture_ops::materialize_setup_requirements(&fixture.facts.layout);
        let installed = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert!(installed.compatible(), "{}", installed.render());
    }

    #[test]
    fn setup_destinations_reject_symlinks_including_dangling_ancestors() {
        for relative in fixture_ops::setup_requirement_paths() {
            let fixture = InstalledFixture::new("setup-symlink");
            let path = fixture.facts.layout.system_path(relative);
            if path.is_dir() {
                fs::remove_dir_all(&path).unwrap();
            }
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            symlink(fixture.root.join("missing-destination"), &path).unwrap();
            let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
            assert_failure(&report, "setup-installation", &path, "found Symlink");
        }
    }

    #[test]
    fn missing_setup_destinations_require_a_trusted_mutable_parent() {
        for parent in ["/usr/lib/lg-buddy", "/usr/share"] {
            let fixture = InstalledFixture::new("setup-parent");
            let path = fixture.facts.layout.system_path(parent);
            for filesystem in [
                OverriddenFilesystem {
                    path: path.clone(),
                    read_only: Some(true),
                    ..Default::default()
                },
                OverriddenFilesystem {
                    path: path.clone(),
                    owner_uid: Some(fixture.facts.system_owner_uid + 1),
                    ..Default::default()
                },
                OverriddenFilesystem {
                    path: path.clone(),
                    mode: Some(0o777),
                    ..Default::default()
                },
            ] {
                let report = evaluate_initial_preflight(&filesystem, &fixture.facts);
                assert!(!report.compatible(), "{}", report.render());
                assert!(report
                    .failures()
                    .iter()
                    .any(|failure| failure.path.as_ref() == Some(&path)));
            }
        }
    }

    #[test]
    fn new_gui_installation_paths_are_optional_but_validated_when_present() {
        let fixture = InstalledFixture::new("optional-installed-gui");
        let gui = fixture.facts.layout.system_path("/usr/bin/lg-buddy-gui");

        let missing = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert!(missing.compatible(), "{}", missing.render());

        write_file(&gui, true);
        let installed = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert!(installed.compatible(), "{}", installed.render());

        fs::remove_file(&gui).unwrap();
        symlink(fixture.facts.layout.installed_executable(), &gui).unwrap();
        let unsafe_installation = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert_failure(
            &unsafe_installation,
            "installed-layout",
            &gui,
            "found Symlink",
        );

        fs::remove_file(&gui).unwrap();
        let icons = fixture.facts.layout.system_path("/usr/share/icons");
        symlink(&fixture.config_directory, &icons).unwrap();
        let unsafe_icon_directory = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert_failure(
            &unsafe_icon_directory,
            "installed-layout",
            &icons,
            "found Symlink",
        );
    }

    #[test]
    fn gui_initial_preflight_requires_the_running_gui_and_installed_gui_binary() {
        let mut fixture = InstalledFixture::new("required-installed-gui");
        let gui = fixture.facts.layout.system_path("/usr/bin/lg-buddy-gui");
        fixture.facts.running_executable = gui.clone();

        let missing = evaluate_gui_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert_failure(
            &missing,
            "installed-layout",
            &gui,
            "required path is missing",
        );

        write_file(&gui, true);
        let installed = evaluate_gui_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert!(installed.compatible(), "{}", installed.render());

        fixture.facts.running_executable = fixture.facts.layout.installed_executable();
        let cli_process = evaluate_gui_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert_failure(
            &cli_process,
            "running-executable",
            &fixture.facts.layout.installed_executable(),
            "not the expected runtime",
        );
    }

    #[test]
    fn legacy_environment_safety_is_required_only_when_removal_is_requested() {
        let fixture = InstalledFixture::new("conditional-legacy-removal");
        let filesystem = RootOwnedFilesystem(OsFilesystemFacts);
        let virtualenv = fixture.facts.layout.system_path("/usr/bin/LG_Buddy_PIP");
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");

        let missing = evaluate_candidate_host_preflight(
            &filesystem,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );
        assert!(missing.compatible(), "{}", missing.render());

        fs::create_dir_all(&virtualenv).unwrap();
        let removal = evaluate_candidate_host_preflight(
            &filesystem,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );
        assert!(removal.compatible(), "{}", removal.render());

        fs::remove_dir(&virtualenv).unwrap();
        symlink(&fixture.config_directory, &virtualenv).unwrap();
        let preserving = evaluate_candidate_host_preflight(
            &filesystem,
            &candidate_facts,
            &fixture.candidate_root,
            false,
        );
        let removal = evaluate_candidate_host_preflight(
            &filesystem,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );

        assert!(preserving.compatible(), "{}", preserving.render());
        assert_failure(
            &removal,
            "legacy-environment-removal",
            &virtualenv,
            "Symlink",
        );
    }

    #[test]
    fn existing_user_desktop_launcher_must_be_safely_replaceable() {
        let fixture = InstalledFixture::new("user-desktop-launcher");
        let launcher = fixture
            .facts
            .layout
            .user_home
            .join("Desktop/io.github.staphylococcus.LGBuddy.desktop");
        fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        write_file(&launcher, false);
        set_mode(&launcher, 0o400);
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            false,
        );

        assert_failure(
            &report,
            "user-desktop",
            &launcher,
            "not writable by its owner",
        );
    }

    #[test]
    fn installed_desktop_entry_accepts_legacy_or_application_id_filename() {
        let fixture = InstalledFixture::new("desktop-entry-alternatives");
        let legacy = fixture
            .facts
            .layout
            .system_path("/usr/share/applications/LG_Buddy_Brightness.desktop");
        let current = fixture
            .facts
            .layout
            .system_path("/usr/share/applications/io.github.staphylococcus.LGBuddy.desktop");

        let legacy_report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert!(legacy_report.compatible(), "{}", legacy_report.render());

        fs::rename(&legacy, &current).unwrap();
        let current_report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert!(current_report.compatible(), "{}", current_report.render());

        fs::remove_file(&current).unwrap();
        let missing_report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);
        assert_failure(
            &missing_report,
            "installed-layout",
            &current,
            "no supported LG Buddy desktop entry",
        );
    }

    #[test]
    fn installer_path_policy_permission_matrix_is_enforced() {
        let cases = [
            (
                "replace-file",
                InstallerPathPolicy::ReplaceFile,
                0o400,
                "not writable by its owner",
            ),
            (
                "replace-executable",
                InstallerPathPolicy::ReplaceExecutable,
                0o600,
                "no execute permission",
            ),
            (
                "mutate-directory",
                InstallerPathPolicy::MutateDirectory,
                0o500,
                "not writable and searchable",
            ),
            (
                "recursive-clear",
                InstallerPathPolicy::RecursiveClear,
                0o500,
                "not writable and searchable",
            ),
            (
                "exact-drop-in",
                InstallerPathPolicy::ExactDropInDirectory {
                    expected_entry: "config.conf",
                },
                0o500,
                "not readable, writable, and searchable",
            ),
            (
                "readable-input",
                InstallerPathPolicy::ReadableInput,
                0o200,
                "not readable by its owner",
            ),
            (
                "system-readable-input",
                InstallerPathPolicy::SystemReadableInput,
                0o400,
                "invoking user",
            ),
            (
                "executable-input",
                InstallerPathPolicy::ExecutableInput,
                0o400,
                "not readable and executable",
            ),
            (
                "input-directory",
                InstallerPathPolicy::InputDirectory,
                0o400,
                "not readable and searchable",
            ),
        ];

        for (label, policy, mode, expected_detail) in cases {
            let fixture = InstalledFixture::new(label);
            let (path, trusted_root, owner_uid) = match policy {
                InstallerPathPolicy::ReplaceFile => (
                    fixture
                        .facts
                        .layout
                        .system_path("/etc/systemd/system/LG_Buddy.service"),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::ReplaceExecutable => (
                    fixture.facts.layout.installed_executable(),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::MutateDirectory => (
                    fixture.facts.layout.system_path("/usr/bin"),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::RecursiveClear => (
                    fixture.facts.layout.system_path("/usr/bin/LG_Buddy_PIP"),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::ExactDropInDirectory { .. } => (
                    fixture
                        .facts
                        .layout
                        .system_path("/etc/systemd/system/LG_Buddy.service.d"),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::ReadableInput => (
                    fixture.candidate_root.join("release-manifest.json"),
                    fixture.candidate_root.clone(),
                    fixture.facts.user_owner_uid,
                ),
                InstallerPathPolicy::SystemReadableInput => (
                    fixture.facts.layout.config_pointer(),
                    fixture.facts.layout.system_root.clone(),
                    fixture.facts.system_owner_uid,
                ),
                InstallerPathPolicy::ExecutableInput => (
                    fixture.candidate_root.join("install.sh"),
                    fixture.candidate_root.clone(),
                    fixture.facts.user_owner_uid,
                ),
                InstallerPathPolicy::InputDirectory => (
                    fixture.candidate_root.join("systemd"),
                    fixture.candidate_root.clone(),
                    fixture.facts.user_owner_uid,
                ),
            };
            if policy == InstallerPathPolicy::RecursiveClear {
                fs::create_dir_all(&path).unwrap();
            }
            let filesystem = OverriddenFilesystem {
                path: path.clone(),
                owner_uid: None,
                mode: Some(mode),
                read_only: None,
                mount_point: None,
            };
            let mut checker = Checker::new(&filesystem);
            checker.check_requirement(
                &path,
                owner_uid,
                Some(TrustedRoot::strict(&trusted_root, owner_uid)),
                policy,
                "policy-contract",
            );

            assert_failure_slice(
                &checker.failures,
                "checker",
                "policy-contract",
                &path,
                expected_detail,
            );
        }
    }

    #[test]
    fn candidate_input_policy_refuses_group_or_other_write_access() {
        for (label, path, mode) in [
            ("writable-manifest", "release-manifest.json", 0o660),
            ("writable-installer", "install.sh", 0o770),
            (
                "writable-gui",
                "docs/lg-buddy-gui-x86_64-unknown-linux-gnu",
                0o660,
            ),
            ("writable-input-directory", "systemd", 0o770),
        ] {
            let fixture = InstalledFixture::new(label);
            let input = fixture.candidate_root.join(path);
            set_mode(&input, mode);

            let report = evaluate_candidate_preflight(
                &OsFilesystemFacts,
                &fixture.candidate_root,
                fixture.facts.user_owner_uid,
            );

            assert_failure(
                &report,
                "path-containment",
                &input,
                "writable by its group or by other users",
            );
        }
    }

    #[test]
    fn candidate_preflight_refuses_a_non_sticky_writable_external_ancestor() {
        let fixture = InstalledFixture::new("writable-candidate-ancestor");
        let ancestor = fixture.candidate_root.parent().unwrap().to_path_buf();
        set_mode(&ancestor, 0o777);

        let report = evaluate_candidate_preflight(
            &OsFilesystemFacts,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert_failure(
            &report,
            "path-containment",
            &ancestor,
            "without sticky-directory protection",
        );
    }

    #[test]
    fn candidate_preflight_refuses_an_external_ancestor_owned_by_another_user() {
        let fixture = InstalledFixture::new("untrusted-candidate-ancestor-owner");
        let ancestor = fixture.candidate_root.parent().unwrap().to_path_buf();
        let filesystem = OverriddenFilesystem {
            path: ancestor.clone(),
            owner_uid: Some(fixture.facts.user_owner_uid + 1),
            mode: None,
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_candidate_preflight(
            &filesystem,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert_failure(
            &report,
            "path-containment",
            &ancestor,
            "expected root or uid",
        );
    }

    #[test]
    fn candidate_preflight_accepts_a_root_owned_sticky_external_ancestor() {
        let fixture = InstalledFixture::new("sticky-candidate-ancestor");
        let ancestor = fixture.candidate_root.parent().unwrap().to_path_buf();
        let filesystem = OverriddenFilesystem {
            path: ancestor,
            owner_uid: Some(0),
            mode: Some(0o1777),
            read_only: None,
            mount_point: None,
        };
        let filesystem = RootOwnedFilesystem(filesystem);

        let report = evaluate_candidate_preflight(
            &filesystem,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert!(report.compatible(), "{}", report.render());
    }

    #[test]
    fn initial_preflight_refuses_a_symlinked_installed_runtime() {
        let fixture = InstalledFixture::new("symlink-runtime");
        let runtime = fixture.facts.layout.installed_executable();
        fs::remove_file(&runtime).unwrap();
        symlink(fixture.candidate_root.join("lg-buddy"), &runtime).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "installed-layout", &runtime, "Symlink");
    }

    #[test]
    fn legacy_environment_removal_preflight_refuses_a_symlinked_installed_virtualenv() {
        let fixture = InstalledFixture::new("symlink-virtualenv");
        let virtualenv = fixture.facts.layout.system_path("/usr/bin/LG_Buddy_PIP");
        let target = fixture.root.join("external-virtualenv");
        fs::create_dir(&virtualenv).unwrap();
        fs::remove_dir(&virtualenv).unwrap();
        fs::create_dir(&target).unwrap();
        symlink(&target, &virtualenv).unwrap();
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );

        assert_failure(
            &report,
            "legacy-environment-removal",
            &virtualenv,
            "Symlink",
        );
    }

    #[test]
    fn legacy_environment_removal_preflight_refuses_a_nested_virtualenv_mount() {
        let fixture = InstalledFixture::new("nested-virtualenv-mount");
        let nested_mount = fixture
            .facts
            .layout
            .system_path("/usr/bin/LG_Buddy_PIP/lib/python/site-packages");
        fs::create_dir_all(&nested_mount).unwrap();
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");
        let filesystem = OverriddenFilesystem {
            path: nested_mount.clone(),
            owner_uid: None,
            mode: None,
            read_only: None,
            mount_point: Some(true),
        };

        let report = evaluate_candidate_host_preflight(
            &filesystem,
            &candidate_facts,
            &fixture.candidate_root,
            true,
        );

        assert_failure(
            &report,
            "legacy-environment-removal",
            &nested_mount,
            "nested mount point",
        );
    }

    #[test]
    fn initial_preflight_refuses_an_incomplete_integration() {
        let fixture = InstalledFixture::new("missing-integration");
        let service = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy_lifecycle.service");
        fs::remove_file(&service).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "installed-layout", &service, "missing");
    }

    #[test]
    fn initial_preflight_refuses_conflicting_ownership() {
        let fixture = InstalledFixture::new("wrong-owner");
        let runtime = fixture.facts.layout.installed_executable();
        let filesystem = OverriddenFilesystem {
            path: runtime.clone(),
            owner_uid: Some(fixture.facts.system_owner_uid + 1),
            mode: None,
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(&report, "installed-layout", &runtime, "owned by uid");
    }

    #[test]
    fn initial_preflight_refuses_an_untrusted_system_ancestor() {
        let fixture = InstalledFixture::new("wrong-ancestor-owner");
        let ancestor = fixture.facts.layout.system_path("/etc/systemd");
        let filesystem = OverriddenFilesystem {
            path: ancestor.clone(),
            owner_uid: Some(fixture.facts.system_owner_uid + 1),
            mode: None,
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(&report, "path-containment", &ancestor, "owned by uid");
    }

    #[test]
    fn initial_preflight_refuses_a_system_path_writable_by_other_users() {
        let fixture = InstalledFixture::new("world-writable-system-path");
        let directory = fixture.facts.layout.system_path("/etc/systemd/system");
        let filesystem = OverriddenFilesystem {
            path: directory.clone(),
            owner_uid: None,
            mode: Some(0o777),
            read_only: None,
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(
            &report,
            "path-containment",
            &directory,
            "writable by its group or by other users",
        );
    }

    #[test]
    fn root_owned_mutation_directory_may_rely_on_privileged_write_access() {
        let fixture = InstalledFixture::new("root-owned-read-only-mode");
        let directory = fixture.facts.layout.system_path("/usr/bin");

        let root_owned = OverriddenFilesystem {
            path: directory.clone(),
            owner_uid: Some(0),
            mode: Some(0o555),
            read_only: None,
            mount_point: None,
        };
        let mut root_checker = Checker::new(&root_owned);
        root_checker.check_requirement(
            &directory,
            0,
            None,
            InstallerPathPolicy::MutateDirectory,
            "policy-contract",
        );
        assert!(
            root_checker.failures.is_empty(),
            "{:?}",
            root_checker.failures
        );

        let user_owned = OverriddenFilesystem {
            path: directory.clone(),
            owner_uid: Some(fixture.facts.user_owner_uid),
            mode: Some(0o555),
            read_only: None,
            mount_point: None,
        };
        let mut user_checker = Checker::new(&user_owned);
        user_checker.check_requirement(
            &directory,
            fixture.facts.user_owner_uid,
            None,
            InstallerPathPolicy::MutateDirectory,
            "policy-contract",
        );
        assert_failure_slice(
            &user_checker.failures,
            "checker",
            "policy-contract",
            &directory,
            "not writable and searchable",
        );
    }

    #[test]
    fn initial_preflight_refuses_read_only_installation_paths() {
        let fixture = InstalledFixture::new("read-only");
        let directory = fixture.facts.layout.system_path("/usr/bin");
        let filesystem = OverriddenFilesystem {
            path: directory.clone(),
            owner_uid: None,
            mode: None,
            read_only: Some(true),
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(
            &report,
            "mutable-installation",
            &directory,
            "read-only filesystem",
        );
    }

    #[test]
    fn initial_preflight_refuses_a_read_only_installed_file() {
        let fixture = InstalledFixture::new("read-only-file");
        let runtime = fixture.facts.layout.installed_executable();
        let filesystem = OverriddenFilesystem {
            path: runtime.clone(),
            owner_uid: None,
            mode: None,
            read_only: Some(true),
            mount_point: None,
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(
            &report,
            "installed-layout",
            &runtime,
            "read-only filesystem",
        );
    }

    #[test]
    fn initial_preflight_refuses_a_mounted_mutation_target() {
        let fixture = InstalledFixture::new("mounted-service");
        let service = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service");
        let filesystem = OverriddenFilesystem {
            path: service.clone(),
            owner_uid: None,
            mode: None,
            read_only: None,
            mount_point: Some(true),
        };

        let report = evaluate_initial_preflight(&filesystem, &fixture.facts);

        assert_failure(&report, "installed-layout", &service, "mount point");
    }

    #[test]
    fn initial_preflight_refuses_a_hard_linked_mutation_target() {
        let fixture = InstalledFixture::new("hard-linked-service");
        let service = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service");
        fs::hard_link(&service, fixture.root.join("shared-service-file")).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "installed-layout", &service, "2 hard links");
    }

    #[test]
    fn initial_preflight_refuses_a_non_writable_mutation_target() {
        let fixture = InstalledFixture::new("non-writable-user-service");
        let service = fixture
            .facts
            .layout
            .user_systemd_path("LG_Buddy_screen.service");
        let mut permissions = fs::metadata(&service).unwrap().permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(&service, permissions).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "user-integration", &service, "not writable");
    }

    #[test]
    fn initial_preflight_refuses_unavailable_service_manager() {
        let mut fixture = InstalledFixture::new("no-user-manager");
        fixture.facts.service_managers.user = SystemdManagerObservation::Reported {
            state: "offline".to_string(),
            stderr: String::new(),
        };

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        let failure = report
            .failures()
            .iter()
            .find(|failure| failure.check == "user-service-manager")
            .expect("user manager refusal");
        assert!(failure.detail.contains("offline"));
        assert!(failure.remedy.contains("user session"));
    }

    #[test]
    fn initial_preflight_refuses_integration_pointing_at_another_config() {
        let fixture = InstalledFixture::new("stale-config-override");
        let override_path = fixture
            .facts
            .layout
            .user_systemd_path("LG_Buddy_screen.service.d/config.conf");
        fs::write(
            &override_path,
            "[Service]\nEnvironment=\"LG_BUDDY_CONFIG=/tmp/other/config.env\"\n",
        )
        .unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(
            &report,
            "integration-config",
            &override_path,
            "does not reference",
        );
    }

    #[test]
    fn initial_preflight_refuses_conflicting_config_assignments() {
        let fixture = InstalledFixture::new("duplicate-config-override");
        let override_path = fixture
            .facts
            .layout
            .user_systemd_path("LG_Buddy_screen.service.d/config.conf");
        let mut contents = fs::read_to_string(&override_path).unwrap();
        contents.push_str("Environment=\"LG_BUDDY_CONFIG=/tmp/other/config.env\"\n");
        fs::write(&override_path, contents).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(
            &report,
            "integration-config",
            &override_path,
            "does not reference exactly",
        );
    }

    #[test]
    fn initial_preflight_refuses_an_unexpected_systemd_drop_in() {
        let fixture = InstalledFixture::new("unexpected-systemd-drop-in");
        let drop_in = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service.d/99-local.conf");
        write_file(&drop_in, false);

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "integration-config", &drop_in, "unexpected entry");
    }

    #[test]
    fn initial_preflight_refuses_a_missing_exact_systemd_drop_in() {
        let fixture = InstalledFixture::new("missing-systemd-drop-in");
        let drop_in = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service.d/config.conf");
        fs::remove_file(&drop_in).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(
            &report,
            "integration-config",
            &drop_in,
            "required drop-in entry is missing",
        );
    }

    #[test]
    fn initial_preflight_refuses_legacy_state_instead_of_migrating_it() {
        let fixture = InstalledFixture::new("legacy-state");
        let legacy = fixture
            .facts
            .layout
            .system_path("/usr/bin/LG_Buddy_Startup");
        write_file(&legacy, false);

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "legacy-layout", &legacy, "legacy");
    }

    #[test]
    fn initial_preflight_refuses_a_legacy_override_directory() {
        let fixture = InstalledFixture::new("legacy-override-directory");
        let legacy = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy_wake.service.d");
        let external = fixture.root.join("external-legacy-override");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("config.conf"), "external\n").unwrap();
        symlink(&external, &legacy).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "legacy-layout", &legacy, "legacy");
    }

    #[test]
    fn initial_preflight_refuses_symlinks_in_config_state() {
        let fixture = InstalledFixture::new("config-symlink");
        let link = fixture.config_directory.join("linked-token.json");
        symlink(fixture.config_directory.join("config.env"), &link).unwrap();

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert_failure(&report, "config-state", &link, "unsafe Symlink");
    }

    #[test]
    fn candidate_preflight_refuses_missing_or_non_executable_inputs() {
        let fixture = InstalledFixture::new("bad-candidate");
        let manifest = fixture.candidate_root.join("release-manifest.json");
        let installer = fixture.candidate_root.join("install.sh");
        fs::remove_file(&manifest).unwrap();
        set_executable(&installer, false);

        let report = evaluate_candidate_preflight(
            &OsFilesystemFacts,
            &fixture.candidate_root,
            fixture.facts.user_owner_uid,
        );

        assert_failure(&report, "candidate-layout", &manifest, "missing");
        assert_failure(
            &report,
            "candidate-layout",
            &installer,
            "not readable and executable",
        );
    }

    #[test]
    fn candidate_preflight_requires_owner_usable_input_modes() {
        for (label, path, mode) in [
            ("unreadable-installer", "install.sh", 0o100),
            ("other-executable", "install.sh", 0o401),
            (
                "unreadable-gui",
                "docs/lg-buddy-gui-x86_64-unknown-linux-gnu",
                0o000,
            ),
        ] {
            let fixture = InstalledFixture::new(label);
            let executable = fixture.candidate_root.join(path);
            set_mode(&executable, mode);

            let report = evaluate_candidate_preflight(
                &OsFilesystemFacts,
                &fixture.candidate_root,
                fixture.facts.user_owner_uid,
            );

            assert_failure(
                &report,
                "candidate-layout",
                &executable,
                if path == "install.sh" {
                    "not readable and executable by its owner"
                } else {
                    "not readable by its owner"
                },
            );
        }
    }

    #[test]
    fn candidate_preflight_refuses_each_missing_upgrade_input() {
        for (index, (relative, is_directory)) in
            fixture_ops::candidate_input_paths().iter().enumerate()
        {
            let fixture =
                InstalledFixture::new(&format!("missing-candidate-input-{index}"));
            let input = fixture.candidate_root.join(relative);
            if *is_directory {
                fs::remove_dir_all(&input).unwrap();
            } else {
                fs::remove_file(&input).unwrap();
            }

            let report = evaluate_candidate_preflight(
                &OsFilesystemFacts,
                &fixture.candidate_root,
                fixture.facts.user_owner_uid,
            );

            assert_failure(&report, "candidate-layout", &input, "missing");
        }
    }

    #[test]
    fn candidate_host_preflight_must_run_from_the_verified_bundle() {
        let fixture = InstalledFixture::new("wrong-candidate-runtime");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &fixture.facts,
            &fixture.candidate_root,
            false,
        );

        let failure = report
            .failures()
            .iter()
            .find(|failure| failure.check == "candidate-executable")
            .expect("candidate executable refusal");
        assert!(failure.detail.contains("candidate/lg-buddy"));
    }

    #[test]
    fn candidate_host_preflight_rechecks_installed_state() {
        let fixture = InstalledFixture::new("candidate-rechecks-installed-state");
        let installed_service = fixture
            .facts
            .layout
            .system_path("/etc/systemd/system/LG_Buddy.service");
        fs::remove_file(&installed_service).unwrap();
        let mut candidate_facts = fixture.facts.clone();
        candidate_facts.running_executable = fixture.candidate_root.join("lg-buddy");

        let report = evaluate_candidate_host_preflight(
            &OsFilesystemFacts,
            &candidate_facts,
            &fixture.candidate_root,
            false,
        );

        assert_failure(&report, "installed-layout", &installed_service, "missing");
    }

    #[test]
    fn candidate_preflight_refuses_unnormalized_relative_root() {
        let report =
            evaluate_candidate_preflight(&OsFilesystemFacts, Path::new("bundle/../next"), 1);

        let failure = report.failures().first().expect("candidate root refusal");
        assert_eq!(failure.check, "candidate-root");
        assert!(failure.detail.contains("normalized and absolute"));
    }

    #[test]
    fn initial_preflight_refuses_root_invocation() {
        let mut fixture = InstalledFixture::new("root-invocation");
        fixture.facts.effective_uid = 0;

        let report = evaluate_initial_preflight(&OsFilesystemFacts, &fixture.facts);

        assert!(report
            .failures()
            .iter()
            .any(|failure| failure.check == "invoking-user"));
    }

    fn assert_failure(report: &CompatibilityReport, check: &str, path: &Path, detail: &str) {
        assert_failure_slice(
            report.failures(),
            "report",
            check,
            path,
            detail,
        );
    }

    /// Assert a failure against a raw `Vec<CompatibilityFailure>` (from a
    /// `Checker`), with a human-readable context for the panic message.
    fn assert_failure_slice(
        failures: &[CompatibilityFailure],
        context: &str,
        check: &str,
        path: &Path,
        detail: &str,
    ) {
        let failure = failures
            .iter()
            .find(|failure| failure.check == check && failure.path.as_deref() == Some(path))
            .unwrap_or_else(|| panic!("missing {check} failure for {path:?} in {context}"));
        assert!(
            failure.detail.contains(detail),
            "expected detail {detail:?}, got {:?}",
            failure.detail
        );
        assert!(!failure.remedy.is_empty());
    }

    #[derive(Default)]
    struct OverriddenFilesystem {
        path: PathBuf,
        owner_uid: Option<u32>,
        mode: Option<u32>,
        read_only: Option<bool>,
        mount_point: Option<bool>,
    }

    // Nix sandboxes can expose `/` as unmapped uid 65534. Positive host-layout
    // tests model a conventional root without relaxing the production check.
    struct RootOwnedFilesystem<F>(F);

    impl<F: FilesystemFacts> FilesystemFacts for RootOwnedFilesystem<F> {
        fn path_facts(&self, path: &Path) -> io::Result<PathFacts> {
            let mut facts = self.0.path_facts(path)?;
            if path == Path::new("/") {
                facts.owner_uid = 0;
            }
            Ok(facts)
        }

        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            self.0.read_to_string(path)
        }

        fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            self.0.read_directory(path)
        }

        fn mount_points(&self) -> io::Result<Vec<PathBuf>> {
            self.0.mount_points()
        }
    }

    impl FilesystemFacts for OverriddenFilesystem {
        fn path_facts(&self, path: &Path) -> io::Result<PathFacts> {
            let mut facts = OsFilesystemFacts.path_facts(path)?;
            if path == self.path {
                if let Some(owner_uid) = self.owner_uid {
                    facts.owner_uid = owner_uid;
                }
                if let Some(mode) = self.mode {
                    facts.mode = mode;
                }
                if let Some(read_only) = self.read_only {
                    facts.read_only_filesystem = read_only;
                }
                if let Some(mount_point) = self.mount_point {
                    facts.mount_point = mount_point;
                }
            }
            Ok(facts)
        }

        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            OsFilesystemFacts.read_to_string(path)
        }

        fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            OsFilesystemFacts.read_directory(path)
        }

        fn mount_points(&self) -> io::Result<Vec<PathBuf>> {
            let mut mount_points = OsFilesystemFacts.mount_points()?;
            match self.mount_point {
                Some(true) if !mount_points.contains(&self.path) => {
                    mount_points.push(self.path.clone());
                }
                Some(false) => mount_points.retain(|path| path != &self.path),
                _ => {}
            }
            Ok(mount_points)
        }
    }

    struct InstalledFixture {
        root: PathBuf,
        facts: HostPreflightFacts,
        candidate_root: PathBuf,
        config_directory: PathBuf,
    }

    impl InstalledFixture {
        fn new(label: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let root = env::temp_dir().join(format!(
                "lg-buddy-upgrade-preflight-{label}-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let system_root = root.join("root");
            let user_home = root.join("home/user");
            let layout = InstalledLayout::new(&system_root, &user_home, None);
            let config_directory = user_home.join(".config/lg-buddy");
            let config_path = config_directory.join("config.env");

            fixture_ops::materialize_system_requirements(&layout);
            set_directory_tree_mode(&system_root, 0o755);
            fixture_ops::materialize_user_requirements(&layout.user_systemd_path(""));
            fs::create_dir_all(config_directory.join("tvs/primary")).unwrap();
            fs::write(&config_path, "updates_channel=stable\n").unwrap();
            fs::write(
                config_directory.join("tvs/primary/access-token.json"),
                "{}\n",
            )
            .unwrap();
            write_file(&layout.config_pointer(), false);
            fs::write(
                layout.config_pointer(),
                format!("{}\n", config_path.display()),
            )
            .unwrap();
            let config_override = format!(
                "[Service]\nEnvironment=\"LG_BUDDY_CONFIG={}\"\n",
                config_path.display()
            );
            for path in [
                layout.system_path("/etc/systemd/system/LG_Buddy.service.d/config.conf"),
                layout.system_path("/etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf"),
                layout.user_systemd_path("LG_Buddy_screen.service.d/config.conf"),
                layout.user_systemd_path("LG_Buddy_update_check.service.d/config.conf"),
            ] {
                fs::write(path, &config_override).unwrap();
            }
            set_directory_tree_mode(&user_home, 0o755);

            let candidate_root = root.join("candidate");
            fixture_ops::materialize_candidate_requirements(&candidate_root);
            set_directory_tree_mode(&candidate_root, 0o755);
            set_mode(&root, 0o755);

            let owner_uid = unsafe { libc::geteuid() };
            let facts = HostPreflightFacts {
                running_executable: layout.installed_executable(),
                layout,
                effective_uid: if owner_uid == 0 { 1000 } else { owner_uid },
                system_owner_uid: owner_uid,
                user_owner_uid: owner_uid,
                service_managers: ServiceManagerFacts::skipped(),
            };

            Self {
                root,
                facts,
                candidate_root,
                config_directory,
            }
        }
    }

    impl Drop for InstalledFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_file(path: &Path, executable: bool) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"fixture\n").unwrap();
        set_executable(path, executable);
    }

    fn set_directory_tree_mode(path: &Path, mode: u32) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                set_directory_tree_mode(&entry.path(), mode);
            }
        }
        set_mode(path, mode);
    }

    fn set_executable(path: &Path, executable: bool) {
        let mode = if executable { 0o755 } else { 0o644 };
        set_mode(path, mode);
    }

    fn set_mode(path: &Path, mode: u32) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn service_manager_refusal_decides_usability() {
        fn reported(state: &str) -> SystemdManagerObservation {
            SystemdManagerObservation::Reported {
                state: state.to_string(),
                stderr: "systemctl stderr".to_string(),
            }
        }
        assert_eq!(service_manager_refusal(&reported("running")), None);
        assert_eq!(service_manager_refusal(&reported("degraded")), None);
        assert_eq!(
            service_manager_refusal(&reported("")),
            Some("systemctl did not report a usable manager state (systemctl stderr)".to_string())
        );
        assert_eq!(
            service_manager_refusal(&reported("offline")),
            Some("systemd manager state is offline".to_string())
        );
        assert_eq!(
            service_manager_refusal(&SystemdManagerObservation::ProbeFailed(
                "could not run systemctl: missing".to_string()
            )),
            Some("could not run systemctl: missing".to_string())
        );
        assert_eq!(
            service_manager_refusal(&SystemdManagerObservation::Skipped),
            None
        );
    }
}
