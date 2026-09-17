//! Compatibility adapter for the current activation-before-persist settings
//! path. Service semantics live in the internal setup steps; the future shared
//! onboarding flow will consume their structured responses directly.

use std::env;
use std::path::{Path, PathBuf};

use super::{ServiceController, SettingValue, SettingsError, SettingsMutation};
use crate::setup::{services::ServiceStep, StepCancellation, StepResponse};

pub(crate) fn activate_before_persist<C: ServiceController>(
    config_path: &Path,
    mutation: SettingsMutation,
    service_controller: &C,
) -> Result<(), SettingsError> {
    let install_root = env::var_os("LG_BUDDY_INSTALL_ROOT").map(PathBuf::from);
    activate_before_persist_with_root(config_path, mutation, service_controller, install_root)
}

fn activate_before_persist_with_root<C: ServiceController>(
    config_path: &Path,
    mutation: SettingsMutation,
    service_controller: &C,
    install_root: Option<PathBuf>,
) -> Result<(), SettingsError> {
    if mutation.new_value().ok().and_then(SettingValue::as_enum) != Some("enabled") {
        return Ok(());
    }
    let step = match mutation.key_name() {
        "screen.idle_blank" => ServiceStep::Screen,
        "system.sleep_wake_policy" => ServiceStep::Lifecycle,
        _ => return Ok(()),
    };
    match step.execute(
        config_path,
        install_root.as_deref(),
        service_controller,
        &StepCancellation::default(),
        &mut |_| {},
    ) {
        StepResponse::Complete => Ok(()),
        StepResponse::Cancelled => Err(SettingsError::ActivationCancelled),
        StepResponse::Blocked(failure) | StepResponse::Failed(failure) => {
            Err(SettingsError::Activation {
                message: failure.diagnostic,
            })
        }
        StepResponse::ActionRequired { .. }
        | StepResponse::Running { .. }
        | StepResponse::NotApplicable
        | StepResponse::InputRequired(_) => Err(SettingsError::Activation {
            message: "Service setup did not complete.".into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SCREEN_SERVICE: &str = "LG_Buddy_screen.service";
    use crate::settings::{ServiceController, UserServiceState, UserUnitEnableOutcome};
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;
    use std::rc::Rc;

    #[derive(Debug, Clone)]
    struct FakeServices {
        screen_config: PathBuf,
        screen_state: UserServiceState,
        screen_active: Rc<Cell<bool>>,
        lifecycle_active: bool,
        screen_starts: Rc<Cell<usize>>,
        lifecycle_starts: Rc<Cell<usize>>,
        lifecycle_error: Option<SettingsError>,
    }

    impl FakeServices {
        fn active() -> Self {
            Self {
                screen_config: PathBuf::new(),
                screen_state: UserServiceState::ActiveOrEnabled,
                screen_active: Rc::new(Cell::new(true)),
                lifecycle_active: false,
                screen_starts: Rc::new(Cell::new(0)),
                lifecycle_starts: Rc::new(Cell::new(0)),
                lifecycle_error: None,
            }
        }
    }

    impl ServiceController for FakeServices {
        fn user_service_config_path(&self, service: &str) -> Result<PathBuf, SettingsError> {
            assert_eq!(service, SCREEN_SERVICE);
            Ok(self.screen_config.clone())
        }
        fn user_service_state(&self, service: &str) -> Result<UserServiceState, SettingsError> {
            assert_eq!(service, SCREEN_SERVICE);
            Ok(self.screen_state)
        }

        fn user_service_is_active(&self, service: &str) -> Result<bool, SettingsError> {
            assert_eq!(service, SCREEN_SERVICE);
            Ok(self.screen_active.get())
        }

        fn restart_user_service(&self, _service: &str) -> Result<(), SettingsError> {
            Ok(())
        }

        fn enable_start_user_unit(
            &self,
            service: &str,
        ) -> Result<UserUnitEnableOutcome, SettingsError> {
            assert_eq!(service, SCREEN_SERVICE);
            self.screen_starts.set(self.screen_starts.get() + 1);
            self.screen_active.set(true);
            Ok(UserUnitEnableOutcome::EnabledStarted)
        }

        fn disable_stop_user_unit(&self, _unit: &str) -> Result<(), SettingsError> {
            Ok(())
        }

        fn system_lifecycle_is_active(&self) -> Result<bool, SettingsError> {
            Ok(self.lifecycle_active)
        }

        fn start_system_lifecycle(&self) -> Result<(), SettingsError> {
            self.lifecycle_starts.set(self.lifecycle_starts.get() + 1);
            self.lifecycle_error.clone().map_or(Ok(()), Err)
        }
    }

    fn mutation(path: &Path, key: &str) -> SettingsMutation {
        let store = crate::settings::SettingsStore::from_reader(
            crate::settings::ConfigEnvReader::parse(path, ""),
        );
        SettingsMutation::set(&store, key, "enabled").unwrap()
    }

    fn installed_root(path: &Path) -> PathBuf {
        let root = path.with_extension("install-root");
        for directory in [
            root.join("usr/lib/lg-buddy"),
            root.join("etc/systemd/system"),
            root.join("etc/NetworkManager/dispatcher.d/pre-down.d"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(
            root.join("usr/lib/lg-buddy/config-path"),
            path.to_string_lossy().as_bytes(),
        )
        .unwrap();
        fs::write(
            path,
            "screen_idle_blank=disabled\nsystem_sleep_wake_policy=disabled\n",
        )
        .unwrap();
        fs::write(
            root.join("etc/systemd/system/LG_Buddy_lifecycle.service"),
            "[Service]\n",
        )
        .unwrap();
        fs::write(
            root.join("etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle"),
            "#!/bin/sh\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn enabled_screen_setting_starts_available_user_service() {
        let path = crate::settings::tests::unique_test_path("activation-screen");
        let root = installed_root(&path);
        let services = FakeServices {
            screen_config: path.clone(),
            screen_active: Rc::new(Cell::new(false)),
            ..FakeServices::active()
        };
        let starts = services.screen_starts.clone();
        activate_before_persist_with_root(
            &path,
            mutation(&path, "screen.idle_blank"),
            &services,
            Some(root.clone()),
        )
        .unwrap();
        assert_eq!(starts.get(), 1);
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn active_lifecycle_is_reused_without_a_privileged_start() {
        let path = crate::settings::tests::unique_test_path("activation-lifecycle");
        let root = installed_root(&path);
        let services = FakeServices {
            lifecycle_active: true,
            ..FakeServices::active()
        };
        let starts = services.lifecycle_starts.clone();
        activate_before_persist_with_root(
            &path,
            mutation(&path, "system.sleep_wake_policy"),
            &services,
            Some(root.as_path().to_path_buf()),
        )
        .unwrap();
        assert_eq!(starts.get(), 0);
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn authorization_cancellation_is_distinguished_from_activation_failure() {
        let path = crate::settings::tests::unique_test_path("activation-cancel");
        let root = installed_root(&path);
        let services = FakeServices {
            lifecycle_error: Some(SettingsError::ActivationCancelled),
            ..FakeServices::active()
        };
        let error = activate_before_persist_with_root(
            &path,
            mutation(&path, "system.sleep_wake_policy"),
            &services,
            Some(root.as_path().to_path_buf()),
        )
        .unwrap_err();
        assert!(matches!(error, SettingsError::ActivationCancelled));
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn sleep_activation_requires_the_installed_unit_and_hook() {
        let path = crate::settings::tests::unique_test_path("activation-missing");
        let root = path.with_extension("install-root");
        fs::create_dir_all(root.join("usr/lib/lg-buddy")).unwrap();
        fs::write(
            root.join("usr/lib/lg-buddy/config-path"),
            path.to_string_lossy().as_bytes(),
        )
        .unwrap();
        fs::write(
            &path,
            "screen_idle_blank=disabled\nsystem_sleep_wake_policy=disabled\n",
        )
        .unwrap();
        let error = activate_before_persist_with_root(
            &path,
            mutation(&path, "system.sleep_wake_policy"),
            &FakeServices::active(),
            Some(root.clone()),
        )
        .unwrap_err();
        assert!(!matches!(error, SettingsError::ActivationCancelled));
        assert!(error.to_string().contains("lifecycle service"));
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(path).unwrap();
    }
}
