use std::process::Command;

use crate::backend::{BackendProbe, SystemBackendProbe};
use crate::config::ScreenBackend;
use crate::session::{
    IdleTimeoutSource, SessionBackend, SessionBackendCapabilities, SessionBackendError,
    SessionEvent,
};

const GNOME_DBUS_NAME: &str = "org.gnome.ScreenSaver";
const GNOME_IDLE_MONITOR_NAME: &str = "org.gnome.Mutter.IdleMonitor";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GnomeBackendStatus {
    pub shell_available: bool,
    pub screen_saver_available: bool,
    pub idle_monitor_available: bool,
}

impl GnomeBackendStatus {
    pub fn can_start(&self) -> bool {
        self.shell_available && self.screen_saver_available && self.idle_monitor_available
    }
}

pub trait GnomeProbe {
    fn gnome_shell_available(&self) -> bool;
    fn screen_saver_available(&self) -> bool;
    fn idle_monitor_available(&self) -> bool;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemGnomeProbe;

impl GnomeProbe for SystemGnomeProbe {
    fn gnome_shell_available(&self) -> bool {
        let probe = SystemBackendProbe;
        probe.has_command("gdbus") && probe.gnome_shell_available()
    }

    fn screen_saver_available(&self) -> bool {
        dbus_name_has_owner(GNOME_DBUS_NAME)
    }

    fn idle_monitor_available(&self) -> bool {
        dbus_name_has_owner(GNOME_IDLE_MONITOR_NAME)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GnomeBackend<P = SystemGnomeProbe> {
    probe: P,
}

impl Default for GnomeBackend<SystemGnomeProbe> {
    fn default() -> Self {
        Self::new(SystemGnomeProbe)
    }
}

impl<P> GnomeBackend<P> {
    pub fn new(probe: P) -> Self {
        Self { probe }
    }
}

impl<P: GnomeProbe> GnomeBackend<P> {
    pub fn status(&self) -> GnomeBackendStatus {
        GnomeBackendStatus {
            shell_available: self.probe.gnome_shell_available(),
            screen_saver_available: self.probe.screen_saver_available(),
            idle_monitor_available: self.probe.idle_monitor_available(),
        }
    }
}

impl<P: GnomeProbe> SessionBackend for GnomeBackend<P> {
    fn backend(&self) -> ScreenBackend {
        ScreenBackend::Gnome
    }

    fn capabilities(&self) -> Result<SessionBackendCapabilities, SessionBackendError> {
        let status = self.status();
        if !status.can_start() {
            return Err(SessionBackendError::Unavailable {
                backend: ScreenBackend::Gnome,
                reason: "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required",
            });
        }

        Ok(SessionBackendCapabilities {
            idle_timeout_source: IdleTimeoutSource::LgBuddyConfigured,
            wake_requested: true,
            before_sleep: false,
            after_resume: false,
            lock_unlock: false,
            early_user_activity: true,
        })
    }
}

pub fn map_monitor_line(line: &str) -> Option<SessionEvent> {
    match line {
        value if value.contains("ActiveChanged") && value.contains("(true,)") => {
            Some(SessionEvent::Idle)
        }
        value
            if value.contains("org.gnome.ScreenSaver.WakeUpScreen")
                || value.contains("member=WakeUpScreen") =>
        {
            Some(SessionEvent::WakeRequested)
        }
        value if value.contains("ActiveChanged") && value.contains("(false,)") => {
            Some(SessionEvent::Active)
        }
        _ => None,
    }
}

fn dbus_name_has_owner(name: &str) -> bool {
    Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            name,
        ])
        .output()
        .is_ok_and(|output| {
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("(true,)")
        })
}

#[cfg(test)]
mod tests {
    use super::{map_monitor_line, GnomeBackend, GnomeBackendStatus, GnomeProbe};
    use crate::config::ScreenBackend;
    use crate::session::{
        IdleTimeoutSource, SessionBackend, SessionBackendCapabilities, SessionBackendError,
        SessionEvent,
    };

    #[derive(Debug, Clone, Copy)]
    struct FakeProbe {
        shell_available: bool,
        screen_saver_available: bool,
        idle_monitor_available: bool,
    }

    impl GnomeProbe for FakeProbe {
        fn gnome_shell_available(&self) -> bool {
            self.shell_available
        }

        fn screen_saver_available(&self) -> bool {
            self.screen_saver_available
        }

        fn idle_monitor_available(&self) -> bool {
            self.idle_monitor_available
        }
    }

    #[test]
    fn active_changed_true_maps_to_idle_event() {
        let line = "signal time=1.0 sender=:1.2 -> destination=(null destination) serial=2 path=/org/gnome/ScreenSaver; interface=org.gnome.ScreenSaver; member=ActiveChanged (true,)";

        assert_eq!(map_monitor_line(line), Some(SessionEvent::Idle));
    }

    #[test]
    fn wakeup_signal_maps_to_wake_requested_event() {
        assert_eq!(
            map_monitor_line("member=WakeUpScreen"),
            Some(SessionEvent::WakeRequested)
        );
    }

    #[test]
    fn active_changed_false_maps_to_active_event() {
        let line = "signal org.gnome.ScreenSaver.ActiveChanged (false,)";

        assert_eq!(map_monitor_line(line), Some(SessionEvent::Active));
    }

    #[test]
    fn unknown_monitor_line_is_ignored() {
        assert_eq!(map_monitor_line("unrelated"), None);
    }

    #[test]
    fn gnome_backend_reports_capabilities_when_required_services_are_available() {
        let backend = GnomeBackend::new(FakeProbe {
            shell_available: true,
            screen_saver_available: true,
            idle_monitor_available: true,
        });

        assert_eq!(backend.backend(), ScreenBackend::Gnome);
        assert_eq!(
            backend.capabilities().expect("backend should be available"),
            SessionBackendCapabilities {
                idle_timeout_source: IdleTimeoutSource::LgBuddyConfigured,
                wake_requested: true,
                before_sleep: false,
                after_resume: false,
                lock_unlock: false,
                early_user_activity: true,
            }
        );
        assert_eq!(
            backend.status(),
            GnomeBackendStatus {
                shell_available: true,
                screen_saver_available: true,
                idle_monitor_available: true,
            }
        );
    }

    #[test]
    fn gnome_backend_requires_idle_monitor() {
        let backend = GnomeBackend::new(FakeProbe {
            shell_available: true,
            screen_saver_available: true,
            idle_monitor_available: false,
        });

        assert_eq!(
            backend.capabilities(),
            Err(SessionBackendError::Unavailable {
                backend: ScreenBackend::Gnome,
                reason: "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required",
            })
        );
    }

    #[test]
    fn gnome_backend_requires_full_service_surface() {
        let backend = GnomeBackend::new(FakeProbe {
            shell_available: false,
            screen_saver_available: true,
            idle_monitor_available: true,
        });

        assert_eq!(
            backend.capabilities(),
            Err(SessionBackendError::Unavailable {
                backend: ScreenBackend::Gnome,
                reason: "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required",
            })
        );
    }
}
