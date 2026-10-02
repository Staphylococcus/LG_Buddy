use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::session_bus::{BusMethodCall, BusValue, SessionBusClient};

use super::{
    gnome_service_status_from_session_bus, resolve_screen_saver_owner, subscribe_to_gnome_signals,
    GnomeActivityWatch, GNOME_IDLE_MONITOR_INTERFACE, GNOME_IDLE_MONITOR_PATH,
    GNOME_SCREEN_SAVER_INTERFACE, GNOME_SCREEN_SAVER_PATH,
};

/// Fixed-stage errors for the GNOME readiness check. Every variant carries a
/// stable message; none embed transport strings, method replies, owner names,
/// or raw input values.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GnomeReadinessError {
    Cancelled,
    Unavailable,
    Subscriptions,
    ActivityWatch,
    ScreenSaver,
    IdleTime,
    Cleanup,
}

impl fmt::Display for GnomeReadinessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Cancelled => "GNOME readiness check cancelled",
            Self::Unavailable => "required GNOME services unavailable",
            Self::Subscriptions => "GNOME signal subscriptions failed",
            Self::ActivityWatch => "Mutter activity watch unavailable",
            Self::ScreenSaver => "GNOME ScreenSaver state unavailable",
            Self::IdleTime => "Mutter idletime unavailable",
            Self::Cleanup => "GNOME activity watch cleanup failed",
        };
        f.write_str(message)
    }
}

impl std::error::Error for GnomeReadinessError {}

/// Check GNOME readiness on a caller-supplied, owned observation client.
///
/// This helper consumes `bus` and drops it on every outcome, including
/// cancellation observed before setup starts. It never constructs a bus,
/// reads or mutates the environment, enters a monitor loop, processes or
/// publishes observations, spawns threads, or invokes screen/TV actions
/// (no `Lock`, `SetActive`, or `ResetIdletime`).
///
/// Cancellation is checked *between* bounded setup phases (entry, service
/// availability, subscriptions, watch creation, ScreenSaver owner
/// resolution, GetActive, GetIdletime, and after cleanup before success).
/// The caller-supplied `SessionBusClient` must bound its individual
/// operations; this helper does not claim bounded production bus
/// acquisition and does not add a timeout wrapper around synchronous calls.
pub(crate) fn check_gnome_readiness_on(
    mut bus: impl SessionBusClient,
    stop: &AtomicBool,
) -> Result<(), GnomeReadinessError> {
    if stop.load(Ordering::SeqCst) {
        return Err(GnomeReadinessError::Cancelled);
    }

    // Service availability.
    let services_available = gnome_service_status_from_session_bus(&mut bus).can_start();
    if stop.load(Ordering::SeqCst) {
        return Err(GnomeReadinessError::Cancelled);
    }
    if !services_available {
        return Err(GnomeReadinessError::Unavailable);
    }

    // Signal subscriptions.
    let subscriptions = subscribe_to_gnome_signals(&mut bus);
    if stop.load(Ordering::SeqCst) {
        return Err(GnomeReadinessError::Cancelled);
    }
    subscriptions.map_err(|_| GnomeReadinessError::Subscriptions)?;

    // Activity watch.
    let watch = match GnomeActivityWatch::connect(&mut bus) {
        Ok(watch) if watch.id > 0 => watch,
        _ if stop.load(Ordering::SeqCst) => return Err(GnomeReadinessError::Cancelled),
        _ => return Err(GnomeReadinessError::ActivityWatch),
    };

    // A valid watch exists from here on: every outcome, including
    // cancellation and read or owner-resolution failure, must attempt
    // RemoveWatch before the owned client is dropped. The post-watch checks
    // run in a local closure with no `?` escaping it, so the unconditional
    // cleanup below cannot be skipped.
    let post_watch = (|| -> Result<(), GnomeReadinessError> {
        if stop.load(Ordering::SeqCst) {
            return Err(GnomeReadinessError::Cancelled);
        }
        let screen_saver_owner =
            resolve_screen_saver_owner(&mut bus).map_err(|_| GnomeReadinessError::ScreenSaver)?;
        if stop.load(Ordering::SeqCst) {
            return Err(GnomeReadinessError::Cancelled);
        }
        bus.call_method(BusMethodCall::new(
            &screen_saver_owner,
            GNOME_SCREEN_SAVER_PATH,
            GNOME_SCREEN_SAVER_INTERFACE,
            "GetActive",
        ))
        .map_err(|_| GnomeReadinessError::ScreenSaver)?
        .single_bool()
        .map_err(|_| GnomeReadinessError::ScreenSaver)?;
        if stop.load(Ordering::SeqCst) {
            return Err(GnomeReadinessError::Cancelled);
        }
        bus.call_method(BusMethodCall::new(
            &watch.owner,
            GNOME_IDLE_MONITOR_PATH,
            GNOME_IDLE_MONITOR_INTERFACE,
            "GetIdletime",
        ))
        .map_err(|_| GnomeReadinessError::IdleTime)?
        .single_u64()
        .map_err(|_| GnomeReadinessError::IdleTime)?;
        Ok(())
    })();

    // Unconditional cleanup on the watch's own unique owner.
    let cleanup = bus
        .call_method(
            BusMethodCall::new(
                &watch.owner,
                GNOME_IDLE_MONITOR_PATH,
                GNOME_IDLE_MONITOR_INTERFACE,
                "RemoveWatch",
            )
            .with_body(vec![BusValue::U32(watch.id)]),
        )
        .map_err(|_| GnomeReadinessError::Cleanup)
        .and_then(|reply| {
            if reply.body.is_empty() {
                Ok(())
            } else {
                Err(GnomeReadinessError::Cleanup)
            }
        });

    // Cancellation observed at a checkpoint takes precedence over operation
    // and cleanup errors; a cleanup failure after a successful primary check
    // prevents readiness from being reported; a primary error is preserved
    // even when cleanup also fails.
    if matches!(&post_watch, Err(GnomeReadinessError::Cancelled)) || stop.load(Ordering::SeqCst) {
        return Err(GnomeReadinessError::Cancelled);
    }

    match (post_watch, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(cleanup_error)) => Err(cleanup_error),
        (Err(primary_error), _) => Err(primary_error),
    }
}

