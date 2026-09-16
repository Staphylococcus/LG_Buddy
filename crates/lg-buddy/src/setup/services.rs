//! Read-only inspection and verified activation of installed services.
//! Used by the legacy Settings adapter; full installation/repair is in provision.

use std::fs;
use std::path::{Path, PathBuf};

use super::{StepCancellation, StepFailure, StepResponse};
use crate::presentation::brightness::UserFacingError;
use crate::settings::{ServiceController, SettingsError, UserServiceState};

const SCREEN_SERVICE: &str = "LG_Buddy_screen.service";
const INSTALL_CONFIG_POINTER: &str = "/usr/lib/lg-buddy/config-path";
const LIFECYCLE_UNIT: &str = "/etc/systemd/system/LG_Buddy_lifecycle.service";
const NETWORK_MANAGER_HOOK: &str = "/etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle";

#[derive(Debug, Clone, Copy)]
pub(crate) enum ServiceStep {
    Screen,
    Lifecycle,
}

impl ServiceStep {
    pub(crate) fn inspect<C: ServiceController>(
        self,
        config_path: &Path,
        install_root: Option<&Path>,
        services: &C,
    ) -> StepResponse {
        let ready = match self {
            Self::Screen => {
                let config = services
                    .user_service_config_path(SCREEN_SERVICE)
                    .and_then(|path| validate_matching_config(config_path, &path));
                if let Err(error) = config {
                    return self.failed(
                        "The screen service's configuration could not be verified.",
                        error,
                    );
                }
                match services.user_service_state(SCREEN_SERVICE) {
                    Ok(UserServiceState::Missing) => {
                        return self.blocked(
                            "LG Buddy's screen service is not installed.",
                            "LG Buddy's screen service is not installed.",
                        )
                    }
                    Err(error) => {
                        return self.failed("The screen service could not be inspected.", error)
                    }
                    Ok(_) => {}
                }
                services.user_service_is_active(SCREEN_SERVICE)
            }
            Self::Lifecycle => {
                if let Err(error) = validate_config_pointer(config_path, install_root)
                    .and_then(|()| validate_sleep_installation(install_root))
                {
                    return self.blocked(
                        "LG Buddy's system service installation needs repair.",
                        error.to_string(),
                    );
                }
                services.system_lifecycle_is_active()
            }
        };
        match ready {
            Ok(true) => StepResponse::Complete,
            Ok(false) if services.systemd_actions_disabled() => self.blocked(
                "Service changes are disabled in this environment.",
                "systemd actions are disabled; the service was not activated",
            ),
            Ok(false) => StepResponse::ActionRequired {
                explanation: match self {
                    Self::Screen => "Start LG Buddy's screen monitor.",
                    Self::Lifecycle => {
                        "Start LG Buddy's system service to follow computer sleep and wake."
                    }
                },
                requires_authorization: matches!(self, Self::Lifecycle),
            },
            Err(error) => self.failed("The service's current state could not be checked.", error),
        }
    }

    pub(crate) fn execute<C: ServiceController>(
        self,
        config_path: &Path,
        install_root: Option<&Path>,
        services: &C,
        cancellation: &StepCancellation,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse {
        if !cancellation.begin() {
            return if cancellation.is_cancelled() {
                StepResponse::Cancelled
            } else {
                self.blocked(
                    "This setup attempt has already started.",
                    "service activation attempt cannot be executed twice",
                )
            };
        }
        progress(StepResponse::Running {
            message: "Activating LG Buddy services…",
            cancelable: false,
        });
        let response = self.execute_inner(config_path, install_root, services);
        cancellation.finish();
        response
    }

    fn execute_inner<C: ServiceController>(
        self,
        config_path: &Path,
        install_root: Option<&Path>,
        services: &C,
    ) -> StepResponse {
        // Always inspect again, even if the caller previously observed missing work.
        let before = self.inspect(config_path, install_root, services);
        if !matches!(before, StepResponse::ActionRequired { .. }) {
            return before;
        }
        let result = match self {
            Self::Screen => start_screen(services),
            Self::Lifecycle => services.start_system_lifecycle(),
        };
        if let Err(error) = result {
            return if matches!(error, SettingsError::ActivationCancelled) {
                StepResponse::Cancelled
            } else {
                self.failed("The service could not be started. Try again.", error)
            };
        }
        match self.inspect(config_path, install_root, services) {
            StepResponse::ActionRequired { .. } => self.failed(
                "The service did not become active. Try again.",
                "service start returned successfully but the service is still inactive",
            ),
            response => response,
        }
    }

    fn failure(self, detail: &str, diagnostic: impl ToString, retryable: bool) -> StepFailure {
        StepFailure {
            presentation: UserFacingError::new(
                match self {
                    Self::Screen => "Screen service setup incomplete",
                    Self::Lifecycle => "System service setup incomplete",
                },
                detail,
            ),
            diagnostic: diagnostic.to_string(),
            retryable,
        }
    }

    fn failed(self, detail: &str, diagnostic: impl ToString) -> StepResponse {
        StepResponse::Failed(self.failure(detail, diagnostic, true))
    }

    fn blocked(self, detail: &str, diagnostic: impl ToString) -> StepResponse {
        StepResponse::Blocked(self.failure(detail, diagnostic, false))
    }
}

fn start_screen<C: ServiceController>(services: &C) -> Result<(), SettingsError> {
    services.enable_start_user_unit(SCREEN_SERVICE)?;
    if !services.user_service_is_active(SCREEN_SERVICE)? {
        // Before a graphical target is active, enable_start may only enable it.
        services.restart_user_service(SCREEN_SERVICE)?;
    }
    Ok(())
}

fn validate_matching_config(
    config_path: &Path,
    service_config: &Path,
) -> Result<(), SettingsError> {
    let service_config =
        fs::canonicalize(service_config).map_err(|error| SettingsError::Activation {
            message: format!("the screen service's configuration could not be resolved: {error}"),
        })?;
    let config_path = fs::canonicalize(config_path).map_err(|error| SettingsError::Activation {
        message: format!("the active configuration could not be resolved: {error}"),
    })?;
    if service_config != config_path {
        return Err(SettingsError::Activation {
            message: "the screen service's configuration does not match the active configuration."
                .to_string(),
        });
    }
    Ok(())
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

fn prefixed_path(root: Option<&Path>, path: &str) -> PathBuf {
    match root {
        Some(root) => root.join(path.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests;
