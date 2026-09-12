//! Inhibition capabilities are independent of activity tracking. Push reads
//! maintained permission; pull requests current permission. Neither acts on TVs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::{Config, ScreenHonorIdleInhibitorsPolicy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InhibitionPreferenceDiagnostics {
    pub honoring: ScreenHonorIdleInhibitorsPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InhibitionPreferenceEvaluation {
    /// Overrides source restrictions and release delay, never activity eligibility.
    /// This is an override, not a third permission to AND with the source sections.
    pub bypass_inhibition: bool,
    pub diagnostics: InhibitionPreferenceDiagnostics,
}

/// Evaluate the preference from the runtime's loaded configuration, without
/// source queries or retained state. Disabled honoring bypasses inhibition;
/// enabled honoring leaves source permission and release delay to the reconciler.
///
/// Settings apply through a screen-service restart. A new runtime evaluates its
/// newly loaded Config; old attempts cannot survive that process boundary. For
/// an in-process reload, the reconciler must cancel the pending attempt before
/// using the new configuration and must not reuse this result across attempts.
pub fn evaluate_inhibition_preference(config: &Config) -> InhibitionPreferenceEvaluation {
    let honoring = config.screen_honor_idle_inhibitors;
    InhibitionPreferenceEvaluation {
        bypass_inhibition: !honoring.is_enabled(),
        diagnostics: InhibitionPreferenceDiagnostics { honoring },
    }
}

/// Only observed inhibition denies permission; other statuses are diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InhibitionStatus {
    Absent,
    Unavailable(String),
    Clear,
    Inhibited,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InhibitionDiagnostics {
    pub source: &'static str,
    pub status: InhibitionStatus,
    /// Time of the last confirmed state query, not the time diagnostics are read.
    pub observed_at: Option<Instant>,
    /// Last observed inhibited -> clear transition. Repeated clear queries and
    /// recovery after source loss do not create a release.
    pub last_release_at: Option<Instant>,
}

/// Permission and diagnostics captured together from the same observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InhibitionEvaluation {
    pub allowed: bool,
    pub diagnostics: InhibitionDiagnostics,
}

/// A source-owned worker maintains the capability; evaluating it is a quick
/// local read. Connection recovery and event ordering stay inside the adapter.
pub trait PushInhibitionAdapter: Send + Sync {
    fn run(&self, stop: &AtomicBool);
    fn evaluate(&self) -> InhibitionEvaluation;
}

/// A source-owned, blocking check, suitable for execution on a worker. Each call
/// queries current state; it never returns cached permission. Exclusive access
/// orders requests, and cancellation discards the reply rather than granting a
/// verdict. Protocol I/O and owner validation remain inside the adapter.
pub trait PullInhibitionAdapter: Send {
    fn query(&mut self, cancelled: &AtomicBool) -> Option<InhibitionEvaluation>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InhibitionSectionEvaluation {
    pub allowed: bool,
    pub contributions: Vec<InhibitionEvaluation>,
}

/// Combine independent inhibition capabilities. Evaluate every contributor so
/// diagnostics describe the same evaluation, even when an earlier source denies.
pub fn evaluate_push_inhibition(
    adapters: &[&dyn PushInhibitionAdapter],
) -> InhibitionSectionEvaluation {
    let contributions: Vec<_> = adapters.iter().map(|adapter| adapter.evaluate()).collect();
    InhibitionSectionEvaluation {
        allowed: contributions.iter().all(|result| result.allowed),
        contributions,
    }
}

/// Query every pull contributor for this attempt, including when another
/// denies permission. Run on a worker: cancellation is checked between bounded
/// adapter calls and before returning. A cancelled attempt has no verdict, and
/// completed permission must not be reused for a later blank attempt.
pub fn evaluate_pull_inhibition(
    adapters: &mut [&mut dyn PullInhibitionAdapter],
    cancelled: &AtomicBool,
) -> Option<InhibitionSectionEvaluation> {
    let mut contributions = Vec::with_capacity(adapters.len());
    for adapter in adapters {
        if cancelled.load(Ordering::SeqCst) {
            return None;
        }
        contributions.push(adapter.query(cancelled)?);
    }
    if cancelled.load(Ordering::SeqCst) {
        return None;
    }
    Some(InhibitionSectionEvaluation {
        allowed: contributions.iter().all(|result| result.allowed),
        contributions,
    })
}

/// The Boolean gate and its supporting evidence, captured together. A missing
/// section was not evaluated on this call (bypass, pending work or retry delay).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlankingEvaluation {
    pub can_blank: bool,
    pub preference: InhibitionPreferenceEvaluation,
    pub push: Option<InhibitionSectionEvaluation>,
    pub pull: Option<InhibitionSectionEvaluation>,
    pub checking: bool,
    pub release_not_before: Option<Instant>,
}

