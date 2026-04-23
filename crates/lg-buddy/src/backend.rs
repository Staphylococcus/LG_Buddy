use std::env;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::config::{load_config, resolve_config_path_from_env, ConfigPathError, ScreenBackend};
use crate::gnome::{
    GNOME_IDLE_MONITOR_NAME, GNOME_REQUIRED_SERVICES_REASON, GNOME_SCREEN_SAVER_NAME,
};
use crate::session_bus::{GdbusSessionBusClient, SessionBusClient};

const GNOME_SHELL_NAME: &str = "org.gnome.Shell";
const GNOME_SHELL_WAIT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendSelectionError {
    InvalidOverride(String),
}

impl fmt::Display for BackendSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOverride(value) => write!(
                f,
                "invalid LG_BUDDY_SCREEN_BACKEND value `{value}`; expected auto, gnome, or swayidle"
            ),
        }
    }
}

impl Error for BackendSelectionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendDetectionError {
    NoSupportedBackend,
    UnavailableBackend {
        backend: ScreenBackend,
        reason: &'static str,
    },
    MissingRequiredCommand {
        backend: ScreenBackend,
        command: &'static str,
    },
}

impl fmt::Display for BackendDetectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSupportedBackend => {
                write!(
                    f,
                    "no supported backend detected; install swayidle or run under GNOME with gdbus"
                )
            }
            Self::UnavailableBackend { backend, reason } => {
                write!(f, "backend `{}` is unavailable: {reason}", backend.as_str())
            }
            Self::MissingRequiredCommand { backend, command } => write!(
                f,
                "backend `{}` requires `{command}` to be installed",
                backend.as_str()
            ),
        }
    }
}

impl Error for BackendDetectionError {}

pub trait BackendProbe {
    fn has_command(&self, command: &str) -> bool;
    fn gnome_shell_available(&self) -> bool;
    fn gnome_screen_saver_available(&self) -> bool;
    fn gnome_idle_monitor_available(&self) -> bool;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBackendProbe;

impl BackendProbe for SystemBackendProbe {
    fn has_command(&self, command: &str) -> bool {
        command_in_path(command)
    }

    fn gnome_shell_available(&self) -> bool {
        let mut bus = GdbusSessionBusClient;
        if bus.name_has_owner(GNOME_SHELL_NAME).unwrap_or(false) {
            return true;
        }

        bus.wait_for_name(GNOME_SHELL_NAME, GNOME_SHELL_WAIT_TIMEOUT)
            .is_ok()
    }

    fn gnome_screen_saver_available(&self) -> bool {
        let mut bus = GdbusSessionBusClient;
        bus.name_has_owner(GNOME_SCREEN_SAVER_NAME).unwrap_or(false)
    }

    fn gnome_idle_monitor_available(&self) -> bool {
        let mut bus = GdbusSessionBusClient;
        bus.name_has_owner(GNOME_IDLE_MONITOR_NAME).unwrap_or(false)
    }
}

pub fn configured_backend_from_env_or_config() -> Result<ScreenBackend, BackendSelectionError> {
    let override_value = env::var("LG_BUDDY_SCREEN_BACKEND").ok();
    let config_backend = match resolve_config_path_from_env() {
        Ok(path) => load_config(&path).ok().map(|config| config.screen_backend),
        Err(ConfigPathError::NotConfigured) => None,
    };

    configured_backend_from_sources(override_value.as_deref(), config_backend)
}

pub fn configured_backend_from_sources(
    override_value: Option<&str>,
    config_backend: Option<ScreenBackend>,
) -> Result<ScreenBackend, BackendSelectionError> {
    if let Some(value) = override_value {
        return value
            .parse::<ScreenBackend>()
            .map_err(|_| BackendSelectionError::InvalidOverride(value.to_string()));
    }

    Ok(config_backend.unwrap_or(ScreenBackend::Auto))
}

pub fn detect_backend_from_system(
    configured: ScreenBackend,
) -> Result<ScreenBackend, BackendDetectionError> {
    detect_backend_with_probe(&SystemBackendProbe, configured)
}

pub fn detect_backend_with_probe(
    probe: &impl BackendProbe,
    configured: ScreenBackend,
) -> Result<ScreenBackend, BackendDetectionError> {
    match configured {
        ScreenBackend::Auto => {
            if probe.has_command("gdbus") && probe.gnome_shell_available() {
                if probe.gnome_screen_saver_available() && probe.gnome_idle_monitor_available() {
                    return Ok(ScreenBackend::Gnome);
                }

                if probe.has_command("swayidle") {
                    return Ok(ScreenBackend::Swayidle);
                }

                return Err(BackendDetectionError::UnavailableBackend {
                    backend: ScreenBackend::Gnome,
                    reason: GNOME_REQUIRED_SERVICES_REASON,
                });
            }

            if probe.has_command("swayidle") {
                return Ok(ScreenBackend::Swayidle);
            }

            Err(BackendDetectionError::NoSupportedBackend)
        }
        ScreenBackend::Gnome => {
            if !probe.has_command("gdbus") {
                return Err(BackendDetectionError::MissingRequiredCommand {
                    backend: ScreenBackend::Gnome,
                    command: "gdbus",
                });
            }

            if probe.gnome_shell_available()
                && probe.gnome_screen_saver_available()
                && probe.gnome_idle_monitor_available()
            {
                Ok(ScreenBackend::Gnome)
            } else {
                Err(BackendDetectionError::UnavailableBackend {
                    backend: ScreenBackend::Gnome,
                    reason: GNOME_REQUIRED_SERVICES_REASON,
                })
            }
        }
        ScreenBackend::Swayidle => {
            if probe.has_command("swayidle") {
                Ok(ScreenBackend::Swayidle)
            } else {
                Err(BackendDetectionError::MissingRequiredCommand {
                    backend: ScreenBackend::Swayidle,
                    command: "swayidle",
                })
            }
        }
    }
}

fn command_in_path(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(command).is_file();
    }

