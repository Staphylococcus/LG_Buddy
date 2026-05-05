use std::os::fd::OwnedFd;

use crate::events::RuntimeEvent;
use crate::session_bus::{
    BusMethodCall, BusSignal, BusSignalMatch, BusValue, SessionBusClient, SessionBusError,
};

pub const LOGIND_SERVICE_NAME: &str = "org.freedesktop.login1";
pub const LOGIND_MANAGER_PATH: &str = "/org/freedesktop/login1";
pub const LOGIND_MANAGER_INTERFACE: &str = "org.freedesktop.login1.Manager";
pub const LOGIND_INHIBIT_WHO: &str = "LG Buddy";
pub const LOGIND_INHIBIT_WHY: &str = "Handle LG TV power state around system sleep";
const DBUS_PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

pub fn logind_signal_match() -> BusSignalMatch<'static> {
    BusSignalMatch {
        sender: None,
        path: Some(LOGIND_MANAGER_PATH),
        interface: Some(LOGIND_MANAGER_INTERFACE),
        member: Some("PrepareForSleep"),
    }
}

pub fn add_logind_signal_match(bus: &mut impl SessionBusClient) -> Result<(), SessionBusError> {
    bus.add_signal_match(logind_signal_match())
}

pub fn map_prepare_for_sleep_signal(signal: &BusSignal) -> Option<RuntimeEvent> {
    if signal.path != LOGIND_MANAGER_PATH
        || signal.interface != LOGIND_MANAGER_INTERFACE
        || signal.member != "PrepareForSleep"
    {
        return None;
    }

    match signal.body.as_slice() {
        [BusValue::Bool(preparing)] => {
            Some(RuntimeEvent::from_logind_prepare_for_sleep(*preparing))
        }
        _ => None,
    }
}

pub fn acquire_sleep_delay_inhibitor(
    bus: &mut impl SessionBusClient,
) -> Result<OwnedFd, SessionBusError> {
    bus.call_method(
        BusMethodCall::new(
            LOGIND_SERVICE_NAME,
            LOGIND_MANAGER_PATH,
            LOGIND_MANAGER_INTERFACE,
            "Inhibit",
        )
        .with_body(vec![
            BusValue::String("sleep".to_string()),
            BusValue::String(LOGIND_INHIBIT_WHO.to_string()),
            BusValue::String(LOGIND_INHIBIT_WHY.to_string()),
            BusValue::String("delay".to_string()),
        ]),
    )?
    .single_unix_fd()
}

