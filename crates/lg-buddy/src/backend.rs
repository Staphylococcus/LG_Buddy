use std::cell::RefCell;
use std::env;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use crate::config::{load_config, resolve_config_path_from_env, ConfigPathError, ScreenBackend};
use crate::session_bus::new_session_bus_client;
use crate::sources::desktop::gnome::{
    GNOME_IDLE_MONITOR_NAME, GNOME_REQUIRED_SERVICES_REASON, GNOME_SCREEN_SAVER_NAME,
    GNOME_SHELL_NAME,
};
use crate::sources::desktop::wayland::{WaylandProviderCapabilities, WaylandSource};

pub const SWAYIDLE_DEPRECATION_NOTICE: &str =
    "swayidle is a deprecated compatibility backend planned for removal in LG Buddy 2.0.0; use auto or wayland";

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
                "invalid LG_BUDDY_SCREEN_BACKEND value `{value}`; expected auto, gnome, wayland, or swayidle"
            ),
        }
    }
}

impl Error for BackendSelectionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendDetectionError {
    NoSupportedBackend {
        gnome_reason: String,
        wayland_reason: String,
    },
    UnavailableBackend {
        backend: ScreenBackend,
        reason: String,
    },
    MissingRequiredCommand {
        backend: ScreenBackend,
        command: &'static str,
    },
}

impl fmt::Display for BackendDetectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSupportedBackend {
                gnome_reason,
                wayland_reason,
            } => write!(
                f,
                "no native activity source available; GNOME unavailable: {gnome_reason}; native Wayland unavailable: {wayland_reason}. Idle blanking will retry when a native source is available. To use LG Buddy without idle monitoring, set screen.idle_blank to disabled"
            ),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendResolution {
    backend: ScreenBackend,
    fallback_reason: Option<String>,
}

impl BackendResolution {
    pub fn backend(&self) -> ScreenBackend {
        self.backend
    }

    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    pub(crate) fn selected(backend: ScreenBackend, fallback_reason: Option<String>) -> Self {
        Self {
            backend,
            fallback_reason,
        }
    }
}

pub trait BackendProbe {
    fn has_command(&self, command: &str) -> bool;
    fn gnome_shell_available(&self) -> bool;
    fn gnome_screen_saver_available(&self) -> bool;
    fn gnome_idle_monitor_available(&self) -> bool;
    fn wayland_capabilities(&self) -> Result<WaylandProviderCapabilities, String> {
        Err("native Wayland capability probing is unavailable".to_string())
    }
}

#[derive(Default)]
pub struct SystemBackendProbe {
    wayland_source: RefCell<Option<WaylandSource>>,
}

impl SystemBackendProbe {
    pub(crate) fn take_wayland_source(&mut self) -> Option<WaylandSource> {
        self.wayland_source.get_mut().take()
    }
}

impl BackendProbe for SystemBackendProbe {
    fn has_command(&self, command: &str) -> bool {
        command_in_path(command)
    }

    fn gnome_shell_available(&self) -> bool {
        let mut bus = match new_session_bus_client() {
            Ok(bus) => bus,
            Err(_) => return false,
        };
        if bus.name_has_owner(GNOME_SHELL_NAME).unwrap_or(false) {
            return true;
        }

        bus.wait_for_name(GNOME_SHELL_NAME, GNOME_SHELL_WAIT_TIMEOUT)
            .is_ok()
    }

    fn gnome_screen_saver_available(&self) -> bool {
        let mut bus = match new_session_bus_client() {
            Ok(bus) => bus,
            Err(_) => return false,
        };
        bus.name_has_owner(GNOME_SCREEN_SAVER_NAME).unwrap_or(false)
    }

    fn gnome_idle_monitor_available(&self) -> bool {
        let mut bus = match new_session_bus_client() {
            Ok(bus) => bus,
            Err(_) => return false,
        };
        bus.name_has_owner(GNOME_IDLE_MONITOR_NAME).unwrap_or(false)
    }

