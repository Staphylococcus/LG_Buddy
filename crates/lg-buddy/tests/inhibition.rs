mod support;

use lg_buddy::inhibition::{evaluate_push_inhibition, InhibitionStatus, PushInhibitionAdapter};
use lg_buddy::sources::desktop::gnome::inhibition::GnomeInhibition;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use support::{MockSessionBusIdleMonitor, TestEnv};

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
fn gnome_push_inhibition_works_without_activity_services() {
    let mut env = TestEnv::new();
    let bus = MockSessionBusIdleMonitor::new("inhibition-capability");
    // libdbus caches the session-bus address process-wide. Exercise all scenarios
    // on one private bus; each gets a fresh worker, and the fixture is cleaned up.
    env.set("DBUS_SESSION_BUS_ADDRESS", bus.address());
    a_late_source_is_discovered_and_loss_drops_its_contribution(&bus);
    playback_inhibitors_release_only_when_all_end(&bus);
    permission_reads_do_not_wait_for_dbus_and_a_quiet_worker_can_stop(&bus);
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
