use super::*;
use crate::settings::UserUnitEnableOutcome;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-service-step-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config.env");
        fs::write(
            &config,
            "screen_idle_blank=enabled\nsystem_sleep_wake_policy=enabled\n",
        )
        .unwrap();
        for (name, contents) in [
            (INSTALL_CONFIG_POINTER, config.to_str().unwrap()),
            (LIFECYCLE_UNIT, "[Service]\n"),
            (NETWORK_MANAGER_HOOK, "#!/bin/sh\n"),
        ] {
            let path = prefixed_path(Some(&root), name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        Self { root, config }
    }

    fn inspect(&self, step: ServiceStep, services: &FakeServices) -> StepResponse {
        step.inspect(&self.config, Some(&self.root), services)
    }

    fn execute(&self, step: ServiceStep, services: &FakeServices) -> StepResponse {
        step.execute(
            &self.config,
            Some(&self.root),
            services,
            &StepCancellation::default(),
            &mut |_| {},
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct FakeServices {
    config: RefCell<PathBuf>,
    installed: bool,
    active: Cell<bool>,
    enable_starts: bool,
    start_succeeds: bool,
    inspection_fails: bool,
    actions_disabled: bool,
    action_error: RefCell<Option<SettingsError>>,
    actions: RefCell<Vec<&'static str>>,
}

impl FakeServices {
    fn new(fixture: &Fixture) -> Self {
        Self {
            config: RefCell::new(fixture.config.clone()),
            installed: true,
            active: Cell::new(false),
            enable_starts: true,
            start_succeeds: true,
            inspection_fails: false,
            actions_disabled: false,
            action_error: RefCell::new(None),
            actions: RefCell::new(Vec::new()),
        }
    }

    fn observe(&self) -> Result<bool, SettingsError> {
        if self.inspection_fails {
            Err(SettingsError::Apply {
                message: "private bus error details".into(),
            })
        } else {
            Ok(self.active.get())
        }
    }

    fn start(&self, action: &'static str, activates: bool) -> Result<(), SettingsError> {
        self.actions.borrow_mut().push(action);
        if let Some(error) = self.action_error.borrow_mut().take() {
            return Err(error);
        }
        if activates && self.start_succeeds {
            self.active.set(true);
        }
        Ok(())
    }
}

impl ServiceController for FakeServices {
    fn systemd_actions_disabled(&self) -> bool {
        self.actions_disabled
    }

    fn user_service_state(&self, _: &str) -> Result<UserServiceState, SettingsError> {
        let active = self.observe()?;
        Ok(if !self.installed {
            UserServiceState::Missing
        } else if active {
            UserServiceState::ActiveOrEnabled
        } else {
            UserServiceState::InactiveDisabled
        })
    }

    fn user_service_config_path(&self, _: &str) -> Result<PathBuf, SettingsError> {
        Ok(self.config.borrow().clone())
    }

    fn user_service_is_active(&self, _: &str) -> Result<bool, SettingsError> {
        self.observe()
    }

    fn restart_user_service(&self, _: &str) -> Result<(), SettingsError> {
        self.start("restart-screen", true)
    }

    fn enable_start_user_unit(&self, _: &str) -> Result<UserUnitEnableOutcome, SettingsError> {
        self.start("enable-start-screen", self.enable_starts)?;
        Ok(if self.enable_starts {
            UserUnitEnableOutcome::EnabledStarted
        } else {
            UserUnitEnableOutcome::Enabled
        })
    }

    fn disable_stop_user_unit(&self, _: &str) -> Result<(), SettingsError> {
        panic!("setup must not disable a service")
    }

    fn system_lifecycle_is_active(&self) -> Result<bool, SettingsError> {
        self.observe()
    }

    fn start_system_lifecycle(&self) -> Result<(), SettingsError> {
        self.start("authorize-start-lifecycle", true)
    }
}

#[test]
fn inspection_reports_work_without_activating_or_authorizing() {
    let fixture = Fixture::new();
    let services = FakeServices::new(&fixture);
    for (step, authorization) in [(ServiceStep::Screen, false), (ServiceStep::Lifecycle, true)] {
        assert!(matches!(fixture.inspect(step, &services),
            StepResponse::ActionRequired { requires_authorization, .. } if requires_authorization == authorization));
    }
    assert!(services.actions.borrow().is_empty());
}

#[test]
fn service_steps_are_idempotent_and_preserve_desired_settings() {
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        let fixture = Fixture::new();
        let services = FakeServices::new(&fixture);
        let config = fs::read(&fixture.config).unwrap();
        assert_eq!(fixture.execute(step, &services), StepResponse::Complete);
        assert!(services.active.get());
        assert_eq!(fixture.inspect(step, &services), StepResponse::Complete);
        assert_eq!(services.actions.borrow().len(), 1);
        assert_eq!(fixture.execute(step, &services), StepResponse::Complete);
        assert_eq!(
            services.actions.borrow().len(),
            1,
            "no second start or authorization"
        );
        assert_eq!(fs::read(&fixture.config).unwrap(), config);
    }
}

#[test]
fn already_active_services_need_no_mutation() {
    let fixture = Fixture::new();
    let services = FakeServices::new(&fixture);
    services.active.set(true);
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        assert_eq!(fixture.inspect(step, &services), StepResponse::Complete);
        assert_eq!(fixture.execute(step, &services), StepResponse::Complete);
    }
    assert!(services.actions.borrow().is_empty());
}

#[test]
fn checks_report_actual_readiness_even_when_mutations_are_disabled() {
    let fixture = Fixture::new();
    let services = FakeServices {
        actions_disabled: true,
        ..FakeServices::new(&fixture)
    };
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        services.active.set(false);
        assert!(matches!(
            fixture.inspect(step, &services),
            StepResponse::Blocked(_)
        ));
        assert!(matches!(
            fixture.execute(step, &services),
            StepResponse::Blocked(_)
        ));
        services.active.set(true);
        assert_eq!(fixture.inspect(step, &services), StepResponse::Complete);
        assert_eq!(fixture.execute(step, &services), StepResponse::Complete);
    }
    assert!(services.actions.borrow().is_empty());
}

