use super::*;
use crate::settings::{UserServiceState, UserUnitEnableOutcome};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    config: PathBuf,
    units: PathBuf,
    user: RefCell<BTreeMap<String, (bool, bool)>>,
    system: Cell<bool>,
    active: Cell<bool>,
    calls: RefCell<Vec<String>>,
    error: RefCell<Option<SettingsError>>,
    broken_repair: Cell<bool>,
    user_binding: Cell<bool>,
    running_user_binding: Cell<bool>,
    system_binding: Cell<bool>,
    fail_after_reload: Cell<bool>,
    fail_stop: Cell<bool>,
    fail_state: Cell<bool>,
    fail_start: Cell<bool>,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-provision-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config with % and quotes'.env");
        fs::write(
            &config,
            "updates_auto_check=disabled\nscreen_idle_blank=enabled\n",
        )
        .unwrap();
        Self {
            units: root.join("user/systemd"),
            root,
            config,
            user: RefCell::new(BTreeMap::new()),
            system: Cell::new(false),
            active: Cell::new(false),
            calls: RefCell::new(Vec::new()),
            error: RefCell::new(None),
            broken_repair: Cell::new(false),
            user_binding: Cell::new(true),
            running_user_binding: Cell::new(false),
            system_binding: Cell::new(true),
            fail_after_reload: Cell::new(false),
            fail_stop: Cell::new(false),
            fail_state: Cell::new(false),
            fail_start: Cell::new(false),
        }
    }
    fn plan(&self) -> ServiceInstallation<'_, Self> {
        ServiceInstallation {
            config: &self.config,
            user_units: &self.units,
            system_root: &self.root,
            controller: self,
            authorization: crate::setup::flow::AuthorizationMode::Noninteractive,
        }
    }
    fn run(&self) -> StepResponse {
        self.plan()
            .execute(&StepCancellation::default(), &mut |_| {})
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
impl ServiceController for Fixture {
    fn user_service_state(&self, unit: &str) -> Result<UserServiceState, SettingsError> {
        if self.fail_state.get() {
            return Err(io_error("service inspection timed out"));
        }
        if !self.user.borrow().contains_key(unit) {
            return Ok(UserServiceState::Missing);
        }
        Ok(
            if self.user_unit_is_enabled(unit)? || self.user_service_is_active(unit)? {
                UserServiceState::ActiveOrEnabled
            } else {
                UserServiceState::InactiveDisabled
            },
        )
    }
    fn user_service_config_path(&self, _: &str) -> Result<PathBuf, SettingsError> {
        if !self.user_binding.get() {
            return Err(io_error("no loaded binding"));
        }
        Ok(self.config.clone())
    }
    fn system_service_config_path(&self, _: &str) -> Result<PathBuf, SettingsError> {
        if !self.system_binding.get() {
            return Ok(self.root.join("old-config.env"));
        }
        Ok(self.config.clone())
    }
    fn user_unit_is_enabled(&self, unit: &str) -> Result<bool, SettingsError> {
        Ok(self.user.borrow().get(unit).map(|s| s.1).unwrap_or(false))
    }
    fn user_service_is_active(&self, unit: &str) -> Result<bool, SettingsError> {
        Ok(self.user.borrow().get(unit).map(|s| s.0).unwrap_or(false))
    }
    fn restart_user_service(&self, unit: &str) -> Result<(), SettingsError> {
        self.calls.borrow_mut().push(format!("restart {unit}"));
        if self.fail_start.get() {
            return Err(io_error("service start failed"));
        }
        self.user.borrow_mut().entry(unit.into()).or_default().0 = true;
        self.running_user_binding.set(self.user_binding.get());
        Ok(())
    }
    fn stop_user_service(&self, unit: &str) -> Result<(), SettingsError> {
        self.calls.borrow_mut().push(format!("stop {unit}"));
        if self.fail_stop.get() {
            return Err(io_error("service stop failed"));
        }
        self.user.borrow_mut().entry(unit.into()).or_default().0 = false;
        Ok(())
    }
    fn enable_start_user_unit(&self, unit: &str) -> Result<UserUnitEnableOutcome, SettingsError> {
        self.calls.borrow_mut().push(format!("enable {unit}"));
        if self.fail_start.get() {
            return Err(io_error("service start failed"));
        }
        // `start` on an already-active unit leaves the existing process alone.
        if unit == SCREEN && !self.user_service_is_active(unit)? {
            self.running_user_binding.set(self.user_binding.get());
        }
        self.user.borrow_mut().insert(unit.into(), (true, true));
        Ok(UserUnitEnableOutcome::EnabledStarted)
    }
    fn disable_stop_user_unit(&self, unit: &str) -> Result<(), SettingsError> {
        self.calls.borrow_mut().push(format!("disable {unit}"));
        self.user.borrow_mut().insert(unit.into(), (false, false));
        Ok(())
    }
    fn reload_user_units(&self) -> Result<(), SettingsError> {
        self.calls.borrow_mut().push("reload".into());
        self.user_binding.set(true);
        if self.fail_after_reload.replace(false) {
            return Err(io_error("interrupted after manager reload"));
        }
        Ok(())
    }
    fn system_unit_is_enabled(&self, _: &str) -> Result<bool, SettingsError> {
        Ok(self.system.get())
    }
    fn system_lifecycle_is_active(&self) -> Result<bool, SettingsError> {
        Ok(self.active.get())
    }
    fn repair_system_services(
        &self,
        config: &Path,
        authorization: crate::setup::flow::AuthorizationMode,
    ) -> Result<(), SettingsError> {
        assert_eq!(config, self.config);
        assert_eq!(
            authorization,
            crate::setup::flow::AuthorizationMode::Noninteractive
        );
        self.calls.borrow_mut().push("repair-system".into());
        if let Some(error) = self.error.borrow_mut().take() {
            return Err(error);
        }
        if !self.broken_repair.get() {
            for path in LEGACY_HANDLERS {
                match fs::remove_file(self.root.join(path)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io_error(error)),
                }
            }
            for spec in self.plan().system_files()? {
                write_file(&spec)?;
            }
        }
        self.system.set(true);
        self.active.set(true);
        self.system_binding.set(true);
        Ok(())
    }
}
#[test]
fn fresh_setup_and_partial_repair_are_verified_and_idempotent() {
    let fixture = Fixture::new();
    let original = fs::read(&fixture.config).unwrap();
    assert!(matches!(
        fixture.plan().inspect(),
        StepResponse::ActionRequired {
            requires_authorization: true,
            ..
        }
    ));
    assert!(fixture.calls.borrow().is_empty());
    assert!(!fixture.units.exists());
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert!(fixture.user_service_is_active(SCREEN).unwrap());
    assert!(!fixture.user_service_is_active(TIMER).unwrap());
    assert_eq!(fs::read(&fixture.config).unwrap(), original);
    let modified = fs::metadata(fixture.units.join(SCREEN))
        .unwrap()
        .modified()
        .unwrap();
    fixture.calls.borrow_mut().clear();
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert!(fixture.calls.borrow().is_empty());
    assert_eq!(
        fs::metadata(fixture.units.join(SCREEN))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    fs::remove_file(
        fixture
            .root
            .join("etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle"),
    )
    .unwrap();
    assert!(matches!(
        fixture.plan().inspect(),
        StepResponse::ActionRequired {
            requires_authorization: true,
            ..
        }
    ));
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert!(fixture.calls.borrow().contains(&"repair-system".into()));
}
#[test]
fn legacy_handlers_require_verified_cleanup_even_when_current_services_are_ready() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(), StepResponse::Complete);
    let original = fs::read(&fixture.config).unwrap();
    for relative in LEGACY_HANDLERS {
        // A dangling link must be detected just like a regular legacy file.
        for symlink in [false, true] {
            let path = fixture.root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if symlink {
                std::os::unix::fs::symlink(fixture.root.join("removed-handler"), &path).unwrap();
            } else {
                fs::write(&path, "legacy handler").unwrap();
            }
            fixture.calls.borrow_mut().clear();
            assert!(matches!(
                fixture.plan().inspect(),
                StepResponse::ActionRequired {
                    requires_authorization: true,
                    ..
                }
            ));
            assert!(fixture.calls.borrow().is_empty());
            fixture.broken_repair.set(true);
            assert!(matches!(fixture.run(), StepResponse::Failed(_)));
            assert!(fs::symlink_metadata(&path).is_ok());
            fixture.broken_repair.set(false);
            assert_eq!(fixture.run(), StepResponse::Complete);
            assert!(fs::symlink_metadata(&path).is_err());
            fixture.calls.borrow_mut().clear();
            assert_eq!(fixture.run(), StepResponse::Complete);
            assert!(fixture.calls.borrow().is_empty());
        }
    }
    assert_eq!(fs::read(&fixture.config).unwrap(), original);
}
#[test]
fn timer_follows_desired_setting_without_authorization_or_triggered_service_activity() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(), StepResponse::Complete);
    for enabled in [true, false] {
        fs::write(
            &fixture.config,
            format!(
                "updates_auto_check={}\n",
                if enabled { "enabled" } else { "disabled" }
            ),
        )
        .unwrap();
        fixture.calls.borrow_mut().clear();
        assert!(matches!(
            fixture.plan().inspect(),
            StepResponse::ActionRequired {
                requires_authorization: false,
                ..
            }
        ));
        assert_eq!(fixture.run(), StepResponse::Complete);
        assert_eq!(fixture.user_unit_is_enabled(TIMER).unwrap(), enabled);
        assert_eq!(fixture.user_service_is_active(TIMER).unwrap(), enabled);
        assert!(!fixture.calls.borrow().contains(&"repair-system".into()));
        assert!(!fixture.calls.borrow().contains(&format!("stop {SCREEN}")));
        assert!(!fixture
            .calls
            .borrow()
            .contains(&format!("restart {SCREEN}")));
        fixture.calls.borrow_mut().clear();
        assert_eq!(fixture.run(), StepResponse::Complete);
        assert!(fixture.calls.borrow().is_empty());
    }
}
#[test]
fn active_but_disabled_services_are_repaired() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(), StepResponse::Complete);
    fixture
        .user
        .borrow_mut()
        .insert(SCREEN.into(), (true, false));
    fixture.system.set(false);
    assert!(matches!(
        fixture.plan().inspect(),
        StepResponse::ActionRequired { .. }
    ));
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert!(fixture.system.get());
    assert!(fixture.user_unit_is_enabled(SCREEN).unwrap());
}
#[test]
fn wrong_bindings_and_missing_user_files_are_repaired_without_changing_config() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(), StepResponse::Complete);
    fs::write(
        fixture.units.join("LG_Buddy_screen.service.d/config.conf"),
        "wrong binding",
    )
    .unwrap();
    fs::remove_file(fixture.units.join(TIMER)).unwrap();
    fixture.calls.borrow_mut().clear();
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert!(!fixture.calls.borrow().contains(&"repair-system".into()));
    assert!(fixture.calls.borrow().contains(&format!("stop {SCREEN}")));
    assert!(fixture.running_user_binding.get());
}
#[test]
fn cancellation_failure_and_failed_verification_do_not_report_completion() {
    let fixture = Fixture::new();
    let cancellation = StepCancellation::default();
    assert!(cancellation.cancel());
    assert_eq!(
        fixture
            .plan()
            .execute(&cancellation, &mut |_| panic!("cancelled")),
        StepResponse::Cancelled
    );
    assert!(fixture.calls.borrow().is_empty());
    *fixture.error.borrow_mut() = Some(SettingsError::ActivationCancelled);
    assert_eq!(fixture.run(), StepResponse::Cancelled);
    assert!(!fixture.units.exists());
    *fixture.error.borrow_mut() = Some(io_error("private diagnostic"));
    let StepResponse::Failed(error) = fixture.run() else {
        panic!("expected normalized failure")
    };
    assert!(error.diagnostic.contains("private diagnostic"));
    assert!(!error.presentation.detail().contains("private diagnostic"));
    fixture.broken_repair.set(true);
    assert!(matches!(fixture.run(), StepResponse::Failed(_)));
    fixture.broken_repair.set(false);
    assert_eq!(fixture.run(), StepResponse::Complete);
}
#[test]
fn authorization_denial_explains_recovery_without_losing_completed_setup() {
    let fixture = Fixture::new();
    let original = fs::read(&fixture.config).unwrap();
    *fixture.error.borrow_mut() = Some(SettingsError::AuthorizationFailed {
        message: "pkexec exited with status 127: private diagnostic".into(),
    });
    let StepResponse::Failed(error) = fixture.run() else {
        panic!("denial must remain retryable, not silently cancel the flow");
    };
    assert_eq!(
        error.presentation.summary(),
        "Administrator permission wasn't granted"
    );
    assert!(error.presentation.detail().contains("Retry"));
    assert!(!error.presentation.detail().contains("private diagnostic"));
    assert!(error.diagnostic.contains("pkexec exited with status 127"));
    assert!(error.retryable);
    assert_eq!(fs::read(&fixture.config).unwrap(), original);
    assert!(!fixture.units.exists());
    assert_eq!(fixture.run(), StepResponse::Complete);
}