fn reconcile(
    preference: InhibitionPreferenceEvaluation,
    push: Option<InhibitionSectionEvaluation>,
    pull: Option<InhibitionSectionEvaluation>,
    release_delay: Duration,
    now: Instant,
    checking: bool,
) -> BlankingEvaluation {
    // Adapter history already distinguishes a release from a clear poll or
    // recovery. Do not infer another transition from aggregate permission.
    let release_not_before = push
        .iter()
        .chain(pull.iter())
        .flat_map(|section| &section.contributions)
        .filter_map(|result| result.diagnostics.last_release_at)
        .max()
        .and_then(|released_at| released_at.checked_add(release_delay));
    let can_blank = preference.bypass_inhibition
        || (push.as_ref().is_some_and(|section| section.allowed)
            && pull.as_ref().is_some_and(|section| section.allowed)
            && release_not_before.is_none_or(|deadline| now >= deadline));
    BlankingEvaluation {
        can_blank,
        preference,
        push,
        pull,
        checking,
        release_not_before,
    }
}

struct PullRequest {
    cancelled: Arc<AtomicBool>,
    reply: mpsc::Sender<Option<InhibitionSectionEvaluation>>,
}

struct PendingCheck {
    cancelled: Arc<AtomicBool>,
    reply: mpsc::Receiver<Option<InhibitionSectionEvaluation>>,
    push: InhibitionSectionEvaluation,
}

/// Long-lived inhibition subsystem. The caller polls a quick Boolean gate only
/// when idle, and cancels the attempt when activity or runtime eligibility changes.
/// No protocol I/O runs on that caller's thread.
pub struct Inhibition {
    preference: InhibitionPreferenceEvaluation,
    release_delay: Duration,
    push: Vec<Arc<dyn PushInhibitionAdapter>>,
    requests: Option<mpsc::SyncSender<PullRequest>>,
    pending: Option<PendingCheck>,
    retry_at: Option<Instant>,
    diagnostics: Option<BlankingEvaluation>,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl Inhibition {
    const RETRY_INTERVAL: Duration = Duration::from_secs(1);

    pub fn new(
        config: &Config,
        release_delay: Duration,
        push: Vec<Arc<dyn PushInhibitionAdapter>>,
        mut pull: Vec<Box<dyn PullInhibitionAdapter>>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut workers: Vec<_> = push
            .iter()
            .map(|adapter| {
                let adapter = Arc::clone(adapter);
                let stop = Arc::clone(&stop);
                thread::spawn(move || adapter.run(&stop))
            })
            .collect();
        // One worker retains the adapters' diagnostic history; each attempt has
        // its own reply channel. Cancelled replies cannot answer a later attempt.
        // The bounded queue also limits work if input repeatedly cancels checks.
        let (requests, receiver) = mpsc::sync_channel::<PullRequest>(1);
        let worker_stop = Arc::clone(&stop);
        workers.push(thread::spawn(move || {
            while let Ok(request) = receiver.recv() {
                if worker_stop.load(Ordering::SeqCst) {
                    break;
                }
                let mut adapters: Vec<&mut dyn PullInhibitionAdapter> = pull
                    .iter_mut()
                    .map(|adapter| adapter.as_mut() as &mut dyn PullInhibitionAdapter)
                    .collect();
                let result = evaluate_pull_inhibition(&mut adapters, &request.cancelled);
                let _ = request.reply.send(result);
            }
        }));
        Self {
            preference: evaluate_inhibition_preference(config),
            release_delay,
            push,
            requests: Some(requests),
            pending: None,
            retry_at: None,
            diagnostics: None,
            stop,
            workers,
        }
    }

    /// Settings currently restart the service. This also makes an in-process
    /// preference/timeout change safe: discard old work before using new policy.
    pub fn configure(&mut self, config: &Config, release_delay: Duration) {
        let preference = evaluate_inhibition_preference(config);
        if self.preference != preference || self.release_delay != release_delay {
            self.cancel();
            self.preference = preference;
            self.release_delay = release_delay;
        }
    }

    pub fn cancel(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.cancelled.store(true, Ordering::SeqCst);
        }
        self.retry_at = None;
        self.diagnostics = None;
    }

