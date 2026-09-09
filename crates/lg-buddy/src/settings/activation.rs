//! GUI-only preparation for settings that require a running service.
//!
//! The command-line settings path deliberately keeps its historical
//! persist-then-apply behavior.  The graphical onboarding path calls this
//! module first for the two settings whose enabled default would otherwise be
//! written while their service is unavailable.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use super::{ServiceController, SettingValue, SettingsError, SettingsMutation};

const SCREEN_SERVICE: &str = "LG_Buddy_screen.service";
const INSTALL_CONFIG_POINTER: &str = "/usr/lib/lg-buddy/config-path";
const LIFECYCLE_UNIT: &str = "/etc/systemd/system/LG_Buddy_lifecycle.service";
const NETWORK_MANAGER_HOOK: &str = "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle";

/// Prepare an enabled GUI setting before persisting it.
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
    let enabled = mutation.new_value().ok().and_then(SettingValue::as_enum) == Some("enabled");
    let requires_activation = enabled
        && matches!(
            mutation.key_name(),
            "screen.idle_blank" | "system.sleep_wake_policy"
        );
    if !requires_activation {
        return Ok(());
    }
    if service_controller.systemd_actions_disabled() {
        return Err(SettingsError::Activation {
            message: "systemd actions are disabled; the service was not activated".to_string(),
        });
    }

    match mutation.key_name() {
        "screen.idle_blank" => {
            validate_config_pointer(config_path, install_root.as_deref())?;
            activate_screen(service_controller)
        }
        "system.sleep_wake_policy" => {
            validate_config_pointer(config_path, install_root.as_deref())?;
            validate_sleep_installation(install_root.as_deref())?;
            activate_lifecycle(service_controller)
        }
        _ => Ok(()),
    }
}

fn activate_screen<C: ServiceController>(service_controller: &C) -> Result<(), SettingsError> {
    match service_controller.user_service_state(SCREEN_SERVICE)? {
        super::UserServiceState::Missing => Err(SettingsError::Activation {
            message: "LG Buddy's screen service is not installed.".to_string(),
        }),
        super::UserServiceState::InactiveDisabled => start_screen_service(service_controller),
        super::UserServiceState::ActiveOrEnabled => {
            if service_controller.user_service_is_active(SCREEN_SERVICE)? {
                Ok(())
            } else {
                start_screen_service(service_controller)
            }
        }
    }
}

fn start_screen_service<C: ServiceController>(service_controller: &C) -> Result<(), SettingsError> {
    service_controller.enable_start_user_unit(SCREEN_SERVICE)?;
    if !service_controller.user_service_is_active(SCREEN_SERVICE)? {
        // `enable_start_user_unit` may only enable a unit when no graphical
        // session target is active. Restarting here starts it before the
        // enabled setting becomes visible to the rest of the application.
        service_controller.restart_user_service(SCREEN_SERVICE)?;
    }
    if service_controller.user_service_is_active(SCREEN_SERVICE)? {
        Ok(())
    } else {
        Err(SettingsError::Activation {
            message: "LG Buddy's screen service did not become active.".to_string(),
        })
    }
}

fn validate_sleep_installation(install_root: Option<&Path>) -> Result<(), SettingsError> {
    for (path, label) in [
        (
            prefixed_path(install_root, LIFECYCLE_UNIT),
            "the installed lifecycle service",
        ),
        (
            prefixed_path(install_root, NETWORK_MANAGER_HOOK),
            "the installed NetworkManager sleep hook",
        ),
    ] {
        let metadata = fs::metadata(&path).map_err(|error| SettingsError::Activation {
            message: format!("{label} could not be read: {error}"),
        })?;
        if !metadata.file_type().is_file() {
            return Err(SettingsError::Activation {
                message: format!("{label} is not a regular file."),
            });
        }
    }
    Ok(())
}

fn validate_config_pointer(
    config_path: &Path,
    install_root: Option<&Path>,
) -> Result<(), SettingsError> {
    let pointer_path = prefixed_path(install_root, INSTALL_CONFIG_POINTER);
    let pointer_metadata =
        fs::metadata(&pointer_path).map_err(|error| SettingsError::Activation {
            message: format!("the installed configuration pointer could not be read: {error}"),
        })?;
    if !pointer_metadata.file_type().is_file() {
        return Err(SettingsError::Activation {
            message: "the installed configuration pointer is not a regular file.".to_string(),
        });
    }
    let pointer_config =
        fs::read_to_string(&pointer_path).map_err(|error| SettingsError::Activation {
            message: format!("the installed configuration pointer could not be read: {error}"),
        })?;
    let pointer_config = pointer_config
        .lines()
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| SettingsError::Activation {
            message: "the installed configuration pointer is empty.".to_string(),
        })?;
    let pointer_config =
        fs::canonicalize(pointer_config).map_err(|error| SettingsError::Activation {
            message: format!("the installed configuration target could not be resolved: {error}"),
        })?;
    let config_path = fs::canonicalize(config_path).map_err(|error| SettingsError::Activation {
        message: format!("the active configuration could not be resolved: {error}"),
    })?;
    if pointer_config != config_path {
        return Err(SettingsError::Activation {
            message: "the installed configuration pointer does not match the active configuration."
                .to_string(),
        });
    }

    Ok(())
}

fn activate_lifecycle<C: ServiceController>(service_controller: &C) -> Result<(), SettingsError> {
    if service_controller.system_lifecycle_is_active()? {
        return Ok(());
    }

    service_controller.start_system_lifecycle()
}

fn prefixed_path(root: Option<&Path>, path: &str) -> PathBuf {
    match root {
        Some(root) => root.join(path.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{ServiceController, UserServiceState, UserUnitEnableOutcome};
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;
    use std::rc::Rc;

    #[derive(Debug, Clone)]
    struct FakeServices {
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
