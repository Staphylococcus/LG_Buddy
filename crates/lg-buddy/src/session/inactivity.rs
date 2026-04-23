#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactivityObservation {
    IdleTimeMs(u64),
    ProviderIdle,
    ProviderActive,
    WakeRequested,
    UserActivityObserved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InactivityDecision {
    BlankNow,
    RestoreNow,
    NoOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InactivityThresholds {
    pub blank_threshold_ms: u64,
    pub active_threshold_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InactivityPhase {
    Unknown,
    Active,
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InactivityEngine {
    thresholds: InactivityThresholds,
    phase: InactivityPhase,
    provider_idle: bool,
}

impl InactivityEngine {
    pub fn new(thresholds: InactivityThresholds) -> Self {
        Self {
            thresholds,
            phase: InactivityPhase::Unknown,
            provider_idle: false,
        }
    }

    pub fn thresholds(&self) -> InactivityThresholds {
        self.thresholds
    }

    fn activate_from_signal(&mut self) -> InactivityDecision {
        if self.phase == InactivityPhase::Active {
            InactivityDecision::NoOp
        } else {
            self.phase = InactivityPhase::Active;
            InactivityDecision::RestoreNow
        }
    }

    fn observe_idletime(&mut self, idletime_ms: u64) -> InactivityDecision {
        if self.provider_idle {
            return InactivityDecision::NoOp;
        }

        if idletime_ms >= self.thresholds.blank_threshold_ms {
            match self.phase {
                InactivityPhase::Unknown | InactivityPhase::Active => {
                    self.phase = InactivityPhase::Idle;
                    InactivityDecision::BlankNow
                }
                InactivityPhase::Idle => InactivityDecision::NoOp,
            }
        } else if idletime_ms < self.thresholds.active_threshold_ms {
            match self.phase {
                InactivityPhase::Idle => {
                    self.phase = InactivityPhase::Active;
                    InactivityDecision::RestoreNow
                }
                InactivityPhase::Unknown => {
                    self.phase = InactivityPhase::Active;
                    InactivityDecision::NoOp
                }
                InactivityPhase::Active => InactivityDecision::NoOp,
            }
        } else {
            InactivityDecision::NoOp
        }
    }

    pub fn observe(&mut self, observation: InactivityObservation) -> InactivityDecision {
        match observation {
            InactivityObservation::ProviderIdle => {
                self.provider_idle = true;
                if self.phase == InactivityPhase::Idle {
                    InactivityDecision::NoOp
                } else {
                    self.phase = InactivityPhase::Idle;
                    InactivityDecision::BlankNow
                }
            }
            InactivityObservation::IdleTimeMs(idletime_ms) => self.observe_idletime(idletime_ms),
            InactivityObservation::ProviderActive => {
                self.provider_idle = false;
                self.activate_from_signal()
            }
            InactivityObservation::WakeRequested | InactivityObservation::UserActivityObserved => {
                self.provider_idle = false;
                self.activate_from_signal()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InactivityDecision, InactivityEngine, InactivityObservation, InactivityThresholds,
    };

    fn test_engine() -> InactivityEngine {
        InactivityEngine::new(InactivityThresholds {
            blank_threshold_ms: 5_000,
            active_threshold_ms: 1_000,
        })
    }

    #[test]
    fn idle_threshold_crossing_blanks_once() {
        let mut engine = test_engine();

        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(4_999)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(5_000)),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn remaining_above_idle_threshold_does_not_blank_repeatedly() {
        let mut engine = test_engine();

        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(5_000)),
            InactivityDecision::BlankNow
        );
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(6_000)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(7_500)),
            InactivityDecision::NoOp
        );
    }

    #[test]
    fn fresh_activity_below_active_threshold_restores_after_blank() {
        let mut engine = test_engine();
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(5_000)),
            InactivityDecision::BlankNow
        );

        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(1_500)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(999)),
            InactivityDecision::RestoreNow
        );
    }

    #[test]
    fn wake_request_restores_after_blank() {
        let mut engine = test_engine();
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(5_000)),
            InactivityDecision::BlankNow
        );

        assert_eq!(
            engine.observe(InactivityObservation::WakeRequested),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe(InactivityObservation::WakeRequested),
            InactivityDecision::NoOp
        );
    }

    #[test]
    fn provider_active_restores_after_blank() {
        let mut engine = test_engine();
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(5_000)),
            InactivityDecision::BlankNow
        );

        assert_eq!(
            engine.observe(InactivityObservation::ProviderActive),
            InactivityDecision::RestoreNow
        );
    }

    #[test]
    fn observed_user_activity_restores_after_blank() {
        let mut engine = test_engine();
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(5_000)),
            InactivityDecision::BlankNow
        );

        assert_eq!(
            engine.observe(InactivityObservation::UserActivityObserved),
            InactivityDecision::RestoreNow
        );
    }

    #[test]
    fn provider_idle_blanks_immediately() {
        let mut engine = test_engine();

        assert_eq!(
            engine.observe(InactivityObservation::ProviderIdle),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn provider_idle_is_a_first_class_blank_source() {
        let mut engine = test_engine();

        assert_eq!(
            engine.observe(InactivityObservation::ProviderIdle),
            InactivityDecision::BlankNow
        );
        assert_eq!(
            engine.observe(InactivityObservation::ProviderIdle),
            InactivityDecision::NoOp
        );
    }

    #[test]
    fn idletime_drop_does_not_restore_while_provider_still_reports_idle() {
        let mut engine = test_engine();
        assert_eq!(
            engine.observe(InactivityObservation::ProviderIdle),
            InactivityDecision::BlankNow
        );

        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(0)),
            InactivityDecision::NoOp
        );
    }

    #[test]
    fn restore_signals_are_noops_after_engine_is_already_active() {
        let mut engine = test_engine();
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(250)),
            InactivityDecision::NoOp
        );

        assert_eq!(
            engine.observe(InactivityObservation::ProviderActive),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe(InactivityObservation::WakeRequested),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe(InactivityObservation::UserActivityObserved),
            InactivityDecision::NoOp
        );
    }

    #[test]
    fn low_idletime_seeds_active_state_without_restoring() {
        let mut engine = test_engine();

        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(999)),
            InactivityDecision::NoOp
        );
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(5_000)),
            InactivityDecision::BlankNow
        );
    }

    #[test]
    fn provider_active_restores_when_engine_starts_unknown() {
        let mut engine = test_engine();

        assert_eq!(
            engine.observe(InactivityObservation::ProviderActive),
            InactivityDecision::RestoreNow
        );
        assert_eq!(
            engine.observe(InactivityObservation::ProviderActive),
            InactivityDecision::NoOp
        );
    }

    #[test]
    fn thresholds_are_reported_verbatim() {
        let engine = test_engine();

        assert_eq!(
            engine.thresholds(),
            InactivityThresholds {
                blank_threshold_ms: 5_000,
                active_threshold_ms: 1_000,
            }
        );
    }
}