#[test]
fn execution_rechecks_current_state_instead_of_replaying_an_old_plan() {
    let fixture = Fixture::new();
    let services = FakeServices::new(&fixture);
    assert!(matches!(
        fixture.inspect(ServiceStep::Screen, &services),
        StepResponse::ActionRequired { .. }
    ));
    services.active.set(true);
    assert_eq!(
        fixture.execute(ServiceStep::Screen, &services),
        StepResponse::Complete
    );
    assert!(services.actions.borrow().is_empty());
}

#[test]
fn missing_installation_is_blocked_before_any_mutation() {
    let fixture = Fixture::new();
    let services = FakeServices {
        installed: false,
        ..FakeServices::new(&fixture)
    };
    fs::remove_file(prefixed_path(Some(&fixture.root), LIFECYCLE_UNIT)).unwrap();
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        assert!(matches!(
            fixture.execute(step, &services),
            StepResponse::Blocked(_)
        ));
    }
    assert!(services.actions.borrow().is_empty());
}

#[test]
fn mismatched_configuration_is_never_activated() {
    let fixture = Fixture::new();
    let services = FakeServices::new(&fixture);
    let other = fixture.root.join("other.env");
    fs::write(&other, "").unwrap();
    *services.config.borrow_mut() = other.clone();
    fs::write(
        prefixed_path(Some(&fixture.root), INSTALL_CONFIG_POINTER),
        other.to_str().unwrap(),
    )
    .unwrap();
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        assert!(matches!(
            fixture.execute(step, &services),
            StepResponse::Failed(_) | StepResponse::Blocked(_)
        ));
    }
    assert!(services.actions.borrow().is_empty());
}

#[test]
fn inspection_errors_are_normalized_without_exposing_details_in_the_presentation() {
    let fixture = Fixture::new();
    let services = FakeServices {
        inspection_fails: true,
        ..FakeServices::new(&fixture)
    };
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        let StepResponse::Failed(failure) = fixture.execute(step, &services) else {
            panic!("expected failure")
        };
        assert!(failure.retryable);
        assert!(failure.diagnostic.contains("private bus error details"));
        assert!(!failure.presentation.detail().contains("private bus"));
    }
    assert!(services.actions.borrow().is_empty());
}