#[cfg(test)]
mod tests {
    use super::{check_gnome_readiness_on, GnomeReadinessError};
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError, DBUS_INTERFACE, DBUS_OBJECT_PATH, DBUS_SERVICE_NAME,
    };
    use crate::sources::desktop::gnome::{
        GNOME_IDLE_MONITOR_INTERFACE, GNOME_IDLE_MONITOR_NAME, GNOME_IDLE_MONITOR_PATH,
        GNOME_SCREEN_SAVER_INTERFACE, GNOME_SCREEN_SAVER_NAME, GNOME_SCREEN_SAVER_PATH,
        GNOME_SHELL_NAME,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Debug, Default)]
    struct ReadyState {
        owners: Vec<String>,
        screen_saver_owner: Option<String>,
        idle_monitor_owner: Option<String>,
        watch_id: u32,
        active: Option<bool>,
        active_reply: Option<Vec<BusValue>>,
        idletime_ms: Option<u64>,
        idletime_reply: Option<Vec<BusValue>>,
        remove_watch_reply: Option<Vec<BusValue>>,
        fail_remove_watch: bool,
        fail_subscriptions: bool,
        fail_watch: bool,
        watch_reply: Option<Vec<BusValue>>,
        queried_names: Vec<String>,
        cancel_service_check: bool,
        cancel_subscription: bool,
        calls: Vec<(String, String, String, String)>,
        bodies: Vec<Vec<BusValue>>,
        match_count: usize,
        cancel_after_call: Option<usize>,
        stop: Option<Arc<AtomicBool>>,
    }

    struct ReadyBus {
        state: Arc<Mutex<ReadyState>>,
        dropped: Arc<Mutex<bool>>,
    }

    impl Drop for ReadyBus {
        fn drop(&mut self) {
            *self.dropped.lock().unwrap() = true;
        }
    }

    /// A fake observation bus with the happy-path configuration: all three
    /// required service names present, unique owners resolvable, a valid
    /// activity watch, active screen saver, and zero idletime.
    fn ready_bus() -> (ReadyBus, Arc<Mutex<ReadyState>>, Arc<Mutex<bool>>) {
        let state = Arc::new(Mutex::new(ReadyState {
            owners: vec![
                GNOME_SHELL_NAME.to_string(),
                GNOME_SCREEN_SAVER_NAME.to_string(),
                GNOME_IDLE_MONITOR_NAME.to_string(),
            ],
            screen_saver_owner: Some(":1.41".to_string()),
            idle_monitor_owner: Some(":1.42".to_string()),
            watch_id: 7,
            active: Some(true),
            idletime_ms: Some(0),
            ..ReadyState::default()
        }));
        let dropped = Arc::new(Mutex::new(false));
        (
            ReadyBus {
                state: Arc::clone(&state),
                dropped: Arc::clone(&dropped),
            },
            state,
            dropped,
        )
    }

    impl SessionBusClient for ReadyBus {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            let mut state = self.state.lock().unwrap();
            state.queried_names.push(name.to_string());
            if state.cancel_service_check {
                state.stop.as_ref().unwrap().store(true, Ordering::SeqCst);
            }
            Ok(state.owners.iter().any(|owner| owner == name))
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            let mut state = self.state.lock().unwrap();
            state.calls.push((
                call.destination.to_string(),
                call.path.to_string(),
                call.interface.to_string(),
                call.member.to_string(),
            ));
            state.bodies.push(call.body.clone());

            if let Some(limit) = state.cancel_after_call {
                if state.calls.len() == limit {
                    if let Some(stop) = &state.stop {
                        stop.store(true, Ordering::SeqCst);
                    }
                }
            }

            if call.destination == DBUS_SERVICE_NAME
                && call.path == DBUS_OBJECT_PATH
                && call.interface == DBUS_INTERFACE
                && call.member == "GetNameOwner"
            {
                let [BusValue::String(name)] = call.body.as_slice() else {
                    return Err(SessionBusError::Transport(
                        "missing GetNameOwner name".to_string(),
                    ));
                };
                let owner = match name.as_str() {
                    GNOME_SCREEN_SAVER_NAME => state.screen_saver_owner.clone(),
                    GNOME_IDLE_MONITOR_NAME => state.idle_monitor_owner.clone(),
                    _ => None,
                };
                return owner
                    .map(|owner| BusReply::new(vec![BusValue::String(owner)]))
                    .ok_or_else(|| SessionBusError::Transport("no GNOME owner reply".to_string()));
            }

            if Some(call.destination) == state.idle_monitor_owner.as_deref()
                && call.path == GNOME_IDLE_MONITOR_PATH
                && call.interface == GNOME_IDLE_MONITOR_INTERFACE
                && call.member == "AddUserActiveWatch"
            {
                assert!(call.body.is_empty());
                if state.fail_watch {
                    return Err(SessionBusError::Transport("watch failed".to_string()));
                }
                if let Some(reply) = &state.watch_reply {
                    return Ok(BusReply::new(reply.clone()));
                }
                return Ok(BusReply::new(vec![BusValue::U32(state.watch_id)]));
            }

            if Some(call.destination) == state.screen_saver_owner.as_deref()
                && call.path == GNOME_SCREEN_SAVER_PATH
                && call.interface == GNOME_SCREEN_SAVER_INTERFACE
                && call.member == "GetActive"
            {
                if let Some(reply) = &state.active_reply {
                    return Ok(BusReply::new(reply.clone()));
                }
                if let Some(value) = state.active {
                    return Ok(BusReply::new(vec![BusValue::Bool(value)]));
                }
                return Err(SessionBusError::Transport("no GetActive reply".to_string()));
            }

            if Some(call.destination) == state.idle_monitor_owner.as_deref()
                && call.path == GNOME_IDLE_MONITOR_PATH
                && call.interface == GNOME_IDLE_MONITOR_INTERFACE
                && call.member == "GetIdletime"
            {
                if let Some(reply) = &state.idletime_reply {
                    return Ok(BusReply::new(reply.clone()));
                }
                if let Some(value) = state.idletime_ms {
                    return Ok(BusReply::new(vec![BusValue::U64(value)]));
                }
                return Err(SessionBusError::Transport(
                    "no GetIdletime reply".to_string(),
                ));
            }

            if Some(call.destination) == state.idle_monitor_owner.as_deref()
                && call.path == GNOME_IDLE_MONITOR_PATH
                && call.interface == GNOME_IDLE_MONITOR_INTERFACE
                && call.member == "RemoveWatch"
            {
                if state.fail_remove_watch {
                    return Err(SessionBusError::Transport("RemoveWatch failed".to_string()));
                }
                return Ok(BusReply::new(
                    state.remove_watch_reply.clone().unwrap_or_default(),
                ));
            }

            Err(SessionBusError::Transport(
                "unexpected GNOME readiness method call".to_string(),
            ))
        }

        fn add_signal_match(&mut self, _rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            let mut state = self.state.lock().unwrap();
            state.match_count += 1;
            if state.cancel_subscription {
                state.stop.as_ref().unwrap().store(true, Ordering::SeqCst);
            }
            if state.fail_subscriptions {
                return Err(SessionBusError::Transport(
                    "subscription failed".to_string(),
                ));
            }
            Ok(())
        }

        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            panic!("readiness must not enter a monitor loop");
        }
    }

    fn call_sequence(state: &Mutex<ReadyState>) -> Vec<(String, String, String, String)> {
        state.lock().unwrap().calls.clone()
    }

    fn assert_dropped(dropped: &Mutex<bool>) {
        assert!(
            *dropped.lock().unwrap(),
            "the owned observation client must be dropped"
        );
    }

    fn expected_calls() -> Vec<(String, String, String, String)> {
        vec![
            (
                DBUS_SERVICE_NAME.to_string(),
                DBUS_OBJECT_PATH.to_string(),
                DBUS_INTERFACE.to_string(),
                "GetNameOwner".to_string(),
            ),
            (
                ":1.42".to_string(),
                GNOME_IDLE_MONITOR_PATH.to_string(),
                GNOME_IDLE_MONITOR_INTERFACE.to_string(),
                "AddUserActiveWatch".to_string(),
            ),
            (
                DBUS_SERVICE_NAME.to_string(),
                DBUS_OBJECT_PATH.to_string(),
                DBUS_INTERFACE.to_string(),
                "GetNameOwner".to_string(),
            ),
            (
                ":1.41".to_string(),
                GNOME_SCREEN_SAVER_PATH.to_string(),
                GNOME_SCREEN_SAVER_INTERFACE.to_string(),
                "GetActive".to_string(),
            ),
            (
                ":1.42".to_string(),
                GNOME_IDLE_MONITOR_PATH.to_string(),
                GNOME_IDLE_MONITOR_INTERFACE.to_string(),
                "GetIdletime".to_string(),
            ),
            (
                ":1.42".to_string(),
                GNOME_IDLE_MONITOR_PATH.to_string(),
                GNOME_IDLE_MONITOR_INTERFACE.to_string(),
                "RemoveWatch".to_string(),
            ),
        ]
    }

    fn expected_bodies(watch_id: u32) -> Vec<Vec<BusValue>> {
        vec![
            vec![BusValue::String(GNOME_IDLE_MONITOR_NAME.to_string())],
            vec![],
            vec![BusValue::String(GNOME_SCREEN_SAVER_NAME.to_string())],
            vec![],
            vec![],
            vec![BusValue::U32(watch_id)],
        ]
    }

    #[test]
    fn readiness_success_traces_all_setup_reads_and_cleanup() {
        let (bus, state, dropped) = ready_bus();

        check_gnome_readiness_on(bus, &AtomicBool::new(false)).expect("readiness succeeds");

        assert_dropped(&dropped);
        assert_eq!(call_sequence(&state), expected_calls());
        assert_eq!(state.lock().unwrap().bodies.clone(), expected_bodies(7));
        assert_eq!(state.lock().unwrap().match_count, 3);
    }

    #[test]
    fn readiness_accepts_inactive_screen_saver_and_nonzero_idletime() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().active = Some(false);
        state.lock().unwrap().idletime_ms = Some(1_234);

        check_gnome_readiness_on(bus, &AtomicBool::new(false)).expect("readiness succeeds");

        assert_dropped(&dropped);
        assert_eq!(call_sequence(&state), expected_calls());
        assert_eq!(state.lock().unwrap().match_count, 3);
    }

    #[test]
    fn missing_required_service_is_unavailable_without_any_watch_or_read() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().owners.clear();

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::Unavailable)
        );
        assert_dropped(&dropped);
        assert!(
            call_sequence(&state).is_empty(),
            "no method calls before the service check"
        );
        assert_eq!(state.lock().unwrap().match_count, 0);
    }

    #[test]
    fn subscription_failure_is_reported_without_watch_or_read() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().fail_subscriptions = true;

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::Subscriptions)
        );
        assert_dropped(&dropped);
        assert!(call_sequence(&state).is_empty());
        assert_eq!(state.lock().unwrap().match_count, 1);
    }

    #[test]
    fn zero_watch_id_is_rejected_before_any_cleanup() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().watch_id = 0;

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::ActivityWatch)
        );
        assert_dropped(&dropped);
        assert!(!call_sequence(&state).iter().any(|call| {
            call.3 == "RemoveWatch" || call.3 == "GetActive" || call.3 == "GetIdletime"
        }));
    }

    #[test]
    fn activity_watch_owner_loss_is_rejected() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().idle_monitor_owner = None;

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::ActivityWatch)
        );
        assert_dropped(&dropped);
        assert_eq!(
            call_sequence(&state),
            vec![(
                DBUS_SERVICE_NAME.to_string(),
                DBUS_OBJECT_PATH.to_string(),
                DBUS_INTERFACE.to_string(),
                "GetNameOwner".to_string(),
            )]
        );
    }

    #[test]
    fn screen_saver_owner_failure_still_removes_the_valid_watch() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().screen_saver_owner = None;

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::ScreenSaver)
        );
        assert_dropped(&dropped);
        assert!(!call_sequence(&state)
            .iter()
            .any(|call| call.3 == "GetActive" || call.3 == "GetIdletime"));
        assert_eq!(
            call_sequence(&state)
                .iter()
                .filter(|call| call.3 == "RemoveWatch")
                .count(),
            1
        );
    }

    #[test]
    fn malformed_active_reply_is_screen_saver_error_with_cleanup() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().active_reply = Some(vec![BusValue::U64(1)]);

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::ScreenSaver)
        );
        assert_dropped(&dropped);
        assert!(
            !call_sequence(&state)
                .iter()
                .any(|call| call.3 == "GetIdletime"),
            "no later readiness read after a ScreenSaver failure"
        );
        assert_eq!(
            call_sequence(&state)
                .iter()
                .filter(|call| call.3 == "RemoveWatch")
                .count(),
            1
        );
    }

    #[test]
    fn idletime_failure_is_distinct_and_triggers_cleanup() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().idletime_reply = Some(vec![BusValue::U32(1)]);

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::IdleTime)
        );
        assert_dropped(&dropped);
        assert_eq!(
            call_sequence(&state)
                .iter()
                .filter(|call| call.3 == "RemoveWatch")
                .count(),
            1
        );
    }

    #[test]
    fn remove_watch_transport_failure_is_a_cleanup_error() {
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().fail_remove_watch = true;

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::Cleanup)
        );
        assert_dropped(&dropped);
        assert_eq!(
            call_sequence(&state)
                .iter()
                .filter(|call| call.3 == "RemoveWatch")
                .count(),
            1
        );
    }

    #[test]
    fn malformed_remove_watch_reply_is_a_cleanup_error() {
        let (bus, _state, dropped) = ready_bus();
        bus.state.lock().unwrap().remove_watch_reply = Some(vec![BusValue::U32(1)]);

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(false)),
            Err(GnomeReadinessError::Cleanup)
        );
        assert_dropped(&dropped);
    }

    #[test]
    fn pre_cancelled_readiness_performs_no_bus_calls_and_drops() {
        let (bus, state, dropped) = ready_bus();

        assert_eq!(
            check_gnome_readiness_on(bus, &AtomicBool::new(true)),
            Err(GnomeReadinessError::Cancelled)
        );
        assert_dropped(&dropped);
        assert!(call_sequence(&state).is_empty());
        assert!(state.lock().unwrap().queried_names.is_empty());
        assert_eq!(state.lock().unwrap().match_count, 0);
    }

    #[test]
    fn review_cancellation_wins_over_failed_pre_watch_phases() {
        for phase in [
            "services",
            "subscriptions",
            "owner",
            "watch",
            "shape",
            "zero",
        ] {
            let stop = Arc::new(AtomicBool::new(false));
            let (bus, state, dropped) = ready_bus();
            {
                let mut state = state.lock().unwrap();
                state.stop = Some(stop.clone());
                match phase {
                    "services" => {
                        state.owners.clear();
                        state.cancel_service_check = true;
                    }
                    "subscriptions" => {
                        state.fail_subscriptions = true;
                        state.cancel_subscription = true;
                    }
                    "owner" => {
                        state.idle_monitor_owner = None;
                        state.cancel_after_call = Some(1);
                    }
                    "watch" => {
                        state.fail_watch = true;
                        state.cancel_after_call = Some(2);
                    }
                    "shape" => {
                        state.watch_reply = Some(vec![BusValue::Bool(true)]);
                        state.cancel_after_call = Some(2);
                    }
                    "zero" => {
                        state.watch_id = 0;
                        state.cancel_after_call = Some(2);
                    }
                    _ => unreachable!(),
                }
            }
            assert_eq!(
                check_gnome_readiness_on(bus, &stop),
                Err(GnomeReadinessError::Cancelled),
                "phase {phase}"
            );
            assert_dropped(&dropped);
            assert!(!call_sequence(&state).iter().any(|call| matches!(
                call.3.as_str(),
                "GetActive" | "GetIdletime" | "RemoveWatch"
            )));
        }
    }

    #[test]
    fn review_invalid_watch_replies_and_transport_failure_are_rejected() {
        for transport_failure in [false, true] {
            let (bus, state, dropped) = ready_bus();
            state.lock().unwrap().fail_watch = transport_failure;
            state.lock().unwrap().watch_reply = Some(vec![BusValue::Bool(true)]);
            assert_eq!(
                check_gnome_readiness_on(bus, &AtomicBool::new(false)),
                Err(GnomeReadinessError::ActivityWatch)
            );
            assert_dropped(&dropped);
            assert_eq!(call_sequence(&state).len(), 2);
        }
    }

    #[test]
    fn review_primary_read_error_survives_cleanup_failure() {
        for screen_failure in [false, true] {
            let (bus, state, dropped) = ready_bus();
            {
                let mut state = state.lock().unwrap();
                state.fail_remove_watch = true;
                if screen_failure {
                    state.active = None;
                } else {
                    state.idletime_ms = None;
                }
            }
            assert_eq!(
                check_gnome_readiness_on(bus, &AtomicBool::new(false)),
                Err(if screen_failure {
                    GnomeReadinessError::ScreenSaver
                } else {
                    GnomeReadinessError::IdleTime
                })
            );
            assert_dropped(&dropped);
            let calls = call_sequence(&state);
            assert_eq!(calls.last().unwrap().3, "RemoveWatch");
            assert_eq!(
                calls.iter().filter(|call| call.3 == "RemoveWatch").count(),
                1
            );
            if screen_failure {
                assert!(!calls.iter().any(|call| call.3 == "GetIdletime"));
            }
        }
    }

    #[test]
    fn cancellation_after_watch_creation_still_removes_the_valid_watch() {
        let stop = Arc::new(AtomicBool::new(false));
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().cancel_after_call = Some(2);
        state.lock().unwrap().stop = Some(stop.clone());

        assert_eq!(
            check_gnome_readiness_on(bus, &stop),
            Err(GnomeReadinessError::Cancelled)
        );
        assert_dropped(&dropped);
        let calls = call_sequence(&state);
        assert_eq!(calls.len(), 3);
        assert!(!calls
            .iter()
            .any(|call| call.3 == "GetActive" || call.3 == "GetIdletime"));
        assert_eq!(
            calls.iter().filter(|call| call.3 == "RemoveWatch").count(),
            1
        );
    }

    #[test]
    fn cancellation_during_get_active_still_removes_the_valid_watch() {
        let stop = Arc::new(AtomicBool::new(false));
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().cancel_after_call = Some(4);
        state.lock().unwrap().stop = Some(stop.clone());

        assert_eq!(
            check_gnome_readiness_on(bus, &stop),
            Err(GnomeReadinessError::Cancelled)
        );
        assert_dropped(&dropped);
        let calls = call_sequence(&state);
        assert!(!calls.iter().any(|call| call.3 == "GetIdletime"));
        assert_eq!(
            calls.iter().filter(|call| call.3 == "RemoveWatch").count(),
            1
        );
    }

    #[test]
    fn cancellation_during_cleanup_wins_over_successful_readiness() {
        let stop = Arc::new(AtomicBool::new(false));
        let (bus, state, dropped) = ready_bus();
        state.lock().unwrap().cancel_after_call = Some(6);
        state.lock().unwrap().fail_remove_watch = true;
        state.lock().unwrap().stop = Some(stop.clone());

        assert_eq!(
            check_gnome_readiness_on(bus, &stop),
            Err(GnomeReadinessError::Cancelled)
        );
        assert_dropped(&dropped);
        assert_eq!(
            call_sequence(&state)
                .iter()
                .filter(|call| call.3 == "RemoveWatch")
                .count(),
            1
        );
    }

    #[test]
    fn fixed_error_messages_do_not_leak_transport_or_reply_values() {
        for error in [
            GnomeReadinessError::Cancelled,
            GnomeReadinessError::Unavailable,
            GnomeReadinessError::Subscriptions,
            GnomeReadinessError::ActivityWatch,
            GnomeReadinessError::ScreenSaver,
            GnomeReadinessError::IdleTime,
            GnomeReadinessError::Cleanup,
        ] {
            let display = error.to_string();
            assert!(!display.contains(":1."), "no owner names leak: {display}");
            assert!(
                !display.contains("simulated"),
                "no transport strings leak: {display}"
            );
            assert!(
                !display.contains("queued"),
                "no reply strings leak: {display}"
            );
            let debug = format!("{error:?}");
            assert!(
                !debug.contains(":1."),
                "Debug carries no owner names: {debug}"
            );
        }
    }

    #[test]
    fn success_trace_reads_screen_saver_state_via_unique_owner() {
        // Guards against a service-name-only readiness stub: without resolving
        // the unique owners and reading state, this trace cannot succeed.
        let (bus, state, dropped) = ready_bus();

        check_gnome_readiness_on(bus, &AtomicBool::new(false)).expect("readiness succeeds");

        assert_dropped(&dropped);
        assert!(
            call_sequence(&state)
                .iter()
                .any(|call| call.3 == "GetActive" && call.0 == ":1.41"),
            "GetActive must target the ScreenSaver unique owner, not just the named service"
        );
    }
}
