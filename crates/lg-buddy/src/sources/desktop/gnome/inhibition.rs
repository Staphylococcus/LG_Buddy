//! GNOME's inhibition capability. It does not depend on Mutter, ScreenSaver,
//! activity observations, the honoring preference, or TV actions.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::inhibition::{InhibitionEvaluation, InhibitionState, PushInhibitionAdapter};
use crate::session_bus::{
    get_name_owner, new_session_bus_client, parse_name_owner_changed_signal, BusMethodCall,
    BusSignal, BusSignalMatch, BusValue, SessionBusClient, SessionBusError, DBUS_INTERFACE,
    DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
};

const SERVICE: &str = "org.gnome.SessionManager";
const PATH: &str = "/org/gnome/SessionManager";
const IDLE_FLAG: u32 = 8;
const PROCESS_INTERVAL: Duration = Duration::from_millis(50);
const DRAIN_INTERVAL: Duration = Duration::from_millis(1);
const RECONCILE_INTERVAL: Duration = Duration::from_secs(30);

pub struct GnomeInhibition {
    state: Mutex<InhibitionState>,
}

impl Default for GnomeInhibition {
    fn default() -> Self {
        Self {
            state: Mutex::new(InhibitionState::new("gnome-session-manager")),
        }
    }
}

impl PushInhibitionAdapter for GnomeInhibition {
    fn run(&self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            let result = new_session_bus_client()
                .and_then(|mut bus| self.watch(&mut bus, stop, RECONCILE_INTERVAL));
            if let Err(error) = result {
                self.unavailable(error);
            }
            super::super::wait_for_retry(stop);
        }
        self.unavailable("inhibition monitoring stopped");
    }

    fn evaluate(&self) -> InhibitionEvaluation {
        self.state
            .lock()
            .expect("GNOME inhibition state")
            .evaluate()
    }
}

impl GnomeInhibition {
    fn unavailable(&self, reason: impl std::fmt::Display) {
        self.state
            .lock()
            .expect("GNOME inhibition state")
            .unavailable(reason);
    }

    fn watch(
        &self,
        bus: &mut impl SessionBusClient,
        stop: &AtomicBool,
        reconcile_interval: Duration,
    ) -> Result<(), SessionBusError> {
        // Subscribe before discovering/querying, so changes during initial
        // synchronization remain queued and are reconciled before publication.
        for member in ["InhibitorAdded", "InhibitorRemoved"] {
            bus.add_signal_match(BusSignalMatch {
                sender: None,
                path: Some(PATH),
                interface: Some(SERVICE),
                member: Some(member),
            })?;
        }
        bus.add_signal_match(BusSignalMatch {
            sender: Some(DBUS_SERVICE_NAME),
            path: Some(DBUS_OBJECT_PATH),
            interface: Some(DBUS_INTERFACE),
            member: Some("NameOwnerChanged"),
        })?;

        if !bus.name_has_owner(SERVICE)? {
            self.state.lock().expect("GNOME inhibition state").absent();
            return Ok(());
        }
        let owner = get_name_owner(bus, SERVICE)?;
        self.refresh(bus, &owner, stop)?;
        let mut last_refresh = Instant::now();
        while !stop.load(Ordering::SeqCst) {
            let source_changed = match bus.process(PROCESS_INTERVAL)? {
                Some(signal) => changed(&signal, &owner)?,
                None => false,
            };
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if source_changed || last_refresh.elapsed() >= reconcile_interval {
                self.refresh(bus, &owner, stop)?;
                last_refresh = Instant::now();
            }
        }
        Ok(())
    }