    let Some(path) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

#[cfg(test)]
mod tests {
    use super::{
        configured_backend_from_sources, detect_backend_with_probe, BackendDetectionError,
        BackendProbe, BackendSelectionError,
    };
    use crate::config::ScreenBackend;

    #[derive(Debug, Clone, Copy)]
    struct FakeProbe {
        has_gdbus: bool,
        gnome_shell_available: bool,
        gnome_screen_saver_available: bool,
        gnome_idle_monitor_available: bool,
        has_swayidle: bool,
    }

    impl BackendProbe for FakeProbe {
        fn has_command(&self, command: &str) -> bool {
            match command {
                "gdbus" => self.has_gdbus,
                "swayidle" => self.has_swayidle,
                _ => false,
            }
        }

        fn gnome_shell_available(&self) -> bool {
            self.gnome_shell_available
        }

        fn gnome_screen_saver_available(&self) -> bool {
            self.gnome_screen_saver_available
        }

        fn gnome_idle_monitor_available(&self) -> bool {
            self.gnome_idle_monitor_available
        }
    }

    #[test]
    fn env_override_wins_over_config_backend() {
        let backend = configured_backend_from_sources(Some("swayidle"), Some(ScreenBackend::Gnome))
            .expect("parse override backend");

        assert_eq!(backend, ScreenBackend::Swayidle);
    }

    #[test]
    fn config_backend_is_used_when_override_is_missing() {
        let backend = configured_backend_from_sources(None, Some(ScreenBackend::Gnome))
            .expect("use config backend");

        assert_eq!(backend, ScreenBackend::Gnome);
    }

    #[test]
    fn auto_is_used_when_no_override_or_config_is_available() {
        let backend =
            configured_backend_from_sources(None, None).expect("fallback to auto backend");

        assert_eq!(backend, ScreenBackend::Auto);
    }

    #[test]
    fn invalid_override_is_rejected() {
        let err = configured_backend_from_sources(Some("kde"), None)
            .expect_err("invalid override should fail");

        assert_eq!(
            err,
            BackendSelectionError::InvalidOverride("kde".to_string())
        );
    }

    #[test]
    fn auto_prefers_gnome_when_available() {
        let probe = FakeProbe {
            has_gdbus: true,
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: true,
            has_swayidle: true,
        };

        let backend =
            detect_backend_with_probe(&probe, ScreenBackend::Auto).expect("detect gnome backend");

        assert_eq!(backend, ScreenBackend::Gnome);
    }

    #[test]
    fn auto_falls_back_to_swayidle() {
        let probe = FakeProbe {
            has_gdbus: true,
            gnome_shell_available: false,
            gnome_screen_saver_available: false,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
        };

        let backend = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect("detect swayidle backend");

        assert_eq!(backend, ScreenBackend::Swayidle);
    }

    #[test]
    fn auto_errors_when_no_supported_backend_is_available() {
        let probe = FakeProbe {
            has_gdbus: false,
            gnome_shell_available: false,
            gnome_screen_saver_available: false,
            gnome_idle_monitor_available: false,
            has_swayidle: false,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect_err("missing backend should fail");

        assert_eq!(err, BackendDetectionError::NoSupportedBackend);
    }

    #[test]
    fn forced_gnome_requires_gdbus() {
        let probe = FakeProbe {
            has_gdbus: false,
            gnome_shell_available: false,
            gnome_screen_saver_available: false,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Gnome)
            .expect_err("forced gnome without gdbus should fail");

        assert_eq!(
            err,
            BackendDetectionError::MissingRequiredCommand {
                backend: ScreenBackend::Gnome,
                command: "gdbus",
            }
        );
    }

    #[test]
    fn auto_reports_gnome_unavailable_when_idle_monitor_is_missing_and_no_fallback_exists() {
        let probe = FakeProbe {
            has_gdbus: true,
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: false,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect_err("unsupported gnome surface should fail explicitly");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Gnome,
                reason:
                    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required",
            }
        );
    }

    #[test]
    fn auto_falls_back_to_swayidle_when_gnome_idle_monitor_is_missing() {
        let probe = FakeProbe {
            has_gdbus: true,
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
        };

        let backend = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect("fallback to swayidle when GNOME is incomplete");

        assert_eq!(backend, ScreenBackend::Swayidle);
    }

    #[test]
    fn forced_gnome_requires_full_service_surface() {
        let probe = FakeProbe {
            has_gdbus: true,
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Gnome)
            .expect_err("forced gnome without idle monitor should fail");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Gnome,
                reason:
                    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required",
            }
        );
    }

    #[test]
    fn forced_swayidle_requires_command() {
        let probe = FakeProbe {
            has_gdbus: true,
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: true,
            has_swayidle: false,
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Swayidle)
            .expect_err("forced swayidle without command should fail");

        assert_eq!(
            err,
            BackendDetectionError::MissingRequiredCommand {
                backend: ScreenBackend::Swayidle,
                command: "swayidle",
            }
        );
    }
}