#[test]
fn action_errors_stay_in_the_step_and_a_fresh_attempt_can_retry() {
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        let fixture = Fixture::new();
        let services = FakeServices::new(&fixture);
        *services.action_error.borrow_mut() = Some(SettingsError::Apply {
            message: "private start error".into(),
        });
        let StepResponse::Failed(failure) = fixture.execute(step, &services) else {
            panic!("expected failure")
        };
        assert!(failure.retryable);
        assert_eq!(failure.diagnostic, "private start error");
        assert!(!failure
            .presentation
            .detail()
            .contains("private start error"));
        assert!(!services.active.get());
        assert_eq!(fixture.execute(step, &services), StepResponse::Complete);
    }
}

#[test]
fn command_success_is_not_completion_without_verified_activation() {
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        let fixture = Fixture::new();
        let services = FakeServices {
            start_succeeds: false,
            ..FakeServices::new(&fixture)
        };
        assert!(matches!(
            fixture.execute(step, &services),
            StepResponse::Failed(_)
        ));
        assert!(!services.active.get());
    }
}

#[test]
fn screen_activation_before_graphical_target_uses_restart_once() {
    let fixture = Fixture::new();
    let services = FakeServices {
        enable_starts: false,
        ..FakeServices::new(&fixture)
    };
    assert_eq!(
        fixture.execute(ServiceStep::Screen, &services),
        StepResponse::Complete
    );
    assert_eq!(
        &*services.actions.borrow(),
        &["enable-start-screen", "restart-screen"]
    );
    assert_eq!(
        fixture.execute(ServiceStep::Screen, &services),
        StepResponse::Complete
    );
    assert_eq!(services.actions.borrow().len(), 2);
}

#[test]
fn cancelled_authorization_returns_cancelled_without_retrying() {
    let fixture = Fixture::new();
    let services = FakeServices::new(&fixture);
    *services.action_error.borrow_mut() = Some(SettingsError::ActivationCancelled);
    assert_eq!(
        fixture.execute(ServiceStep::Lifecycle, &services),
        StepResponse::Cancelled
    );
    assert_eq!(services.actions.borrow().len(), 1);
    assert!(!services.active.get());
}

#[test]
fn cancellation_before_execution_prevents_all_actions() {
    let fixture = Fixture::new();
    let services = FakeServices::new(&fixture);
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        let cancellation = StepCancellation::default();
        assert!(cancellation.can_cancel());
        assert!(cancellation.cancel());
        assert_eq!(
            step.execute(
                &fixture.config,
                Some(&fixture.root),
                &services,
                &cancellation,
                &mut |_| panic!("cancelled operation must not start")
            ),
            StepResponse::Cancelled
        );
    }
    assert!(services.actions.borrow().is_empty());
}

#[test]
fn running_steps_report_and_enforce_non_cancelability() {
    for step in [ServiceStep::Screen, ServiceStep::Lifecycle] {
        let fixture = Fixture::new();
        let services = FakeServices::new(&fixture);
        let cancellation = StepCancellation::default();
        let mut reports = 0;
        let result = step.execute(
            &fixture.config,
            Some(&fixture.root),
            &services,
            &cancellation,
            &mut |response| {
                reports += 1;
                assert!(matches!(
                    response,
                    StepResponse::Running {
                        cancelable: false,
                        ..
                    }
                ));
                assert!(!cancellation.can_cancel());
                assert!(
                    !cancellation.cancel(),
                    "stale cancellation must be rejected"
                );
                assert!(matches!(
                    step.execute(
                        &fixture.config,
                        Some(&fixture.root),
                        &services,
                        &cancellation,
                        &mut |_| panic!("duplicate execution")
                    ),
                    StepResponse::Blocked(_)
                ));
            },
        );
        assert_eq!(reports, 1);
        assert_eq!(result, StepResponse::Complete);
        assert!(!cancellation.can_cancel());
        assert!(!cancellation.cancel());
        assert_eq!(services.actions.borrow().len(), 1);
    }
}