    pub fn diagnostics(&self) -> Option<&BlankingEvaluation> {
        self.diagnostics.as_ref()
    }

    pub fn can_blank(&mut self, now: Instant) -> bool {
        if self.preference.bypass_inhibition {
            self.cancel();
            return self.record(None, None, now, false);
        }
        let adapters: Vec<_> = self.push.iter().map(|adapter| adapter.as_ref()).collect();
        let push = evaluate_push_inhibition(&adapters);
        if self.pending.as_ref().is_some_and(|pending| {
            // Refreshing the same Boolean does not invalidate a check. A source
            // change or intervening observed release does.
            pending
                .push
                .contributions
                .iter()
                .zip(&push.contributions)
                .any(|(old, new)| {
                    old.allowed != new.allowed
                        || old.diagnostics.status != new.diagnostics.status
                        || old.diagnostics.last_release_at != new.diagnostics.last_release_at
                })
        }) {
            self.cancel();
        }
        if let Some(pending) = &self.pending {
            match pending.reply.try_recv() {
                Ok(pull) => {
                    self.pending = None;
                    self.retry_at = Some(now + Self::RETRY_INTERVAL);
                    return self.record(Some(push), pull, now, false);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    return self.record(Some(push), None, now, true);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.cancel();
                    self.retry_at = Some(now + Self::RETRY_INTERVAL);
                }
            }
        }
        if self.retry_at.is_none_or(|deadline| now >= deadline) {
            let (reply, receiver) = mpsc::channel();
            let cancelled = Arc::new(AtomicBool::new(false));
            let request = PullRequest {
                cancelled: Arc::clone(&cancelled),
                reply,
            };
            if self.requests.as_ref().unwrap().try_send(request).is_ok() {
                self.pending = Some(PendingCheck {
                    cancelled,
                    reply: receiver,
                    push: push.clone(),
                });
            }
            self.retry_at = Some(now + Self::RETRY_INTERVAL);
        }
        self.record(Some(push), None, now, self.pending.is_some())
    }

    fn record(
        &mut self,
        push: Option<InhibitionSectionEvaluation>,
        pull: Option<InhibitionSectionEvaluation>,
        now: Instant,
        checking: bool,
    ) -> bool {
        let evaluation = reconcile(
            self.preference,
            push,
            pull,
            self.release_delay,
            now,
            checking,
        );
        let allowed = evaluation.can_blank;
        self.diagnostics = Some(evaluation);
        allowed
    }
}

