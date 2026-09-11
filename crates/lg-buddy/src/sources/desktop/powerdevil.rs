//! PowerDevil's pull inhibition capability. Its effective screen policy already
//! includes desktop filtering; do not reinterpret raw logind inhibitors here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::inhibition::{InhibitionEvaluation, InhibitionState, PullInhibitionAdapter};
use crate::session_bus::{
    get_name_owner, new_session_bus_client, BusMethodCall, BusValue, SessionBusClient,
    SessionBusError,
};

const SERVICE: &str = "org.kde.Solid.PowerManagement";
const PATH: &str = "/org/kde/Solid/PowerManagement/PolicyAgent";
const INTERFACE: &str = "org.kde.Solid.PowerManagement.PolicyAgent";
const CHANGE_SCREEN_SETTINGS: u32 = 4;

pub struct PowerDevilInhibition {
    // History is diagnostic only. Every request obtains a fresh answer.
    state: InhibitionState,
    owner: Option<String>,
}

impl Default for PowerDevilInhibition {
    fn default() -> Self {
        Self {
            state: InhibitionState::new("powerdevil"),
            owner: None,
        }
    }
}

impl PullInhibitionAdapter for PowerDevilInhibition {
    fn query(&mut self, cancelled: &AtomicBool) -> Option<InhibitionEvaluation> {
        if cancelled.load(Ordering::SeqCst) {
            return None;
        }
        // ponytail: connect per request; the next check also handles late service
        // startup or connection recovery without a background monitor.
        let result = new_session_bus_client().and_then(|mut bus| read(&mut bus, cancelled));
        self.complete(result, cancelled)
    }
}

impl PowerDevilInhibition {
    fn complete(
        &mut self,
        result: Result<Option<(String, bool, Instant)>, SessionBusError>,
        cancelled: &AtomicBool,
    ) -> Option<InhibitionEvaluation> {
        if cancelled.load(Ordering::SeqCst) {
            self.owner = None;
            self.state.unavailable("inhibition check cancelled");
            return None;
        }
        match result {
            Ok(Some((owner, inhibited, observed_at))) => {
                if self.owner.as_ref() != Some(&owner) {
                    // A new owner's clear answer is not an observed release by
                    // the old owner. Neither is recovery after a failed check.
                    self.state.unavailable("PowerDevil owner changed");
                }
                self.owner = Some(owner);
                self.state.observe(inhibited, observed_at);
            }
            Ok(None) => {
                self.owner = None;
                self.state.absent();
            }
            Err(error) => {
                self.owner = None;
                self.state.unavailable(error);
            }
        }
        Some(self.state.evaluate())
    }
}

