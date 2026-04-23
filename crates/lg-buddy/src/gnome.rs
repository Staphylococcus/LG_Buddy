use crate::backend::{BackendProbe, SystemBackendProbe};
use crate::config::ScreenBackend;
use crate::session::{
    IdleTimeoutSource, SessionBackend, SessionBackendCapabilities, SessionBackendError,
    SessionEvent,
};
use crate::session_bus::{
    BusMethodCall, BusSignal, BusValue, GdbusSessionBusClient, SessionBusClient, SessionBusError,
};

pub const GNOME_SCREEN_SAVER_NAME: &str = "org.gnome.ScreenSaver";
pub const GNOME_SCREEN_SAVER_PATH: &str = "/org/gnome/ScreenSaver";
pub const GNOME_SCREEN_SAVER_INTERFACE: &str = "org.gnome.ScreenSaver";
pub const GNOME_IDLE_MONITOR_NAME: &str = "org.gnome.Mutter.IdleMonitor";
pub const GNOME_IDLE_MONITOR_PATH: &str = "/org/gnome/Mutter/IdleMonitor/Core";
pub const GNOME_IDLE_MONITOR_INTERFACE: &str = "org.gnome.Mutter.IdleMonitor";
pub const GNOME_REQUIRED_SERVICES_REASON: &str =
    "GNOME Shell, org.gnome.ScreenSaver, and org.gnome.Mutter.IdleMonitor are required";

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
        let mut bus = GdbusSessionBusClient;
        bus.name_has_owner(GNOME_SCREEN_SAVER_NAME).unwrap_or(false)
    }

    fn idle_monitor_available(&self) -> bool {
        let mut bus = GdbusSessionBusClient;
        bus.name_has_owner(GNOME_IDLE_MONITOR_NAME).unwrap_or(false)
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
                reason: GNOME_REQUIRED_SERVICES_REASON,
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

pub fn map_screen_saver_signal(signal: &BusSignal) -> Option<SessionEvent> {
    if signal.path != GNOME_SCREEN_SAVER_PATH || signal.interface != GNOME_SCREEN_SAVER_INTERFACE {
        return None;
    }

    match (signal.member.as_str(), signal.body.as_slice()) {
        ("ActiveChanged", [BusValue::Bool(true)]) => Some(SessionEvent::Idle),
        ("ActiveChanged", [BusValue::Bool(false)]) => Some(SessionEvent::Active),
        ("WakeUpScreen", []) => Some(SessionEvent::WakeRequested),
        _ => None,
    }
}

pub fn current_idle_monitor_idletime_ms(
    bus: &mut impl SessionBusClient,
) -> Result<u64, SessionBusError> {
    bus.call_method(BusMethodCall::new(
        GNOME_IDLE_MONITOR_NAME,
        GNOME_IDLE_MONITOR_PATH,
        GNOME_IDLE_MONITOR_INTERFACE,
        "GetIdletime",
    ))?
    .single_u64()
}

#[cfg(test)]
fn gnome_backend_status_from_session_bus(
    bus: &mut impl SessionBusClient,
    shell_available: bool,
) -> GnomeBackendStatus {
    GnomeBackendStatus {
        shell_available,
        screen_saver_available: bus.name_has_owner(GNOME_SCREEN_SAVER_NAME).unwrap_or(false),
        idle_monitor_available: bus.name_has_owner(GNOME_IDLE_MONITOR_NAME).unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        current_idle_monitor_idletime_ms, gnome_backend_status_from_session_bus, map_monitor_line,
        map_screen_saver_signal, GnomeBackend, GnomeBackendStatus, GnomeProbe,
        GNOME_REQUIRED_SERVICES_REASON,
    };
    use crate::config::ScreenBackend;
    use crate::session::{
        IdleTimeoutSource, SessionBackend, SessionBackendCapabilities, SessionBackendError,
        SessionEvent,
    };
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError,
    };
    use std::time::Duration;

    #[derive(Debug, Clone, Copy)]
    struct FakeProbe {
        shell_available: bool,
        screen_saver_available: bool,
        idle_monitor_available: bool,
    }

    #[derive(Debug, Default)]
    struct FakeSessionBus {
        screen_saver_available: bool,
        idle_monitor_available: bool,
        idletime_ms: Option<u64>,
        method_calls: Vec<(String, String, String, String)>,
        failed_names: Vec<String>,
    }

    impl SessionBusClient for FakeSessionBus {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            if self.failed_names.iter().any(|failed| failed == name) {
                return Err(SessionBusError::Transport(
                    "simulated bus failure".to_string(),
                ));
            }

            match name {
                super::GNOME_SCREEN_SAVER_NAME => Ok(self.screen_saver_available),
                super::GNOME_IDLE_MONITOR_NAME => Ok(self.idle_monitor_available),
                _ => Ok(false),
            }
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.method_calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
            ));
            match (
                call.destination,
                call.path,
                call.interface,
                call.member,
                self.idletime_ms,
            ) {
                (
                    super::GNOME_IDLE_MONITOR_NAME,
                    super::GNOME_IDLE_MONITOR_PATH,
                    super::GNOME_IDLE_MONITOR_INTERFACE,
                    "GetIdletime",
                    Some(value),
                ) => Ok(BusReply::new(vec![crate::session_bus::BusValue::U64(
                    value,
                )])),
                _ => Err(SessionBusError::Transport(
                    "no queued GNOME method reply".to_string(),
                )),
            }
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            let _ = rule;
            unreachable!("not used in GNOME probing tests")
        }

        fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            let _ = timeout;
            unreachable!("not used in GNOME probing tests")
        }
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
    fn active_changed_true_signal_maps_to_idle_event() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_body(vec![BusValue::Bool(true)]);

        assert_eq!(map_screen_saver_signal(&signal), Some(SessionEvent::Idle));
    }

    #[test]
    fn active_changed_false_signal_maps_to_active_event() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "ActiveChanged",
        )
        .with_body(vec![BusValue::Bool(false)]);

        assert_eq!(map_screen_saver_signal(&signal), Some(SessionEvent::Active));
    }

    #[test]
    fn wakeup_signal_maps_to_wake_requested_event_via_bus_signal() {
        let signal = BusSignal::new(
            super::GNOME_SCREEN_SAVER_PATH,
            super::GNOME_SCREEN_SAVER_INTERFACE,
            "WakeUpScreen",
        );

        assert_eq!(
            map_screen_saver_signal(&signal),
            Some(SessionEvent::WakeRequested)
        );
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
                reason: GNOME_REQUIRED_SERVICES_REASON,
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
                reason: GNOME_REQUIRED_SERVICES_REASON,
            })
        );
    }

    #[test]
    fn status_from_session_bus_uses_required_gnome_service_names() {
        let mut bus = FakeSessionBus {
            screen_saver_available: true,
            idle_monitor_available: false,
            ..FakeSessionBus::default()
        };

        assert_eq!(
            gnome_backend_status_from_session_bus(&mut bus, true),
            GnomeBackendStatus {
                shell_available: true,
                screen_saver_available: true,
                idle_monitor_available: false,
            }
        );
    }

    #[test]
    fn status_from_session_bus_treats_bus_errors_as_unavailable() {
        let mut bus = FakeSessionBus {
            screen_saver_available: true,
            idle_monitor_available: true,
            failed_names: vec![super::GNOME_IDLE_MONITOR_NAME.to_string()],
            ..FakeSessionBus::default()
        };

        assert_eq!(
            gnome_backend_status_from_session_bus(&mut bus, true),
            GnomeBackendStatus {
                shell_available: true,
                screen_saver_available: true,
                idle_monitor_available: false,
            }
        );
    }

    #[test]
    fn current_idle_monitor_idletime_uses_gnome_idle_monitor_endpoint() {
        let mut bus = FakeSessionBus {
            idletime_ms: Some(1_500),
            ..FakeSessionBus::default()
        };

        assert_eq!(current_idle_monitor_idletime_ms(&mut bus), Ok(1_500));
        assert_eq!(
            bus.method_calls,
            vec![(
                super::GNOME_IDLE_MONITOR_NAME.to_string(),
                super::GNOME_IDLE_MONITOR_PATH.to_string(),
                super::GNOME_IDLE_MONITOR_INTERFACE.to_string(),
                "GetIdletime".to_string(),
            )]
        );
    }
}