    fn wayland_capabilities(&self) -> Result<WaylandProviderCapabilities, String> {
        self.wayland_source
            .borrow_mut()
            .get_or_insert_with(WaylandSource::default)
            .probe_capabilities()
            .map_err(|err| err.to_string())
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
    resolve_backend_from_system(configured).map(|resolution| resolution.backend())
}

pub fn resolve_backend_from_system(
    configured: ScreenBackend,
) -> Result<BackendResolution, BackendDetectionError> {
    resolve_backend_with_probe(&SystemBackendProbe::default(), configured)
}

pub fn detect_backend_with_probe(
    probe: &impl BackendProbe,
    configured: ScreenBackend,
) -> Result<ScreenBackend, BackendDetectionError> {
    resolve_backend_with_probe(probe, configured).map(|resolution| resolution.backend())
}

pub fn resolve_backend_with_probe(
    probe: &impl BackendProbe,
    configured: ScreenBackend,
) -> Result<BackendResolution, BackendDetectionError> {
    match configured {
        ScreenBackend::Auto => {
            let gnome_shell_available = probe.gnome_shell_available();
            let gnome_screen_saver_available =
                gnome_shell_available && probe.gnome_screen_saver_available();
            let gnome_idle_monitor_available =
                gnome_screen_saver_available && probe.gnome_idle_monitor_available();
            let gnome_core_available = gnome_shell_available
                && gnome_screen_saver_available
                && gnome_idle_monitor_available;
            if gnome_core_available {
                return Ok(BackendResolution::selected(ScreenBackend::Gnome, None));
            }

            let gnome_reason = if !gnome_shell_available {
                "GNOME Shell is not available".to_string()
            } else {
                GNOME_REQUIRED_SERVICES_REASON.to_string()
            };

            match probe.wayland_capabilities() {
                Ok(_) => Ok(BackendResolution::selected(
                    ScreenBackend::Wayland,
                    Some(format!("GNOME unavailable: {gnome_reason}")),
                )),
                Err(wayland_reason) => Err(BackendDetectionError::NoSupportedBackend {
                    gnome_reason,
                    wayland_reason,
                }),
            }
        }
        ScreenBackend::Gnome => {
            let gnome_shell_available = probe.gnome_shell_available();
            let gnome_screen_saver_available =
                gnome_shell_available && probe.gnome_screen_saver_available();
            let gnome_idle_monitor_available =
                gnome_screen_saver_available && probe.gnome_idle_monitor_available();
            let gnome_core_available = gnome_shell_available
                && gnome_screen_saver_available
                && gnome_idle_monitor_available;
            if gnome_core_available {
                Ok(BackendResolution::selected(ScreenBackend::Gnome, None))
            } else {
                let reason = GNOME_REQUIRED_SERVICES_REASON;
                Err(BackendDetectionError::UnavailableBackend {
                    backend: ScreenBackend::Gnome,
                    reason: reason.to_string(),
                })
            }
        }
        ScreenBackend::Wayland => probe
            .wayland_capabilities()
            .map(|_| BackendResolution::selected(ScreenBackend::Wayland, None))
            .map_err(|reason| BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Wayland,
                reason,
            }),
        ScreenBackend::Swayidle => {
            if probe.has_command("swayidle") {
                Ok(BackendResolution::selected(ScreenBackend::Swayidle, None))
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
        configured_backend_from_sources, detect_backend_with_probe, resolve_backend_with_probe,
        BackendDetectionError, BackendProbe, BackendSelectionError,
    };
    use crate::config::ScreenBackend;
    use crate::sources::desktop::wayland::WaylandProviderCapabilities;

    #[derive(Debug, Clone, Copy)]
    struct FakeProbe {
        gnome_shell_available: bool,
        gnome_screen_saver_available: bool,
        gnome_idle_monitor_available: bool,

        has_swayidle: bool,
        forbid_command_probe: bool,
        wayland_capabilities: Result<WaylandProviderCapabilities, &'static str>,
    }

    impl Default for FakeProbe {
        fn default() -> Self {
            Self {
                gnome_shell_available: false,
                gnome_screen_saver_available: false,
                gnome_idle_monitor_available: false,

                has_swayidle: false,
                forbid_command_probe: false,
                wayland_capabilities: Err("no Wayland compositor is available"),
            }
        }
    }

    impl BackendProbe for FakeProbe {
        fn has_command(&self, command: &str) -> bool {
            assert!(
                !self.forbid_command_probe,
                "automatic detection probed a command"
            );
            match command {
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

        fn wayland_capabilities(&self) -> Result<WaylandProviderCapabilities, String> {
            self.wayland_capabilities.map_err(str::to_string)
        }
    }

    struct WaylandProbe(Result<WaylandProviderCapabilities, &'static str>);

    impl BackendProbe for WaylandProbe {
        fn has_command(&self, _command: &str) -> bool {
            false
        }

        fn gnome_shell_available(&self) -> bool {
            false
        }

        fn gnome_screen_saver_available(&self) -> bool {
            false
        }

        fn gnome_idle_monitor_available(&self) -> bool {
            false
        }

        fn wayland_capabilities(&self) -> Result<WaylandProviderCapabilities, String> {
            self.0.map_err(str::to_string)
        }
    }

    fn native_wayland_capabilities() -> WaylandProviderCapabilities {
        WaylandProviderCapabilities {
            idle_notifier_version: 2,
            seat_count: 1,
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
    fn wayland_override_is_accepted() {
        let backend = configured_backend_from_sources(Some("wayland"), None)
            .expect("parse native Wayland backend");

        assert_eq!(backend, ScreenBackend::Wayland);
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
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: true,

            has_swayidle: true,
            ..FakeProbe::default()
        };

        let backend =
            detect_backend_with_probe(&probe, ScreenBackend::Auto).expect("detect gnome backend");

        assert_eq!(backend, ScreenBackend::Gnome);
    }

    #[test]
    fn forced_gnome_activity_does_not_require_session_manager() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: true,
            ..FakeProbe::default()
        };
        assert_eq!(
            detect_backend_with_probe(&probe, ScreenBackend::Gnome),
            Ok(ScreenBackend::Gnome)
        );
    }

    #[test]
    fn auto_selects_native_wayland_before_swayidle() {
        let probe = FakeProbe {
            has_swayidle: true,
            wayland_capabilities: Ok(native_wayland_capabilities()),
            ..FakeProbe::default()
        };

        let resolution = resolve_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect("detect native Wayland backend");

        assert_eq!(resolution.backend(), ScreenBackend::Wayland);
        assert_eq!(
            resolution.fallback_reason(),
            Some("GNOME unavailable: GNOME Shell is not available")
        );
    }

    #[test]
    fn auto_selects_native_wayland_when_gnome_is_incomplete() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
            wayland_capabilities: Ok(native_wayland_capabilities()),
            ..FakeProbe::default()
        };

        let resolution = resolve_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect("fall back from incomplete GNOME to native Wayland");

        assert_eq!(resolution.backend(), ScreenBackend::Wayland);
        assert!(resolution
            .fallback_reason()
            .unwrap()
            .contains("org.gnome.Mutter.IdleMonitor"));
    }

    #[test]
    fn auto_never_probes_swayidle_even_when_it_is_installed() {
        for gnome_available in [false, true] {
            for wayland_available in [false, true] {
                let probe = FakeProbe {
                    gnome_shell_available: gnome_available,
                    gnome_screen_saver_available: gnome_available,
                    gnome_idle_monitor_available: gnome_available,
                    has_swayidle: true,
                    forbid_command_probe: true,
                    wayland_capabilities: if wayland_available {
                        Ok(native_wayland_capabilities())
                    } else {
                        Err("native activity is unavailable")
                    },
                };
                let result = detect_backend_with_probe(&probe, ScreenBackend::Auto);
                assert_eq!(result.is_ok(), gnome_available || wayland_available);
                assert_ne!(result, Ok(ScreenBackend::Swayidle));
            }
        }
    }

    #[test]
    fn auto_errors_when_no_supported_backend_is_available() {
        let probe = FakeProbe {
            gnome_shell_available: false,
            gnome_screen_saver_available: false,
            gnome_idle_monitor_available: false,
            has_swayidle: false,
            ..FakeProbe::default()
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect_err("missing backend should fail");

        assert_eq!(
            err,
            BackendDetectionError::NoSupportedBackend {
                gnome_reason: "GNOME Shell is not available".to_string(),
                wayland_reason: "no Wayland compositor is available".to_string(),
            }
        );
    }

    #[test]
    fn forced_gnome_requires_full_service_surface() {
        let probe = FakeProbe {
            gnome_shell_available: false,
            gnome_screen_saver_available: false,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
            ..FakeProbe::default()
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Gnome)
            .expect_err("forced gnome without a full GNOME session should fail");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Gnome,
                reason:
                    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required"
                        .to_string(),
            }
        );
    }

    #[test]
    fn auto_reports_gnome_unavailable_when_idle_monitor_is_missing_and_no_fallback_exists() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: false,
            ..FakeProbe::default()
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Auto)
            .expect_err("unsupported gnome surface should fail explicitly");