#[test]
fn repair_rejects_cancellation_after_execution_begins() {
    let fixture = Fixture::new();
    let cancellation = StepCancellation::default();
    assert_eq!(
        fixture.plan().execute(&cancellation, &mut |state| {
            assert!(matches!(
                state,
                StepResponse::Running {
                    cancelable: false,
                    ..
                }
            ));
            assert!(!cancellation.cancel());
        }),
        StepResponse::Complete
    );
}

#[test]
fn stale_loaded_bindings_are_repaired_even_when_files_match() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(), StepResponse::Complete);
    fixture.user_binding.set(false);
    fixture.running_user_binding.set(false);
    fixture.calls.borrow_mut().clear();
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert!(fixture.calls.borrow().contains(&format!("stop {SCREEN}")));
    assert!(fixture.running_user_binding.get());
    fixture.system_binding.set(false);
    fixture.calls.borrow_mut().clear();
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert!(fixture.calls.borrow().contains(&"repair-system".into()));
    fixture.calls.borrow_mut().clear();
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert!(fixture.calls.borrow().is_empty());
}

#[test]
fn unsupported_installation_is_blocked_without_mutation() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("etc")).unwrap();
    fs::write(fixture.root.join("etc/NIXOS"), "").unwrap();
    assert!(matches!(fixture.run(), StepResponse::Blocked(_)));
    assert!(fixture.calls.borrow().is_empty());
    assert!(!fixture.units.exists());
}

