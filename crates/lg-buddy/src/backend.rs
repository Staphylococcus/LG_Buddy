use std::env;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::process::Command;

use crate::config::{load_config, resolve_config_path_from_env, ConfigPathError, ScreenBackend};

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
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBackendProbe;

impl BackendProbe for SystemBackendProbe {
    fn has_command(&self, command: &str) -> bool {
        command_in_path(command)
    }

    fn gnome_shell_available(&self) -> bool {
        let call_output = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.DBus",
                "--object-path",
                "/org/freedesktop/DBus",
                "--method",
                "org.freedesktop.DBus.NameHasOwner",
                "org.gnome.Shell",
            ])
            .output();

        if let Ok(output) = call_output {
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("(true,)")
            {
                return true;
            }
        }

        Command::new("gdbus")
            .args(["wait", "--session", "--timeout", "2", "org.gnome.Shell"])
            .status()
            .is_ok_and(|status| status.success())
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
                return Ok(ScreenBackend::Gnome);
            }

            if probe.has_command("swayidle") {
                return Ok(ScreenBackend::Swayidle);
            }

            Err(BackendDetectionError::NoSupportedBackend)
        }
        ScreenBackend::Gnome => {
            if probe.has_command("gdbus") {
                Ok(ScreenBackend::Gnome)
            } else {
                Err(BackendDetectionError::MissingRequiredCommand {
                    backend: ScreenBackend::Gnome,
                    command: "gdbus",
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
    fn forced_swayidle_requires_command() {
        let probe = FakeProbe {
            has_gdbus: true,
            gnome_shell_available: true,
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
