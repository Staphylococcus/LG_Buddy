mod support;

use lg_buddy::inhibition::{
    evaluate_pull_inhibition, evaluate_push_inhibition, Inhibition, InhibitionStatus,
    PullInhibitionAdapter, PushInhibitionAdapter,
};
use lg_buddy::sources::desktop::gnome::inhibition::GnomeInhibition;
use lg_buddy::sources::desktop::powerdevil::PowerDevilInhibition;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use support::{MockPowerDevil, MockSessionBusIdleMonitor, TestEnv};

// Run the production capability against a private bus, without activity or TV
// machinery. Drop stops the worker even if an assertion fails.
struct RunningInhibition {
    adapter: Arc<GnomeInhibition>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl RunningInhibition {
    fn start() -> Self {
        let adapter = Arc::new(GnomeInhibition::default());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_adapter = Arc::clone(&adapter);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || worker_adapter.run(&worker_stop));
        Self {
            adapter,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for RunningInhibition {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[track_caller]
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "inhibition condition timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn independent_inhibition_capabilities_work_without_activity_services() {
    let mut env = TestEnv::new();
    let bus = MockSessionBusIdleMonitor::new("inhibition-capability");
    // libdbus caches the session-bus address process-wide. Exercise all scenarios
    // on one private bus; each gets a fresh worker, and the fixture is cleaned up.
    env.set("DBUS_SESSION_BUS_ADDRESS", bus.address());
    a_late_source_is_discovered_and_loss_drops_its_contribution(&bus);
    playback_inhibitors_release_only_when_all_end(&bus);
    permission_reads_do_not_wait_for_dbus_and_a_quiet_worker_can_stop(&bus);
    powerdevil_queries_current_permission_and_recovers(&bus);
    powerdevil_discards_delayed_cancelled_and_obsolete_replies(&bus);
    composed_gate_uses_both_capabilities_and_the_observed_release_clock(&bus);
}

fn composed_gate_uses_both_capabilities_and_the_observed_release_clock(
    bus: &MockSessionBusIdleMonitor,
) {
    let mut config = lg_buddy::config::parse_config(
        "tv_ip=192.168.1.42\ntv_mac=aa:bb:cc:dd:ee:ff\ninput=HDMI_1\nscreen_honor_idle_inhibitors=enabled\n"
    ).unwrap();
    let push = Arc::new(GnomeInhibition::default());
    let service = MockPowerDevil::new(bus.address());
    bus.set_idle_inhibitor_count(1);
    service.set_inhibited(true);
    let delay = Duration::from_secs(10);
    let mut gate = Inhibition::new(
        &config,
        delay,
        vec![push.clone()],
        vec![Box::new(PowerDevilInhibition::default())],
    );
    wait_until(|| !push.evaluate().allowed);
    let check = |gate: &mut Inhibition, now| {
        let mut allowed = false;
        wait_until(|| {
            allowed = gate.can_blank(now);
            gate.diagnostics().unwrap().pull.is_some()
        });
        let diagnostic = gate.diagnostics().unwrap();
        assert_eq!(diagnostic.can_blank, allowed);
        assert_eq!(diagnostic.push.as_ref().unwrap().contributions.len(), 1);
        assert_eq!(diagnostic.pull.as_ref().unwrap().contributions.len(), 1);
        allowed
    };
    let now = Instant::now();
    assert!(!check(&mut gate, now));
    bus.schedule_idle_inhibitor_count(Duration::ZERO, 0);
    wait_until(|| push.evaluate().allowed);
    assert!(!check(&mut gate, now + delay), "PowerDevil still inhibits");
    service.set_inhibited(false);
    // The next successful query records PowerDevil's real observation time.
    gate.cancel();
    assert!(!check(&mut gate, Instant::now()));
    let release_deadline = gate.diagnostics().unwrap().release_not_before.unwrap();
    assert!(check(&mut gate, release_deadline));
    assert_eq!(
        gate.diagnostics().unwrap().release_not_before,
        Some(release_deadline),
        "clear queries do not restart the clock"
    );
    service.set_inhibited(true);
    assert!(!check(&mut gate, release_deadline + Duration::from_secs(1)));
    let before = service.query_count();
    config.screen_honor_idle_inhibitors =
        lg_buddy::config::ScreenHonorIdleInhibitorsPolicy::Disabled;
    gate.configure(&config, delay);
    assert!(gate.can_blank(Instant::now()));
    assert_eq!(
        service.query_count(),
        before,
        "override does not need a successful pull"
    );
    drop(gate);
}

fn powerdevil_queries_current_permission_and_recovers(bus: &MockSessionBusIdleMonitor) {
    let cancelled = AtomicBool::new(false);
    let mut adapter = PowerDevilInhibition::default();
    let absent = adapter.query(&cancelled).unwrap();
    assert!(absent.allowed);
    assert_eq!(absent.diagnostics.status, InhibitionStatus::Absent);

    let service = MockPowerDevil::new(bus.address());
    // Playback already active when the first request arrives.
    service.set_inhibited(true);
    assert!(
        !evaluate_pull_inhibition(&mut [&mut adapter], &cancelled)
            .unwrap()
            .allowed
    );
    // The fixture supplies effective policy, including overlapping requests or
    // a user suppressing them in Plasma. It emits no change notifications.
    for inhibited in [true, false, false, true] {
        service.set_inhibited(inhibited);
        let previous_queries = service.query_count();
        let result = evaluate_pull_inhibition(&mut [&mut adapter], &cancelled).unwrap();
        assert_eq!(result.allowed, !inhibited);
        assert_eq!(result.contributions.len(), 1);
        assert_eq!(result.contributions[0].diagnostics.source, "powerdevil");
        assert_eq!(service.query_count(), previous_queries + 1);
    }
    let release = adapter
        .query(&cancelled)
        .unwrap()
        .diagnostics
        .last_release_at;
    assert!(release.is_some());
    service.fail_next_query();
    let failed = adapter.query(&cancelled).unwrap();
    assert!(failed.allowed);
    assert!(matches!(
        failed.diagnostics.status,
        InhibitionStatus::Unavailable(_)
    ));
    service.set_inhibited(false);
    let recovered = adapter.query(&cancelled).unwrap();
    assert_eq!(recovered.diagnostics.status, InhibitionStatus::Clear);
    assert_eq!(recovered.diagnostics.last_release_at, release);
    drop(service);
    assert_eq!(
        adapter.query(&cancelled).unwrap().diagnostics.status,
        InhibitionStatus::Absent
    );
}

fn powerdevil_discards_delayed_cancelled_and_obsolete_replies(bus: &MockSessionBusIdleMonitor) {
    let cancelled = AtomicBool::new(false);
    let mut adapter = PowerDevilInhibition::default();
    let service = MockPowerDevil::new(bus.address());
    service.delay_next_query(Duration::from_millis(400));
    thread::scope(|scope| {
        let worker = scope.spawn(|| evaluate_pull_inhibition(&mut [&mut adapter], &cancelled));
        wait_until(|| service.query_count() == 1);
        // The caller is free while the source is replying. Cancelling discards
        // the delayed clear response; it cannot authorize this or a later attempt.
        cancelled.store(true, Ordering::SeqCst);
        assert!(worker.join().unwrap().is_none());
    });
    cancelled.store(false, Ordering::SeqCst);
    service.set_inhibited(true);
    assert!(!adapter.query(&cancelled).unwrap().allowed);

    // A reply arriving after the transport timeout cannot answer the next check.
    service.set_inhibited(false);
    service.delay_next_query(Duration::from_millis(1200));
    let timed_out = adapter.query(&cancelled).unwrap();
    assert!(matches!(
        timed_out.diagnostics.status,
        InhibitionStatus::Unavailable(_)
    ));
    service.set_inhibited(true);
    assert!(!adapter.query(&cancelled).unwrap().allowed);

    service.delay_next_query(Duration::from_millis(400));
    service.set_inhibited(false);
    let queries = service.query_count();
    let replacement = thread::scope(|scope| {
        let worker = scope.spawn(|| adapter.query(&cancelled));
        wait_until(|| service.query_count() > queries);
        let replacement = MockPowerDevil::new(bus.address());
        replacement.set_inhibited(true);
        let obsolete = worker.join().unwrap().unwrap();
        assert!(matches!(
            obsolete.diagnostics.status,
            InhibitionStatus::Unavailable(_)
        ));
        assert_eq!(obsolete.diagnostics.last_release_at, None);
        replacement
    });
    assert!(!adapter.query(&cancelled).unwrap().allowed);
    replacement.set_inhibited(false);
    assert!(adapter.query(&cancelled).unwrap().allowed);
}

fn playback_inhibitors_release_only_when_all_end(bus: &MockSessionBusIdleMonitor) {
    // SessionManager alone: Shell, Mutter and ScreenSaver remain absent.
    bus.set_idle_inhibitor_count(2);
    let running = RunningInhibition::start();
    let adapter = &*running.adapter;
    wait_until(|| adapter.evaluate().diagnostics.status == InhibitionStatus::Inhibited);
    assert!(!evaluate_push_inhibition(&[adapter]).allowed);

    let before = adapter.evaluate().diagnostics.observed_at;
    bus.schedule_idle_inhibitor_count(Duration::ZERO, 1);
    wait_until(|| adapter.evaluate().diagnostics.observed_at != before);
    assert!(!evaluate_push_inhibition(&[adapter]).allowed);
    assert_eq!(adapter.evaluate().diagnostics.last_release_at, None);

    bus.schedule_idle_inhibitor_count(Duration::ZERO, 0);
    wait_until(|| adapter.evaluate().allowed);
    let clear = adapter.evaluate();
    assert!(clear.diagnostics.last_release_at.is_some());
    let queries = bus.inhibition_query_count();
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        adapter.evaluate(),
        clear,
        "quiet subscriptions retain state"
    );
    assert_eq!(
        bus.inhibition_query_count(),
        queries,
        "quiet sources are not polled before reconciliation is due"
    );

    // Playback starting after monitoring began blocks the same contribution.
    bus.schedule_idle_inhibitor_count(Duration::ZERO, 1);
    wait_until(|| adapter.evaluate().diagnostics.status == InhibitionStatus::Inhibited);
    assert!(!evaluate_push_inhibition(&[adapter]).allowed);
}

fn a_late_source_is_discovered_and_loss_drops_its_contribution(bus: &MockSessionBusIdleMonitor) {
    let running = RunningInhibition::start();
    let adapter = &*running.adapter;
    wait_until(|| adapter.evaluate().diagnostics.status == InhibitionStatus::Absent);
    assert!(adapter.evaluate().allowed);

    bus.set_idle_inhibitor_count(1);
    wait_until(|| adapter.evaluate().diagnostics.status == InhibitionStatus::Inhibited);
    let inhibited = adapter.evaluate();
    bus.set_session_manager_available(false);
    wait_until(|| {
        matches!(
            adapter.evaluate().diagnostics.status,
            InhibitionStatus::Unavailable(_)
        )
    });
    assert!(adapter.evaluate().allowed);
    assert_eq!(adapter.evaluate().diagnostics.last_release_at, None);
    assert_eq!(
        adapter.evaluate().diagnostics.observed_at,
        inhibited.diagnostics.observed_at
    );

    // Reconnect updates the same long-lived capability from current state.
    bus.set_idle_inhibitor_count(0);
    wait_until(|| adapter.evaluate().diagnostics.status == InhibitionStatus::Clear);
    assert!(adapter.evaluate().allowed);
    assert_eq!(adapter.evaluate().diagnostics.last_release_at, None);
}

fn permission_reads_do_not_wait_for_dbus_and_a_quiet_worker_can_stop(
    bus: &MockSessionBusIdleMonitor,
) {
    bus.set_idle_inhibitor_count(1);
    bus.delay_next_inhibited_query(Duration::from_millis(800));
    let queries_before = bus.inhibition_query_count();
    let running = RunningInhibition::start();
    wait_until(|| bus.inhibition_query_count() > queries_before);
    let started = Instant::now();
    let evaluation = running.adapter.evaluate();
    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(evaluation.allowed);
    assert!(matches!(
        evaluation.diagnostics.status,
        InhibitionStatus::Unavailable(_)
    ));
    wait_until(|| running.adapter.evaluate().diagnostics.status == InhibitionStatus::Inhibited);
    let adapter = Arc::clone(&running.adapter);
    let started = Instant::now();
    drop(running);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(adapter.evaluate().allowed);
    assert!(matches!(
        adapter.evaluate().diagnostics.status,
        InhibitionStatus::Unavailable(_)
    ));
}