impl Drop for Inhibition {
    fn drop(&mut self) {
        self.cancel();
        self.stop.store(true, Ordering::SeqCst);
        self.requests.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Adapter-owned evidence. Source loss drops its inhibition contribution without
/// inventing an observed release.
pub(crate) struct InhibitionState {
    diagnostics: InhibitionDiagnostics,
}

impl InhibitionState {
    pub(crate) fn new(source: &'static str) -> Self {
        Self {
            diagnostics: InhibitionDiagnostics {
                source,
                status: InhibitionStatus::Unavailable("monitoring has not started".into()),
                observed_at: None,
                last_release_at: None,
            },
        }
    }

    pub(crate) fn unavailable(&mut self, reason: impl std::fmt::Display) {
        self.diagnostics.status =
            InhibitionStatus::Unavailable(reason.to_string().chars().take(512).collect());
    }

    pub(crate) fn absent(&mut self) {
        self.diagnostics.status = InhibitionStatus::Absent;
    }

    pub(crate) fn observe(&mut self, inhibited: bool, at: Instant) {
        if !inhibited && self.diagnostics.status == InhibitionStatus::Inhibited {
            self.diagnostics.last_release_at = Some(at);
        }
        self.diagnostics.observed_at = Some(at);
        self.diagnostics.status = if inhibited {
            InhibitionStatus::Inhibited
        } else {
            InhibitionStatus::Clear
        };
    }

    pub(crate) fn evaluate(&self) -> InhibitionEvaluation {
        InhibitionEvaluation {
            allowed: self.diagnostics.status != InhibitionStatus::Inhibited,
            diagnostics: self.diagnostics.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{parse_config, ScreenBackend, ScreenIdleBlankPolicy};
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn preference_uses_the_runtime_config_default_and_effective_value() {
        for (setting, honoring, bypass) in [
            ("", ScreenHonorIdleInhibitorsPolicy::Disabled, true),
            (
                "screen_honor_idle_inhibitors=enabled",
                ScreenHonorIdleInhibitorsPolicy::Enabled,
                false,
            ),
            (
                "screen_honor_idle_inhibitors=disabled",
                ScreenHonorIdleInhibitorsPolicy::Disabled,
                true,
            ),
            // Keep the runtime loader's existing invalid-value fallback. The
            // settings editor still rejects invalid new writes independently.
            (
                "screen_honor_idle_inhibitors=invalid",
                ScreenHonorIdleInhibitorsPolicy::Disabled,
                true,
            ),
        ] {
            let config = parse_config(&format!(
                "tv_ip=192.168.1.42\ntv_mac=aa:bb:cc:dd:ee:ff\ninput=HDMI_1\n{setting}\n"
            ))
            .unwrap();
            let result = evaluate_inhibition_preference(&config);
            assert_eq!(result.bypass_inhibition, bypass);
            assert_eq!(result.diagnostics.honoring, honoring);
        }
    }

    #[test]
    fn preference_has_no_desktop_or_activity_policy_gate() {
        let mut config =
            parse_config("tv_ip=192.168.1.42\ntv_mac=aa:bb:cc:dd:ee:ff\ninput=HDMI_1\n").unwrap();
        for backend in [
            ScreenBackend::Auto,
            ScreenBackend::Gnome,
            ScreenBackend::Wayland,
            ScreenBackend::Swayidle,
        ] {
            config.screen_backend = backend;
            for idle_blank in [
                ScreenIdleBlankPolicy::Enabled,
                ScreenIdleBlankPolicy::Disabled,
            ] {
                config.screen_idle_blank = idle_blank;
                for (honoring, bypass) in [
                    (ScreenHonorIdleInhibitorsPolicy::Disabled, true),
                    (ScreenHonorIdleInhibitorsPolicy::Enabled, false),
                ] {
                    config.screen_honor_idle_inhibitors = honoring;
                    let result = evaluate_inhibition_preference(&config);
                    assert_eq!(result.bypass_inhibition, bypass);
                    assert_eq!(result.diagnostics.honoring, honoring);
                }
            }
        }
    }

    struct FakeAdapter(Mutex<InhibitionState>);

    impl FakeAdapter {
        fn new(name: &'static str) -> Self {
            Self(Mutex::new(InhibitionState::new(name)))
        }
    }

    impl PushInhibitionAdapter for FakeAdapter {
        fn run(&self, _: &AtomicBool) {
            // The fake publishes directly into its maintained state.
        }

        fn evaluate(&self) -> InhibitionEvaluation {
            self.0.lock().unwrap().evaluate()
        }
    }

    fn enabled_config() -> Config {
        parse_config("tv_ip=192.168.1.42\ntv_mac=aa:bb:cc:dd:ee:ff\ninput=HDMI_1\nscreen_honor_idle_inhibitors=enabled\n").unwrap()
    }

    fn section(state: &InhibitionState) -> InhibitionSectionEvaluation {
        let result = state.evaluate();
        InhibitionSectionEvaluation {
            allowed: result.allowed,
            contributions: vec![result],
        }
    }

    #[test]
    fn reconciliation_combines_sources_and_only_observed_releases_delay_blanking() {
        let config = enabled_config();
        let preference = evaluate_inhibition_preference(&config);
        let start = Instant::now();
        let delay = Duration::from_secs(10);
        let mut push = InhibitionState::new("push");
        let mut pull = InhibitionState::new("pull");
        let evaluate = |push: &InhibitionState, pull: &InhibitionState, now| {
            reconcile(
                preference,
                Some(section(push)),
                Some(section(pull)),
                delay,
                now,
                false,
            )
        };
        assert!(
            evaluate(&push, &pull, start).can_blank,
            "no observed inhibitors"
        );
        for push_inhibited in [false, true] {
            for pull_inhibited in [false, true] {
                push.observe(push_inhibited, start);
                pull.observe(pull_inhibited, start);
                assert_eq!(
                    evaluate(&push, &pull, start + delay).can_blank,
                    !push_inhibited && !pull_inhibited
                );
            }
        }
        push.observe(true, start);
        pull.observe(true, start);
        push.observe(false, start + Duration::from_secs(1));
        assert!(
            !evaluate(&push, &pull, start + delay * 2).can_blank,
            "overlapping inhibitor still active"
        );
        pull.observe(false, start + Duration::from_secs(4));
        let deadline = start + Duration::from_secs(14);
        let denied = evaluate(&push, &pull, deadline - Duration::from_nanos(1));
        assert!(!denied.can_blank);
        assert_eq!(denied.release_not_before, Some(deadline));
        // Repeated clear replies and a delayed completion do not restart delay.
        pull.observe(false, start + Duration::from_secs(12));
        assert!(evaluate(&push, &pull, deadline).can_blank);
        assert!(evaluate(&push, &pull, deadline + delay).can_blank);
        // Failure drops known inhibition; clear recovery is not a release.
        push.observe(true, deadline);
        push.unavailable("query failed");
        pull.absent();
        assert!(evaluate(&push, &pull, deadline).can_blank);
        push.observe(false, deadline);
        assert!(evaluate(&push, &pull, deadline).can_blank);
    }

    struct ControlledPull {
        calls: mpsc::Sender<Arc<AtomicBool>>,
        replies: mpsc::Receiver<InhibitionEvaluation>,
    }

    impl PullInhibitionAdapter for ControlledPull {
        fn query(&mut self, cancelled: &AtomicBool) -> Option<InhibitionEvaluation> {
            // Deliberately complete even after cancellation. The section and
            // facade must discard this reply independently of adapter goodwill.
            let seen_cancel = Arc::new(AtomicBool::new(false));
            self.calls.send(Arc::clone(&seen_cancel)).unwrap();
            let reply = self.replies.recv_timeout(Duration::from_secs(2)).unwrap();
            seen_cancel.store(cancelled.load(Ordering::SeqCst), Ordering::SeqCst);
            Some(reply)
        }
    }

    fn finish_check(gate: &mut Inhibition, now: Instant) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let allowed = gate.can_blank(now);
            if !gate.diagnostics().unwrap().checking {
                return allowed;
            }
            assert!(Instant::now() < deadline, "worker completion timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn gate_discards_cancelled_answers_bounds_retries_and_never_reuses_permission() {
        let (calls, called) = mpsc::channel();
        let (reply, replies) = mpsc::channel();
        let config = enabled_config();
        let mut gate = Inhibition::new(
            &config,
            Duration::from_secs(5),
            vec![],
            vec![Box::new(ControlledPull { calls, replies })],
        );
        let now = Instant::now();
        let mut source = InhibitionState::new("pull");
        source.observe(false, now);
        assert!(!gate.can_blank(now));
        let cancelled = called.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!gate.can_blank(now)); // nonblocking while adapter waits
        assert!(gate.diagnostics().unwrap().checking);
        gate.cancel(); // activity or lifecycle invalidates the attempt
        reply.send(source.evaluate()).unwrap();
        assert!(!gate.can_blank(now));
        let _ = called.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(cancelled.load(Ordering::SeqCst));
        source.observe(true, now);
        reply.send(source.evaluate()).unwrap();
        assert!(!finish_check(&mut gate, now));
        for _ in 0..100 {
            assert!(!gate.can_blank(now + Duration::from_millis(999)));
        }
        assert!(called.try_recv().is_err(), "no busy query retry");
        let next = now + Duration::from_secs(1);
        assert!(!gate.can_blank(next));
        let _ = called.recv_timeout(Duration::from_secs(1)).unwrap();
        // An absent source is neutral and does not manufacture a release.
        source.absent();
        reply.send(source.evaluate()).unwrap();
        assert!(finish_check(&mut gate, next));
        assert!(
            !gate.can_blank(next),
            "permission was consumed by its attempt"
        );
    }

    #[test]
    fn preference_and_push_changes_invalidate_in_flight_checks() {
        let (calls, called) = mpsc::channel();
        let (reply, replies) = mpsc::channel();
        let mut config = enabled_config();
        let push = Arc::new(FakeAdapter::new("push"));
        let now = Instant::now();
        push.0.lock().unwrap().observe(false, now);
        let mut gate = Inhibition::new(
            &config,
            Duration::from_secs(5),
            vec![push.clone()],
            vec![Box::new(ControlledPull { calls, replies })],
        );
        let mut pull = InhibitionState::new("pull");
        pull.observe(false, now);
        assert!(!gate.can_blank(now));
        let cancelled = called.recv_timeout(Duration::from_secs(1)).unwrap();
        push.0.lock().unwrap().observe(true, now);
        assert!(!gate.can_blank(now));
        reply.send(pull.evaluate()).unwrap();
        let cancelled_by_config = called.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(cancelled.load(Ordering::SeqCst));
        config.screen_honor_idle_inhibitors = ScreenHonorIdleInhibitorsPolicy::Disabled;
        gate.configure(&config, Duration::from_secs(5));
        assert!(
            gate.can_blank(now),
            "disabled honoring bypasses pending work"
        );
        assert!(gate.diagnostics().unwrap().push.is_none());
        reply.send(pull.evaluate()).unwrap();
        config.screen_honor_idle_inhibitors = ScreenHonorIdleInhibitorsPolicy::Enabled;
        gate.configure(&config, Duration::from_secs(5));
        assert!(!gate.can_blank(now));
        let _ = called.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(cancelled_by_config.load(Ordering::SeqCst));
        reply.send(pull.evaluate()).unwrap();
        assert!(!finish_check(&mut gate, now), "push inhibitor still blocks");

        push.0.lock().unwrap().observe(false, now);
        // A timeout edit changes release delay, without inventing a release.
        gate.configure(&config, Duration::ZERO);
        assert!(!gate.can_blank(now));
        let _ = called.recv_timeout(Duration::from_secs(1)).unwrap();
        reply.send(pull.evaluate()).unwrap();
        assert!(finish_check(&mut gate, now));
    }

    #[test]
    fn only_observed_inhibition_denies_permission() {
        let mut state = InhibitionState::new("test");
        assert!(state.evaluate().allowed);
        state.absent();
        assert!(state.evaluate().allowed);
        assert_eq!(
            state.evaluate().diagnostics.status,
            InhibitionStatus::Absent
        );
        for inhibited in [true, false] {
            state.observe(inhibited, Instant::now());
            let result = state.evaluate();
            assert_eq!(result.allowed, !inhibited);
            assert_eq!(
                result.diagnostics.status == InhibitionStatus::Inhibited,
                inhibited
            );
        }
        state.observe(true, Instant::now());
        assert!(!state.evaluate().allowed);
        state.unavailable("connection lost");
        assert!(state.evaluate().allowed);
        state.observe(true, Instant::now());
        state.absent();
        assert!(state.evaluate().allowed);
    }

    #[test]
    fn release_history_records_only_observed_transitions() {
        let now = Instant::now();
        let mut state = InhibitionState::new("test");
        state.observe(false, now);
        state.observe(false, now + Duration::from_secs(1));
        assert_eq!(state.evaluate().diagnostics.last_release_at, None);
        state.observe(true, now + Duration::from_secs(2));
        state.observe(true, now + Duration::from_secs(3));
        assert_eq!(state.evaluate().diagnostics.last_release_at, None);
        let released = now + Duration::from_secs(4);
        state.observe(false, released);
        let quiet = state.evaluate();
        assert_eq!(quiet, state.evaluate());
        state.observe(false, now + Duration::from_secs(5));
        assert_eq!(state.evaluate().diagnostics.last_release_at, Some(released));
        state.observe(true, now + Duration::from_secs(6));
        state.unavailable("connection lost");
        assert!(state.evaluate().allowed);
        state.observe(false, now + Duration::from_secs(7));
        assert_eq!(state.evaluate().diagnostics.last_release_at, Some(released));
    }

    #[test]
    fn section_combines_independent_sources_without_losing_other_denials() {
        let first = FakeAdapter::new("first");
        let second = FakeAdapter::new("second");
        for first_inhibited in [false, true] {
            for second_inhibited in [false, true] {
                first
                    .0
                    .lock()
                    .unwrap()
                    .observe(first_inhibited, Instant::now());
                second
                    .0
                    .lock()
                    .unwrap()
                    .observe(second_inhibited, Instant::now());
                let result = evaluate_push_inhibition(&[&first, &second]);
                assert_eq!(result.allowed, !first_inhibited && !second_inhibited);
                assert_eq!(result.contributions, [first.evaluate(), second.evaluate()]);
            }
        }
        first.0.lock().unwrap().absent();
        assert!(!evaluate_push_inhibition(&[&first, &second]).allowed);
        second.0.lock().unwrap().unavailable("refresh failed");
        assert!(evaluate_push_inhibition(&[&first, &second]).allowed);
        first.0.lock().unwrap().observe(true, Instant::now());
        assert!(!evaluate_push_inhibition(&[&first, &second]).allowed);
        first.0.lock().unwrap().observe(false, Instant::now());
        second.0.lock().unwrap().observe(false, Instant::now());
        assert!(evaluate_push_inhibition(&[&first, &second]).allowed);
        assert!(evaluate_push_inhibition(&[]).allowed);
    }

    #[test]
    fn failure_diagnostics_are_bounded() {
        let mut state = InhibitionState::new("test");
        state.unavailable("é".repeat(1000));
        let InhibitionStatus::Unavailable(reason) = state.evaluate().diagnostics.status else {
            panic!("expected unavailable");
        };
        assert_eq!(reason.chars().count(), 512);
    }

    struct FakePull {
        state: InhibitionState,
        queries: usize,
        cancel: bool,
    }

    impl FakePull {
        fn new(source: &'static str) -> Self {
            Self {
                state: InhibitionState::new(source),
                queries: 0,
                cancel: false,
            }
        }
    }

    impl PullInhibitionAdapter for FakePull {
        fn query(&mut self, cancelled: &AtomicBool) -> Option<InhibitionEvaluation> {
            self.queries += 1;
            if self.cancel {
                cancelled.store(true, Ordering::SeqCst);
            }
            Some(self.state.evaluate())
        }
    }

    #[test]
    fn pull_section_queries_every_source_on_every_attempt_with_matching_diagnostics() {
        let cancelled = AtomicBool::new(false);
        let mut first = FakePull::new("first");
        let mut second = FakePull::new("second");
        for (index, (a, b)) in [(false, false), (true, false), (true, true), (false, true)]
            .into_iter()
            .enumerate()
        {
            first.state.observe(a, Instant::now());
            second.state.observe(b, Instant::now());
            let result =
                evaluate_pull_inhibition(&mut [&mut first, &mut second], &cancelled).unwrap();
            assert_eq!(result.allowed, !a && !b);
            assert_eq!(
                result.contributions,
                [first.state.evaluate(), second.state.evaluate()]
            );
            assert_eq!((first.queries, second.queries), (index + 1, index + 1));
        }
        first.state.absent();
        assert!(
            !evaluate_pull_inhibition(&mut [&mut first, &mut second], &cancelled)
                .unwrap()
                .allowed
        );
        second.state.unavailable("failed current check");
        let result = evaluate_pull_inhibition(&mut [&mut first, &mut second], &cancelled).unwrap();
        assert!(result.allowed);
        assert_eq!(
            result.contributions,
            [first.state.evaluate(), second.state.evaluate()]
        );
        assert!(
            evaluate_pull_inhibition(&mut [], &cancelled)
                .unwrap()
                .allowed
        );
    }

    #[test]
    fn cancelled_pull_sections_have_no_verdict_or_further_queries() {
        let cancelled = AtomicBool::new(true);
        let mut first = FakePull::new("first");
        let mut second = FakePull::new("second");
        assert!(evaluate_pull_inhibition(&mut [&mut first], &cancelled).is_none());
        assert!(evaluate_pull_inhibition(&mut [], &cancelled).is_none());
        assert_eq!(first.queries, 0);

        cancelled.store(false, Ordering::SeqCst);
        first.cancel = true;
        assert!(evaluate_pull_inhibition(&mut [&mut first, &mut second], &cancelled).is_none());
        assert_eq!((first.queries, second.queries), (1, 0));

        cancelled.store(false, Ordering::SeqCst);
        // Even cancellation during the last adapter discards the whole verdict.
        assert!(evaluate_pull_inhibition(&mut [&mut first], &cancelled).is_none());
    }
}
