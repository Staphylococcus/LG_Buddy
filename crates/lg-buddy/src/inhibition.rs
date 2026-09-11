//! Inhibition capabilities are independent of activity tracking. Push reads
//! maintained permission; pull requests current permission. Neither acts on TVs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

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
    use std::sync::Mutex;
    use std::time::Duration;

    struct FakeAdapter(Mutex<InhibitionState>);

    impl FakeAdapter {
        fn new(name: &'static str) -> Self {
            Self(Mutex::new(InhibitionState::new(name)))
        }
    }

    impl PushInhibitionAdapter for FakeAdapter {
        fn run(&self, _: &AtomicBool) {
            panic!("section evaluation must not run an adapter or perform I/O");
        }

        fn evaluate(&self) -> InhibitionEvaluation {
            self.0.lock().unwrap().evaluate()
        }
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