fn read(
    bus: &mut impl SessionBusClient,
    cancelled: &AtomicBool,
) -> Result<Option<(String, bool, Instant)>, SessionBusError> {
    // Discovery does not activate PowerDevil on a desktop where it is absent.
    if !bus.name_has_owner(SERVICE)? || cancelled.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let owner = get_name_owner(bus, SERVICE)?;
    if cancelled.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let inhibited = bus
        .call_method(
            BusMethodCall::new(&owner, PATH, INTERFACE, "HasInhibition")
                .with_body(vec![BusValue::U32(CHANGE_SCREEN_SETTINGS)]),
        )?
        .single_bool()?;
    let observed_at = Instant::now();
    if cancelled.load(Ordering::SeqCst) {
        return Ok(None);
    }
    if get_name_owner(bus, SERVICE)? != owner {
        return Err(SessionBusError::Transport(
            "PowerDevil owner changed during check".into(),
        ));
    }
    Ok(Some((owner, inhibited, observed_at)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inhibition::InhibitionStatus;
    use crate::session_bus::{
        BusReply, BusSignal, BusSignalMatch, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use std::collections::VecDeque;
    use std::time::Duration;

    struct FakeBus<'a> {
        present: bool,
        replies: VecDeque<Result<BusReply, SessionBusError>>,
        destinations: Vec<String>,
        cancelled: &'a AtomicBool,
        cancel_after_call: Option<usize>,
    }

    impl<'a> FakeBus<'a> {
        fn new(cancelled: &'a AtomicBool, owner: &str, inhibited: bool, final_owner: &str) -> Self {
            Self {
                present: true,
                replies: [
                    BusValue::String(owner.into()),
                    BusValue::Bool(inhibited),
                    BusValue::String(final_owner.into()),
                ]
                .into_iter()
                .map(|value| Ok(BusReply::new(vec![value])))
                .collect(),
                destinations: Vec::new(),
                cancelled,
                cancel_after_call: None,
            }
        }
    }

    impl SessionBusClient for FakeBus<'_> {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            assert_eq!(name, SERVICE);
            Ok(self.present)
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            match call.member {
                "GetNameOwner" => {
                    assert_eq!(
                        (call.destination, call.path, call.interface),
                        (DBUS_SERVICE_NAME, DBUS_OBJECT_PATH, DBUS_INTERFACE)
                    );
                    assert_eq!(call.body, [BusValue::String(SERVICE.into())]);
                }
                "HasInhibition" => {
                    assert_eq!((call.path, call.interface), (PATH, INTERFACE));
                    assert_eq!(call.body, [BusValue::U32(4)]);
                    assert!(call.destination.starts_with(':'));
                }
                other => panic!("unexpected query: {other}"),
            }
            self.destinations.push(call.destination.to_string());
            if self.cancel_after_call == Some(self.destinations.len()) {
                self.cancelled.store(true, Ordering::SeqCst);
            }
            self.replies.pop_front().expect("unexpected extra call")
        }

        fn add_signal_match(&mut self, _: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            panic!("pull inhibition must not subscribe to signals");
        }

        fn process(&mut self, _: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            panic!("pull inhibition must not wait for signals");
        }
    }

    fn query(
        adapter: &mut PowerDevilInhibition,
        bus: &mut FakeBus<'_>,
    ) -> Option<InhibitionEvaluation> {
        let result = read(bus, bus.cancelled);
        adapter.complete(result, bus.cancelled)
    }

    #[test]
    fn effective_screen_policy_is_queried_each_time_and_release_is_observed_once() {
        let cancelled = AtomicBool::new(false);
        let mut adapter = PowerDevilInhibition::default();
        let mut release = None;
        // Effective answers with no signals: initially clear, playback begins,
        // one of two inhibitors ends, then the last ends (or Plasma suppresses it).
        for inhibited in [false, true, true, false, false, true] {
            let mut bus = FakeBus::new(&cancelled, ":1.1", inhibited, ":1.1");
            let result = query(&mut adapter, &mut bus).unwrap();
            assert_eq!(result.allowed, !inhibited);
            assert_eq!(
                result.diagnostics.status == InhibitionStatus::Inhibited,
                inhibited
            );
            assert_eq!(result.diagnostics.source, "powerdevil");
            assert!(result.diagnostics.observed_at.is_some());
            assert_eq!(
                bus.destinations,
                [DBUS_SERVICE_NAME, ":1.1", DBUS_SERVICE_NAME]
            );
            assert!(bus.replies.is_empty());
            if release.is_none() {
                release = result.diagnostics.last_release_at;
            } else {
                assert_eq!(result.diagnostics.last_release_at, release);
            }
        }
        assert!(release.is_some());
    }

    #[test]
    fn absent_powerdevil_is_neutral_without_a_policy_call_and_can_appear_later() {
        let cancelled = AtomicBool::new(false);
        let mut adapter = PowerDevilInhibition::default();
        let mut bus = FakeBus::new(&cancelled, ":1.1", true, ":1.1");
        bus.present = false;
        let absent = query(&mut adapter, &mut bus).unwrap();
        assert!(absent.allowed);
        assert_eq!(absent.diagnostics.status, InhibitionStatus::Absent);
        assert!(bus.destinations.is_empty());
        bus.present = true;
        assert!(!query(&mut adapter, &mut bus).unwrap().allowed);
    }

    #[test]
    fn failed_or_malformed_queries_drop_previous_inhibition_without_a_release() {
        let cancelled = AtomicBool::new(false);
        for reply in [
            Err(SessionBusError::Transport("é".repeat(1000))),
            Ok(BusReply::new(vec![BusValue::U32(0)])),
        ] {
            let mut adapter = PowerDevilInhibition::default();
            assert!(
                !query(
                    &mut adapter,
                    &mut FakeBus::new(&cancelled, ":1.1", true, ":1.1")
                )
                .unwrap()
                .allowed
            );
            let mut bus = FakeBus::new(&cancelled, ":1.1", false, ":1.1");
            bus.replies[1] = reply;
            let failed = query(&mut adapter, &mut bus).unwrap();
            assert!(failed.allowed);
            let InhibitionStatus::Unavailable(reason) = failed.diagnostics.status else {
                panic!("expected failure");
            };
            assert!(reason.chars().count() <= 512);
            assert_eq!(failed.diagnostics.last_release_at, None);
            let recovered = query(
                &mut adapter,
                &mut FakeBus::new(&cancelled, ":1.1", false, ":1.1"),
            )
            .unwrap();
            assert_eq!(recovered.diagnostics.status, InhibitionStatus::Clear);
            assert_eq!(recovered.diagnostics.last_release_at, None);
        }
    }

    #[test]
    fn owner_replacement_rejects_old_replies_and_does_not_invent_a_release() {
        let cancelled = AtomicBool::new(false);
        for replace_during_query in [false, true] {
            let mut adapter = PowerDevilInhibition::default();
            query(
                &mut adapter,
                &mut FakeBus::new(&cancelled, ":1.1", true, ":1.1"),
            )
            .unwrap();
            if replace_during_query {
                let result = query(
                    &mut adapter,
                    &mut FakeBus::new(&cancelled, ":1.1", false, ":1.2"),
                )
                .unwrap();
                assert!(matches!(
                    result.diagnostics.status,
                    InhibitionStatus::Unavailable(_)
                ));
                assert_eq!(result.diagnostics.last_release_at, None);
            }
            let current = query(
                &mut adapter,
                &mut FakeBus::new(&cancelled, ":1.2", false, ":1.2"),
            )
            .unwrap();
            assert!(current.allowed);
            assert_eq!(current.diagnostics.last_release_at, None);
        }
    }

    #[test]
    fn cancellation_at_each_io_boundary_discards_the_result_and_later_checks_are_fresh() {
        let cancelled = AtomicBool::new(true);
        let mut adapter = PowerDevilInhibition::default();
        assert!(adapter.query(&cancelled).is_none()); // No connection needed.
        for boundary in 1..=3 {
            cancelled.store(false, Ordering::SeqCst);
            query(
                &mut adapter,
                &mut FakeBus::new(&cancelled, ":1.1", true, ":1.1"),
            )
            .unwrap();
            let mut bus = FakeBus::new(&cancelled, ":1.1", false, ":1.1");
            bus.cancel_after_call = Some(boundary);
            assert!(query(&mut adapter, &mut bus).is_none());
            assert_eq!(bus.destinations.len(), boundary);
            cancelled.store(false, Ordering::SeqCst);
            let next = query(
                &mut adapter,
                &mut FakeBus::new(&cancelled, ":1.1", false, ":1.1"),
            )
            .unwrap();
            assert!(next.allowed);
            assert_eq!(next.diagnostics.last_release_at, None);
        }
    }
}