pub fn preparing_for_sleep(bus: &mut impl SessionBusClient) -> Result<bool, SessionBusError> {
    bus.call_method(
        BusMethodCall::new(
            LOGIND_SERVICE_NAME,
            LOGIND_MANAGER_PATH,
            DBUS_PROPERTIES_INTERFACE,
            "Get",
        )
        .with_body(vec![
            BusValue::String(LOGIND_MANAGER_INTERFACE.to_string()),
            BusValue::String("PreparingForSleep".to_string()),
        ]),
    )?
    .single_bool()
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_sleep_delay_inhibitor, add_logind_signal_match, logind_signal_match,
        map_prepare_for_sleep_signal, LOGIND_INHIBIT_WHO, LOGIND_INHIBIT_WHY,
        LOGIND_MANAGER_INTERFACE, LOGIND_MANAGER_PATH, LOGIND_SERVICE_NAME,
    };
    use super::{preparing_for_sleep, DBUS_PROPERTIES_INTERFACE};
    use crate::events::{EventSource, RuntimeEvent, RuntimeEventKind};
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError,
    };
    use std::collections::VecDeque;
    use std::os::fd::AsRawFd;
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct FakeBus {
        calls: Vec<(String, String, String, String, Vec<BusValue>)>,
        matches: Vec<OwnedBusSignalMatch>,
        replies: VecDeque<BusReply>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct OwnedBusSignalMatch {
        sender: Option<String>,
        path: Option<String>,
        interface: Option<String>,
        member: Option<String>,
    }

    impl<'a> From<BusSignalMatch<'a>> for OwnedBusSignalMatch {
        fn from(value: BusSignalMatch<'a>) -> Self {
            Self {
                sender: value.sender.map(ToOwned::to_owned),
                path: value.path.map(ToOwned::to_owned),
                interface: value.interface.map(ToOwned::to_owned),
                member: value.member.map(ToOwned::to_owned),
            }
        }
    }

    impl SessionBusClient for FakeBus {
        fn name_has_owner(&mut self, _name: &str) -> Result<bool, SessionBusError> {
            unreachable!("name probing is not used by logind tests")
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
                call.body,
            ));
            self.replies
                .pop_front()
                .ok_or_else(|| SessionBusError::Transport("missing fake reply".to_string()))
        }

        fn add_signal_match(&mut self, rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            self.matches.push(OwnedBusSignalMatch::from(rule));
            Ok(())
        }

        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            unreachable!("process is not used by logind tests")
        }
    }

    fn prepare_for_sleep_signal(value: bool) -> BusSignal {
        BusSignal::new(
            LOGIND_MANAGER_PATH,
            LOGIND_MANAGER_INTERFACE,
            "PrepareForSleep",
        )
        .with_sender(LOGIND_SERVICE_NAME)
        .with_body(vec![BusValue::Bool(value)])
    }

    #[test]
    fn prepare_for_sleep_true_maps_to_machine_preparing_event() {
        assert_eq!(
            map_prepare_for_sleep_signal(&prepare_for_sleep_signal(true)),
            Some(RuntimeEvent::new(
                EventSource::LinuxLogind,
                RuntimeEventKind::MachinePreparingForSleep
            ))
        );
    }

    #[test]
    fn prepare_for_sleep_false_maps_to_machine_resumed_event() {
        assert_eq!(
            map_prepare_for_sleep_signal(&prepare_for_sleep_signal(false)),
            Some(RuntimeEvent::new(
                EventSource::LinuxLogind,
                RuntimeEventKind::MachineResumed
            ))
        );
    }

    #[test]
    fn unrelated_or_malformed_logind_signals_are_ignored() {
        let wrong_member = BusSignal::new(LOGIND_MANAGER_PATH, LOGIND_MANAGER_INTERFACE, "Lock");
        let malformed = BusSignal::new(
            LOGIND_MANAGER_PATH,
            LOGIND_MANAGER_INTERFACE,
            "PrepareForSleep",
        )
        .with_body(vec![BusValue::String("true".to_string())]);

        assert_eq!(map_prepare_for_sleep_signal(&wrong_member), None);
        assert_eq!(map_prepare_for_sleep_signal(&malformed), None);
    }

    #[test]
    fn logind_signal_match_targets_prepare_for_sleep_without_sender_owner() {
        assert_eq!(
            logind_signal_match(),
            BusSignalMatch {
                sender: None,
                path: Some(LOGIND_MANAGER_PATH),
                interface: Some(LOGIND_MANAGER_INTERFACE),
                member: Some("PrepareForSleep"),
            }
        );
    }

    #[test]
    fn add_logind_signal_match_registers_prepare_for_sleep_match() {
        let mut bus = FakeBus::default();

        add_logind_signal_match(&mut bus).expect("add logind signal match");

        assert_eq!(
            bus.matches,
            vec![OwnedBusSignalMatch::from(logind_signal_match())]
        );
    }

    #[test]
    fn acquire_sleep_delay_inhibitor_calls_logind_inhibit() {
        let mut pipe_fds = [0; 2];
        let pipe_result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(pipe_result, 0, "test pipe should be created");

        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::UnixFd(pipe_fds[0])]));

        let inhibitor =
            acquire_sleep_delay_inhibitor(&mut bus).expect("acquire sleep delay inhibitor");

        assert_eq!(inhibitor.as_raw_fd(), pipe_fds[0]);
        assert_eq!(
            bus.calls,
            vec![(
                LOGIND_SERVICE_NAME.to_string(),
                LOGIND_MANAGER_PATH.to_string(),
                LOGIND_MANAGER_INTERFACE.to_string(),
                "Inhibit".to_string(),
                vec![
                    BusValue::String("sleep".to_string()),
                    BusValue::String(LOGIND_INHIBIT_WHO.to_string()),
                    BusValue::String(LOGIND_INHIBIT_WHY.to_string()),
                    BusValue::String("delay".to_string()),
                ],
            )]
        );

        drop(inhibitor);
        unsafe {
            libc::close(pipe_fds[1]);
        }
    }

    #[test]
    fn preparing_for_sleep_reads_logind_property() {
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::Variant(Box::new(
                BusValue::Bool(true),
            ))]));

        let preparing = preparing_for_sleep(&mut bus).expect("read PreparingForSleep");

        assert!(preparing);
        assert_eq!(
            bus.calls,
            vec![(
                LOGIND_SERVICE_NAME.to_string(),
                LOGIND_MANAGER_PATH.to_string(),
                DBUS_PROPERTIES_INTERFACE.to_string(),
                "Get".to_string(),
                vec![
                    BusValue::String(LOGIND_MANAGER_INTERFACE.to_string()),
                    BusValue::String("PreparingForSleep".to_string()),
                ],
            )]
        );
    }

    #[test]
    fn preparing_for_sleep_accepts_unwrapped_bool_for_tests() {
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::Bool(false)]));

        assert!(!preparing_for_sleep(&mut bus).expect("read PreparingForSleep"));
    }

    #[test]
    fn preparing_for_sleep_rejects_unexpected_property_shape() {
        let mut bus = FakeBus::default();
        bus.replies
            .push_back(BusReply::new(vec![BusValue::Variant(Box::new(
                BusValue::String("false".to_string()),
            ))]));

        let err = preparing_for_sleep(&mut bus).expect_err("malformed property should fail");

        assert_eq!(
            err,
            SessionBusError::UnexpectedReplyShape {
                expected: "single bool",
                actual: "string",
            }
        );
    }
}