    fn refresh(
        &self,
        bus: &mut impl SessionBusClient,
        owner: &str,
        stop: &AtomicBool,
    ) -> Result<(), SessionBusError> {
        // A refresh keeps the last observation until a valid result replaces it.
        while !stop.load(Ordering::SeqCst) {
            let inhibited = bus
                .call_method(
                    BusMethodCall::new(owner, PATH, SERVICE, "IsInhibited")
                        .with_body(vec![BusValue::U32(IDLE_FLAG)]),
                )?
                .single_bool()?;
            let observed_at = Instant::now();
            if get_name_owner(bus, SERVICE)? != owner {
                return Err(owner_changed());
            }

            // Blocking queries may queue signals. A change after the queried
            // snapshot requires another query, not publication of stale clear.
            let mut dirty = false;
            while !stop.load(Ordering::SeqCst) {
                let Some(signal) = bus.process(DRAIN_INTERVAL)? else {
                    break;
                };
                dirty |= changed(&signal, owner)?;
            }
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if !dirty {
                self.state
                    .lock()
                    .expect("GNOME inhibition state")
                    .observe(inhibited, observed_at);
                return Ok(());
            }
        }
        Ok(())
    }
}

fn owner_changed() -> SessionBusError {
    SessionBusError::Transport("GNOME SessionManager owner changed".into())
}