#[test]
fn invalid_update_preference_fails_before_any_mutation() {
    for installed in [false, true] {
        let fixture = Fixture::new();
        fs::write(&fixture.config, "updates_auto_check=enabled\n").unwrap();
        if installed {
            assert_eq!(fixture.run(), StepResponse::Complete);
        }
        fixture.calls.borrow_mut().clear();
        fs::write(&fixture.config, "updates_auto_check=typo\n").unwrap();

        for response in [fixture.plan().inspect(), fixture.run()] {
            let StepResponse::Failed(failure) = response else {
                panic!("invalid preference must fail: {response:?}");
            };
            assert!(failure.diagnostic.contains("updates.auto_check"));
        }
        assert!(fixture.plan().repair().is_err());
        assert!(fixture.calls.borrow().is_empty());
        assert_eq!(fixture.units.exists(), installed);
        assert_eq!(fixture.user_service_is_active(TIMER).unwrap(), installed);
        assert_eq!(fixture.user_unit_is_enabled(TIMER).unwrap(), installed);
        assert_eq!(
            fs::read_to_string(&fixture.config).unwrap(),
            "updates_auto_check=typo\n"
        );
    }
}

#[test]
fn interrupted_reconfiguration_remains_incomplete_until_new_process_starts() {
    for file_changed in [false, true] {
        let fixture = Fixture::new();
        assert_eq!(fixture.run(), StepResponse::Complete);
        fixture.user_binding.set(false);
        fixture.running_user_binding.set(false);
        if file_changed {
            fs::write(
                fixture.units.join("LG_Buddy_screen.service.d/config.conf"),
                "old binding",
            )
            .unwrap();
        }
        fixture.calls.borrow_mut().clear();
        fixture.fail_after_reload.set(true);
        assert!(matches!(fixture.run(), StepResponse::Failed(_)));
        assert_eq!(
            *fixture.calls.borrow(),
            [format!("stop {SCREEN}"), "reload".into()]
        );
        assert!(files_match(&fixture.plan().user_files().unwrap()).unwrap());
        assert!(fixture.user_binding.get());
        assert!(!fixture.user_service_is_active(SCREEN).unwrap());
        assert!(!fixture.running_user_binding.get());
        assert!(matches!(
            fixture.plan().inspect(),
            StepResponse::ActionRequired {
                requires_authorization: false,
                ..
            }
        ));

        // A failed start must not erase the unfinished repair either.
        fixture.fail_start.set(true);
        assert!(matches!(fixture.run(), StepResponse::Failed(_)));
        assert!(!fixture.user_service_is_active(SCREEN).unwrap());
        fixture.fail_start.set(false);
        assert_eq!(fixture.run(), StepResponse::Complete);
        assert!(fixture.running_user_binding.get());
        fixture.calls.borrow_mut().clear();
        assert_eq!(fixture.run(), StepResponse::Complete);
        assert!(fixture.calls.borrow().is_empty());
    }
}

