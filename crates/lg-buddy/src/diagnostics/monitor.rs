//! Current activity and scheduling state from the existing monitor owner.

use super::DiagnosticSection;
use crate::session_bus::{new_session_bus_client, SessionBusClient};

pub(super) fn collect() -> Vec<DiagnosticSection> {
    match new_session_bus_client() {
        Ok(mut bus) => collect_from_bus(&mut bus),
        Err(_) => unavailable(),
    }
}

fn unavailable() -> Vec<DiagnosticSection> {
    vec![DiagnosticSection::new(
        "Desktop monitor",
        "Runtime snapshot: unavailable",
    )]
}

fn collect_from_bus(bus: &mut impl SessionBusClient) -> Vec<DiagnosticSection> {
    use crate::session_bus::{
        BusMethodCall, BusValue, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use crate::session_notifications::{
        GET_MONITOR_DIAGNOSTICS_METHOD, SESSION_BUS_NAME, SESSION_INTERFACE, SESSION_OBJECT_PATH,
    };
    let report = (|| {
        // Address the existing unique owner: diagnostics must never activate a
        // monitor, or silently move to a replacement process during this read.
        let owner = bus
            .call_method(
                BusMethodCall::new(
                    DBUS_SERVICE_NAME,
                    DBUS_OBJECT_PATH,
                    DBUS_INTERFACE,
                    "GetNameOwner",
                )
                .with_body(vec![BusValue::String(SESSION_BUS_NAME.into())]),
            )
            .ok()?;
        let owner = owner.single_string().ok()?;
        let reply = bus
            .call_method(BusMethodCall::new(
                owner,
                SESSION_OBJECT_PATH,
                SESSION_INTERFACE,
                GET_MONITOR_DIAGNOSTICS_METHOD,
            ))
            .ok()?;
        let [BusValue::String(activity), BusValue::String(_), BusValue::String(context)] =
            reply.body.as_slice()
        else {
            return None;
        };
        Some((activity.clone(), context.clone()))
    })();
    match report {
        Some((activity, context)) => vec![
            DiagnosticSection::new(
                "Desktop monitor",
                monitor_fields(
                    &context,
                    &[
                        "monitor process:",
                        "mode:",
                        "snapshot age:",
                        "configured integration:",
                        "configuration origin:",
                    ],
                ),
            ),
            DiagnosticSection::new(
                "Activity sources",
                monitor_fields(
                    &activity,
                    &[
                        "next scheduled inactivity action:",
                        "source:",
                        "absence detail:",
                        "No native activity adapter is running.",
                    ],
                ),
            ),
        ],
        None => unavailable(),
    }
}

// The monitor endpoint also carries policy debugging data. Select current
// fields only, including when collecting from an older running monitor.
fn monitor_fields(text: &str, prefixes: &[&str]) -> String {
    text.lines()
        .filter(|line| {
            prefixes
                .iter()
                .any(|prefix| line.trim_start().starts_with(prefix))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusError,
    };
    use std::collections::VecDeque;
    use std::time::Duration;

    struct MonitorBus {
        replies: VecDeque<Result<BusReply, SessionBusError>>,
        calls: usize,
    }
    impl SessionBusClient for MonitorBus {
        fn name_has_owner(&mut self, _: &str) -> Result<bool, SessionBusError> {
            panic!("unexpected probe")
        }
        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            use crate::session_notifications::{
                GET_MONITOR_DIAGNOSTICS_METHOD, SESSION_BUS_NAME, SESSION_INTERFACE,
                SESSION_OBJECT_PATH,
            };
            if self.calls == 0 {
                assert_eq!(call.destination, "org.freedesktop.DBus");
                assert_eq!(call.member, "GetNameOwner");
                assert_eq!(call.body, [BusValue::String(SESSION_BUS_NAME.into())]);
            } else {
                assert_eq!(call.destination, ":1.23");
                assert_eq!(call.path, SESSION_OBJECT_PATH);
                assert_eq!(call.interface, SESSION_INTERFACE);
                assert_eq!(call.member, GET_MONITOR_DIAGNOSTICS_METHOD);
                assert!(call.body.is_empty());
            }
            self.calls += 1;
            self.replies.pop_front().expect("unexpected bus call")
        }
        fn add_signal_match(&mut self, _: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            panic!("must not subscribe")
        }
        fn process(&mut self, _: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            panic!("must not drain activity")
        }
    }

    #[test]
    fn getter_addresses_the_existing_owner_and_retains_only_current_fields() {
        let mut bus = MonitorBus {
            replies: [
                Ok(BusReply::new(vec![BusValue::String(":1.23".into())])),
                Ok(BusReply::new(vec![
                    BusValue::String("next scheduled inactivity action: none\nsource: wayland; available: false\n  absence detail: unavailable interface\nAvailability comes from the interface connection.\n".into()),
                    BusValue::String("historical inhibition evaluation".into()),
                    BusValue::String("monitor process: 42\nmode: running\nsnapshot age: 3 ms\nconfigured integration: auto\nSnapshot and evaluation ages describe observations.\n".into()),
                ])),
            ].into(), calls: 0,
        };
        let sections = collect_from_bus(&mut bus);
        assert_eq!(bus.calls, 2);
        assert!(bus.replies.is_empty());
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].body(), "monitor process: 42\nmode: running\nsnapshot age: 3 ms\nconfigured integration: auto\n");
        assert_eq!(sections[1].body(), "next scheduled inactivity action: none\nsource: wayland; available: false\n  absence detail: unavailable interface\n");
    }

    #[test]
    fn absent_owner_failed_read_and_malformed_reply_yield_an_unavailable_snapshot() {
        let failed = || Err(SessionBusError::Transport("private bus error".into()));
        for replies in [
            vec![failed()],
            vec![
                Ok(BusReply::new(vec![BusValue::String(":1.23".into())])),
                failed(),
            ],
            vec![
                Ok(BusReply::new(vec![BusValue::String(":1.23".into())])),
                Ok(BusReply::new(vec![BusValue::Bool(true)])),
            ],
        ] {
            let expected_calls = replies.len();
            let mut bus = MonitorBus {
                replies: replies.into(),
                calls: 0,
            };
            let sections = collect_from_bus(&mut bus);
            assert_eq!(bus.calls, expected_calls);
            assert_eq!(sections.len(), 1);
            assert_eq!(sections[0].body(), "Runtime snapshot: unavailable\n");
        }
    }
}