        assert_eq!(
            err,
            BackendDetectionError::NoSupportedBackend {
                gnome_reason:
                    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required"
                        .to_string(),
                wayland_reason: "no Wayland compositor is available".to_string(),
            }
        );
    }

    #[test]
    fn explicit_swayidle_remains_available_without_native_sources() {
        let probe = FakeProbe {
            has_swayidle: true,
            ..FakeProbe::default()
        };
        let error = detect_backend_with_probe(&probe, ScreenBackend::Auto).unwrap_err();
        assert!(error.to_string().contains("screen.idle_blank to disabled"));
        assert!(!error.to_string().contains("swayidle"));
        assert_eq!(
            detect_backend_with_probe(&probe, ScreenBackend::Swayidle),
            Ok(ScreenBackend::Swayidle)
        );
    }

    #[test]
    fn incomplete_gnome_does_not_trigger_swayidle() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
            forbid_command_probe: true,
            ..FakeProbe::default()
        };
        let error = resolve_backend_with_probe(&probe, ScreenBackend::Auto).unwrap_err();
        assert!(error.to_string().contains("org.gnome.Mutter.IdleMonitor"));
        assert!(error.to_string().contains("native Wayland unavailable"));
    }

    #[test]
    fn forced_gnome_requires_idle_monitor() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: false,
            has_swayidle: true,
            ..FakeProbe::default()
        };

        let err = detect_backend_with_probe(&probe, ScreenBackend::Gnome)
            .expect_err("forced gnome without idle monitor should fail");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Gnome,
                reason:
                    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required"
                        .to_string(),
            }
        );
    }

    #[test]
    fn forced_swayidle_requires_command() {
        let probe = FakeProbe {
            gnome_shell_available: true,
            gnome_screen_saver_available: true,
            gnome_idle_monitor_available: true,
            has_swayidle: false,
            ..FakeProbe::default()
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

    #[test]
    fn forced_wayland_requires_the_native_protocol_surface() {
        let err = detect_backend_with_probe(
            &WaylandProbe(Err(
                "ext_idle_notifier_v1 version 1 is unsupported; version 2 or newer is required",
            )),
            ScreenBackend::Wayland,
        )
        .expect_err("forced Wayland without protocol v2 should fail");

        assert_eq!(
            err,
            BackendDetectionError::UnavailableBackend {
                backend: ScreenBackend::Wayland,
                reason:
                    "ext_idle_notifier_v1 version 1 is unsupported; version 2 or newer is required"
                        .to_string(),
            }
        );
    }

    #[test]
    fn forced_wayland_is_selected_when_the_native_protocol_surface_is_available() {
        let backend = detect_backend_with_probe(
            &WaylandProbe(Ok(native_wayland_capabilities())),
            ScreenBackend::Wayland,
        )
        .expect("forced Wayland should be available");

        assert_eq!(backend, ScreenBackend::Wayland);
    }
}