#[test]
fn failed_state_query_does_not_rewrite_or_reload_an_active_service() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(), StepResponse::Complete);
    let binding = fixture.units.join("LG_Buddy_screen.service.d/config.conf");
    fs::write(&binding, "old binding").unwrap();
    fixture.calls.borrow_mut().clear();
    fixture.fail_state.set(true);

    let StepResponse::Failed(error) = fixture.run() else {
        panic!("expected a failed inspection");
    };
    assert!(error.diagnostic.contains("service inspection timed out"));
    assert!(fixture.calls.borrow().is_empty());
    assert_eq!(fs::read_to_string(&binding).unwrap(), "old binding");
    assert!(fixture.user_service_is_active(SCREEN).unwrap());

    fixture.fail_state.set(false);
    assert_eq!(fixture.run(), StepResponse::Complete);
    assert_eq!(fixture.calls.borrow()[0], format!("stop {SCREEN}"));
}

#[test]
fn failed_stop_does_not_rewrite_or_reload_an_active_service() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run(), StepResponse::Complete);
    let binding = fixture.units.join("LG_Buddy_screen.service.d/config.conf");
    fs::write(&binding, "old binding").unwrap();
    fixture.calls.borrow_mut().clear();
    fixture.fail_stop.set(true);
    assert!(matches!(fixture.run(), StepResponse::Failed(_)));
    assert_eq!(*fixture.calls.borrow(), [format!("stop {SCREEN}")]);
    assert_eq!(fs::read_to_string(binding).unwrap(), "old binding");
    assert!(fixture.user_service_is_active(SCREEN).unwrap());
}