fn changed(signal: &BusSignal, owner: &str) -> Result<bool, SessionBusError> {
    if signal.sender.as_deref() == Some(DBUS_SERVICE_NAME) {
        if let Some(change) = parse_name_owner_changed_signal(signal) {
            // A previous owner's loss may have been queued before we resolved this one.
            if change.name == SERVICE
                && change.old_owner.as_deref() == Some(owner)
                && change.new_owner.as_deref() != Some(owner)
            {
                return Err(owner_changed());
            }
        }
    }
    Ok(signal.sender.as_deref() == Some(owner)
        && signal.path == PATH
        && signal.interface == SERVICE
        && matches!(
            signal.member.as_str(),
            "InhibitorAdded" | "InhibitorRemoved"
        )
        && matches!(signal.body.as_slice(), [BusValue::ObjectPath(_)]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inhibition::InhibitionStatus;
    use crate::session_bus::BusReply;
    use std::collections::VecDeque;

    const OWNER: &str = ":1.42";

    struct FakeBus<'a> {
        adapter: &'a GnomeInhibition,
        stop: &'a AtomicBool,
        present: bool,
        owner: &'static str,
        replies: VecDeque<Result<bool, SessionBusError>>,
        signals: VecDeque<Option<BusSignal>>,
        subscriptions: usize,
        queries: Vec<InhibitionEvaluation>,
        stop_on_query: bool,
        polls_before_stop: usize,
    }

    impl<'a> FakeBus<'a> {
        fn new(adapter: &'a GnomeInhibition, stop: &'a AtomicBool, values: &[bool]) -> Self {
            Self {
                adapter,
                stop,
                present: true,
                owner: OWNER,
                replies: values.iter().copied().map(Ok).collect(),
                signals: VecDeque::new(),
                subscriptions: 0,
                queries: Vec::new(),
                stop_on_query: false,
                polls_before_stop: 1,
            }
        }
    }

    impl SessionBusClient for FakeBus<'_> {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            assert_eq!(
                name, SERVICE,
                "inhibition must not require activity services"
            );
            assert_eq!(self.subscriptions, 3, "subscribe before discovery");
            Ok(self.present)
        }

        fn add_signal_match(&mut self, _: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            self.subscriptions += 1;
            Ok(())
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            match call.member {
                "IsInhibited" => {
                    assert_eq!(call.destination, OWNER);
                    assert_eq!(call.path, PATH);
                    assert_eq!(call.interface, SERVICE);
                    assert_eq!(call.body, [BusValue::U32(8)]);
                    self.queries.push(self.adapter.evaluate());
                    if self.stop_on_query {
                        self.stop.store(true, Ordering::SeqCst);
                    }
                    self.replies
                        .pop_front()
                        .expect("unexpected query")
                        .map(|value| BusReply::new(vec![BusValue::Bool(value)]))
                }
                "GetNameOwner" => Ok(BusReply::new(vec![BusValue::String(self.owner.into())])),
                member => panic!("unexpected method: {member}"),
            }
        }

        fn process(&mut self, timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            if timeout == PROCESS_INTERVAL && self.signals.is_empty() {
                self.polls_before_stop -= 1;
                if self.polls_before_stop == 0 {
                    self.stop.store(true, Ordering::SeqCst);
                }
            }
            Ok(self.signals.pop_front().flatten())
        }
    }

    fn inhibitor_changed(owner: &str, member: &str) -> BusSignal {
        BusSignal::new(PATH, SERVICE, member)
            .with_sender(owner)
            .with_body(vec![BusValue::ObjectPath("/inhibitor/1".into())])
    }

    fn owner_change(old: &str, new: &str) -> BusSignal {
        BusSignal::new(DBUS_OBJECT_PATH, DBUS_INTERFACE, "NameOwnerChanged")
            .with_sender(DBUS_SERVICE_NAME)
            .with_body(vec![
                BusValue::String(SERVICE.into()),
                BusValue::String(old.into()),
                BusValue::String(new.into()),
            ])
    }

    #[test]
    fn subscribes_before_startup_snapshot_and_retains_quiet_inhibition() {
        let adapter = GnomeInhibition::default();
        let stop = AtomicBool::new(false);
        let mut bus = FakeBus::new(&adapter, &stop, &[true]);
        adapter.watch(&mut bus, &stop, RECONCILE_INTERVAL).unwrap();
        assert_eq!(bus.queries.len(), 1);
        assert_eq!(
            adapter.evaluate().diagnostics.status,
            InhibitionStatus::Inhibited
        );
        assert!(!adapter.evaluate().allowed);
    }

    #[test]
    fn periodic_reads_reconcile_missed_additions_and_removals() {
        let adapter = GnomeInhibition::default();
        let stop = AtomicBool::new(false);
        // The source changes without sending any notification.
        let mut bus = FakeBus::new(&adapter, &stop, &[false, true, false]);
        bus.polls_before_stop = 3;
        adapter.watch(&mut bus, &stop, Duration::ZERO).unwrap();
        assert_eq!(bus.queries.len(), 3);
        assert!(bus.queries[0].allowed);
        assert_eq!(bus.queries[1].diagnostics.status, InhibitionStatus::Clear);
        assert!(bus.queries[1].allowed);
        assert_eq!(
            bus.queries[2].diagnostics.status,
            InhibitionStatus::Inhibited
        );
        assert!(!bus.queries[2].allowed);
        assert!(adapter.evaluate().allowed);
        assert!(adapter.evaluate().diagnostics.last_release_at.is_some());
    }

    #[test]
    fn changes_during_snapshot_are_requeried_before_publishing() {
        let adapter = GnomeInhibition::default();
        let stop = AtomicBool::new(false);
        let mut bus = FakeBus::new(&adapter, &stop, &[false, true]);
        let before = adapter.evaluate();
        bus.signals
            .extend([Some(inhibitor_changed(OWNER, "InhibitorAdded")), None]);
        adapter.refresh(&mut bus, OWNER, &stop).unwrap();
        assert_eq!(bus.queries, [before.clone(), before]);
        assert_eq!(
            adapter.evaluate().diagnostics.status,
            InhibitionStatus::Inhibited
        );
        assert_eq!(adapter.evaluate().diagnostics.last_release_at, None);
    }

    #[test]
    fn refresh_retains_the_last_known_value_until_a_result_arrives() {
        for inhibited in [false, true] {
            let adapter = GnomeInhibition::default();
            adapter
                .state
                .lock()
                .unwrap()
                .observe(inhibited, Instant::now());
            let before = adapter.evaluate();
            let stop = AtomicBool::new(false);
            let mut bus = FakeBus::new(&adapter, &stop, &[!inhibited]);
            adapter.refresh(&mut bus, OWNER, &stop).unwrap();
            assert_eq!(bus.queries, [before]);
            assert_eq!(adapter.evaluate().allowed, inhibited);
        }
    }

    #[test]
    fn only_the_last_inhibitor_ending_records_a_release() {
        let adapter = GnomeInhibition::default();
        let stop = AtomicBool::new(false);
        let mut bus = FakeBus::new(&adapter, &stop, &[true, true, false, false]);
        for _ in 0..2 {
            adapter.refresh(&mut bus, OWNER, &stop).unwrap();
            assert!(!adapter.evaluate().allowed);
            assert_eq!(adapter.evaluate().diagnostics.last_release_at, None);
        }
        adapter.refresh(&mut bus, OWNER, &stop).unwrap();
        let release = adapter.evaluate().diagnostics.last_release_at.unwrap();
        adapter.refresh(&mut bus, OWNER, &stop).unwrap();
        assert!(adapter.evaluate().allowed);
        assert_eq!(
            adapter.evaluate().diagnostics.last_release_at,
            Some(release)
        );
    }

    #[test]
    fn absence_drops_any_previous_inhibition_contribution() {
        let adapter = GnomeInhibition::default();
        let stop = AtomicBool::new(false);
        let mut bus = FakeBus::new(&adapter, &stop, &[]);
        bus.present = false;
        adapter.watch(&mut bus, &stop, RECONCILE_INTERVAL).unwrap();
        assert!(adapter.evaluate().allowed);
        assert_eq!(
            adapter.evaluate().diagnostics.status,
            InhibitionStatus::Absent
        );
        adapter.state.lock().unwrap().observe(true, Instant::now());
        bus.subscriptions = 0;
        adapter.watch(&mut bus, &stop, RECONCILE_INTERVAL).unwrap();
        assert!(adapter.evaluate().allowed);
        assert_eq!(adapter.evaluate().diagnostics.last_release_at, None);
    }

    #[test]
    fn validates_owners_and_ignores_delayed_or_unrelated_signals() {
        assert!(changed(&inhibitor_changed(OWNER, "InhibitorRemoved"), OWNER).unwrap());
        assert!(!changed(&inhibitor_changed(":1.41", "InhibitorRemoved"), OWNER).unwrap());
        assert!(!changed(&inhibitor_changed(OWNER, "ActiveChanged"), OWNER).unwrap());
        let mut signal = inhibitor_changed(OWNER, "InhibitorAdded");
        signal.body.clear();
        assert!(!changed(&signal, OWNER).unwrap());
        assert!(changed(&owner_change(OWNER, ""), OWNER).is_err());
        assert!(changed(&owner_change(OWNER, ":1.43"), OWNER).is_err());
        assert!(!changed(&owner_change("", OWNER), OWNER).unwrap());
        assert!(!changed(&owner_change(OWNER, "").with_sender(":1.99"), OWNER).unwrap());
    }

    #[test]
    fn queued_loss_of_a_previous_owner_preserves_the_current_reading() {
        let adapter = GnomeInhibition::default();
        let stop = AtomicBool::new(false);
        let mut bus = FakeBus::new(&adapter, &stop, &[true]);
        // These notifications were queued before GetNameOwner resolved OWNER.
        bus.signals.extend([
            Some(owner_change(":1.41", "")),
            Some(owner_change("", OWNER)),
            None,
        ]);
        adapter.watch(&mut bus, &stop, RECONCILE_INTERVAL).unwrap();
        assert_eq!(bus.queries.len(), 1);
        assert_eq!(
            adapter.evaluate().diagnostics.status,
            InhibitionStatus::Inhibited
        );
        assert!(!adapter.evaluate().allowed);
    }

    #[test]
    fn owner_replacement_during_query_rejects_the_old_reply() {
        let adapter = GnomeInhibition::default();
        let stop = AtomicBool::new(false);
        let mut bus = FakeBus::new(&adapter, &stop, &[true]);
        bus.owner = ":1.43";
        assert!(adapter.refresh(&mut bus, OWNER, &stop).is_err());
        assert!(adapter.evaluate().allowed);
        assert_eq!(adapter.evaluate().diagnostics.observed_at, None);
    }

    #[test]
    fn read_failure_and_cancellation_do_not_publish_a_result() {
        let adapter = GnomeInhibition::default();
        let stop = AtomicBool::new(false);
        let mut bus = FakeBus::new(&adapter, &stop, &[]);
        bus.replies
            .push_back(Err(SessionBusError::Transport("read failed".into())));
        assert!(adapter.refresh(&mut bus, OWNER, &stop).is_err());
        assert!(adapter.evaluate().allowed);
        bus.replies.push_back(Ok(true));
        bus.stop_on_query = true;
        adapter.refresh(&mut bus, OWNER, &stop).unwrap();
        assert!(adapter.evaluate().allowed);
        assert_eq!(adapter.evaluate().diagnostics.observed_at, None);
    }
}