fn native_steps(fixture: Fixture, plasma: bool) -> crate::setup::environment::NativeSteps<Fixture> {
    use crate::setup::{
        environment::{NativeSteps, SetupContext},
        flow::AuthorizationMode,
    };
    let helper = fixture.root.join("kwin.sh");
    fs::write(
        fixture.root.join("kwin-status"),
        if plasma { "3" } else { "2" },
    )
    .unwrap();
    fs::write(
        &helper,
        r#"#!/bin/bash
cd -- "$(dirname -- "$0")"
[ "$1" != --status ] || exit "$(cat kwin-status)"
printf '%s\n' "$*" >> kwin-actions
[[ "$*" == *--noninteractive* ]] || exit 1
[[ "$*" == *--allow-dependencies* ]] || exit 77
echo 0 > kwin-status
"#,
    )
    .unwrap();
    let lock_path = fixture.root.join("flow.lock");
    let context = SetupContext {
        config: fixture.config.clone(),
        user_units: fixture.units.clone(),
        system_root: fixture.root.clone(),
        kwin_helper: helper,
        lock_path: lock_path.clone(),
        authorization: AuthorizationMode::Noninteractive,
    };
    NativeSteps {
        context,
        controller: fixture,
    }
}

fn native_flow(fixture: Fixture, plasma: bool) -> crate::setup::flow::OnboardingFlow {
    let steps = native_steps(fixture, plasma);
    let lock = steps.context.lock_path.clone();
    crate::setup::flow::OnboardingFlow::with_backend(Box::new(steps), &lock).unwrap()
}

#[test]
fn native_flow_composes_pairing_service_repair_and_plasma_dependency_consent() {
    use crate::{
        config::HdmiInput,
        pairing::{PairingOperation, PairingRequest},
        platform_access_token::PlatformAccessToken,
        setup::flow::{FlowOutcome, SetupStep, StepAnswer},
    };
    for plasma in [false, true] {
        let fixture = Fixture::new();
        let config = fixture.config.clone();
        let root = fixture.root.clone();
        let user_files = fixture.plan().user_files().unwrap();
        let system_files = fixture.plan().system_files().unwrap();
        let mut flow = native_flow(fixture, plasma);
        assert_eq!(flow.snapshot().current().unwrap().0, SetupStep::Pairing);
        // A missing TV cannot be bypassed by a generic Continue answer.
        let snapshot = flow.advance(flow.snapshot().token, StepAnswer::Continue, &mut |_| {});
        assert!(matches!(
            snapshot.current().unwrap().1,
            StepResponse::Failed(_)
        ));
        assert!(!root.join("etc/systemd").exists());
        let request =
            PairingRequest::parse("192.0.2.42", "02:11:22:33:44:55", HdmiInput::Hdmi1).unwrap();
        crate::pairing::pair_and_save(
            &PairingOperation::for_setup(request, StepCancellation::default()),
            &config,
            &mut |_| {},
            |_| Ok(PlatformAccessToken::new("fixture-token").unwrap()),
        )
        .unwrap();
        let saved = fs::read(&config).unwrap();
        // A changed prerequisite replans first instead of applying a stale answer.
        let snapshot = flow.advance(snapshot.token, StepAnswer::Continue, &mut |_| {});
        assert_eq!(snapshot.current().unwrap().0, SetupStep::Services);
        assert!(!root.join("etc/systemd").exists());
        let snapshot = flow.advance(snapshot.token, StepAnswer::Continue, &mut |_| {});
        assert!(files_match(&user_files).unwrap());
        assert!(files_match(&system_files).unwrap());
        if plasma {
            assert_eq!(snapshot.current().unwrap().0, SetupStep::Plasma);
            let snapshot = flow.advance(snapshot.token, StepAnswer::Continue, &mut |_| {});
            assert!(matches!(
                snapshot.current().unwrap().1,
                StepResponse::InputRequired(crate::setup::StepInput::BuildDependencies { .. })
            ));
            let snapshot = flow.advance(
                snapshot.token,
                StepAnswer::InstallBuildDependencies,
                &mut |_| {},
            );
            assert_eq!(snapshot.outcome, FlowOutcome::Complete);
            assert_eq!(
                fs::read_to_string(root.join("kwin-actions"))
                    .unwrap()
                    .lines()
                    .count(),
                2
            );
        } else {
            assert_eq!(snapshot.outcome, FlowOutcome::Complete);
            assert!(!root.join("kwin-actions").exists());
        }
        assert_eq!(fs::read(&config).unwrap(), saved);
    }
}

#[test]
fn native_pairing_adapter_uses_the_flows_live_cancellation_gate() {
    use crate::{
        config::HdmiInput,
        pairing::PairingRequest,
        setup::flow::{FlowOutcome, StepAnswer},
    };
    let fixture = Fixture::new();
    let config = fixture.config.clone();
    let original = fs::read(&config).unwrap();
    let mut flow = native_flow(fixture, false);
    let cancellation = flow.cancellation();
    let request =
        PairingRequest::parse("192.0.2.42", "02:11:22:33:44:55", HdmiInput::Hdmi1).unwrap();
    let snapshot = flow.advance(
        flow.snapshot().token,
        StepAnswer::Pairing(request),
        &mut |_| {
            assert!(cancellation.cancel());
        },
    );
    assert_eq!(snapshot.outcome, FlowOutcome::Cancelled);
    assert_eq!(fs::read(config).unwrap(), original);
}

#[test]
fn terminal_renderer_repairs_native_steps_and_repeat_preserves_files() {
    use crate::{parse_args, setup::cli, Command, ParseOutcome};
    let fixture = Fixture::new();
    let config = fixture.config.clone();
    let units = fixture.units.clone();
    let root = fixture.root.clone();
    // Existing TV, with valid local credentials: terminal setup must skip pairing.
    fs::write(&config, "tvs_primary_ip=192.0.2.1\ntvs_primary_mac=02:11:22:33:44:55\ntvs_primary_input=HDMI_1\ntvs_primary_platform=lg_webos\nupdates_auto_check=disabled\nscreen_idle_blank=disabled\n").unwrap();
    let token = config
        .parent()
        .unwrap()
        .join("tvs/primary/access-token.json");
    fs::create_dir_all(token.parent().unwrap()).unwrap();
    fs::write(&token, "{\"access_token\":\"stored-token\"}").unwrap();
    for relative in LEGACY_HANDLERS {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "legacy handler").unwrap();
    }
    let original = fs::read(&config).unwrap();
    let mut flow = native_flow(fixture, true);
    let ParseOutcome::Command(Command::Setup(options)) = parse_args([
        "setup",
        "--non-interactive",
        "--yes",
        "--allow-build-dependencies",
    ])
    .unwrap() else {
        panic!()
    };
    let mut output = Vec::new();
    cli::render(
        &mut flow,
        &options,
        false,
        &mut std::io::empty(),
        &mut output,
    )
    .unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .ends_with("Setup complete.\n"));
    assert_eq!(fs::read(&config).unwrap(), original);
    assert!(units.join(SCREEN).exists());
    for relative in LEGACY_HANDLERS {
        assert!(!root.join(relative).exists());
    }
    let unit_time = fs::metadata(units.join(SCREEN))
        .unwrap()
        .modified()
        .unwrap();
    let actions = fs::read(root.join("kwin-actions")).unwrap();
    cli::render(
        &mut flow,
        &options,
        false,
        &mut std::io::empty(),
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(units.join(SCREEN))
            .unwrap()
            .modified()
            .unwrap(),
        unit_time
    );
    assert_eq!(fs::read(root.join("kwin-actions")).unwrap(), actions);
    assert_eq!(fs::read(config).unwrap(), original);
}

#[test]
fn native_assessment_tracks_repairs_removals_and_desktop_changes_without_mutation() {
    use crate::setup::{
        assessment::{assess_steps, SetupStatus},
        lock::FlowLock,
    };
    let fixture = Fixture::new();
    fs::write(&fixture.config, "tvs_primary_ip=192.0.2.1\ntvs_primary_mac=02:11:22:33:44:55\ntvs_primary_input=HDMI_1\ntvs_primary_platform=lg_webos\nupdates_auto_check=disabled\nscreen_idle_blank=disabled\n").unwrap();
    let token = fixture.root.join("tvs/primary/access-token.json");
    fs::create_dir_all(token.parent().unwrap()).unwrap();
    fs::write(&token, "{\"access_token\":\"stored-token\"}").unwrap();
    let steps = native_steps(fixture, false);
    let fixture = &steps.controller;
    // Inspection is usable even with an open flow and never takes its lock.
    let _flow = FlowLock::acquire(&steps.context.lock_path).unwrap();
    let original = fs::read(&fixture.config).unwrap();
    let check = |expected| {
        fixture.calls.borrow_mut().clear();
        assert_eq!(assess_steps(&steps).status(), expected);
        assert!(fixture.calls.borrow().is_empty());
        assert!(!fixture.root.join("kwin-actions").exists());
        assert_eq!(fs::read(&fixture.config).unwrap(), original);
    };
    check(SetupStatus::Incomplete);
    assert!(!fixture.units.exists());
    assert_eq!(fixture.run(), StepResponse::Complete);
    let unit = fixture.units.join(SCREEN);
    let modified = fs::metadata(&unit).unwrap().modified().unwrap();
    check(SetupStatus::Complete);
    assert_eq!(fs::metadata(&unit).unwrap().modified().unwrap(), modified);
    // A later Plasma session requires its currently loaded bridge.
    fs::write(fixture.root.join("kwin-status"), "3").unwrap();
    check(SetupStatus::Incomplete);
    fs::write(fixture.root.join("kwin-status"), "0").unwrap();
    check(SetupStatus::Complete);
    // Stopped common services and removed files are discovered afresh.
    fixture.user.borrow_mut().get_mut(SCREEN).unwrap().0 = false;
    check(SetupStatus::Incomplete);
    assert_eq!(fixture.run(), StepResponse::Complete);
    check(SetupStatus::Complete);
    fs::remove_file(&unit).unwrap();
    check(SetupStatus::Incomplete);
    assert_eq!(fixture.run(), StepResponse::Complete);
    check(SetupStatus::Complete);
    fs::remove_file(&token).unwrap();
    check(SetupStatus::Incomplete);
}
