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
pub struct InactivityEngine {
    thresholds: InactivityThresholds,
    blanked_by_lg_buddy: bool,
    provider_idle: bool,
}

impl InactivityEngine {
    pub fn new(thresholds: InactivityThresholds) -> Self {
        Self {
            thresholds,
            blanked_by_lg_buddy: false,
            provider_idle: false,
        }
    }

    pub fn thresholds(&self) -> InactivityThresholds {
        self.thresholds
    }

    pub fn blanked_by_lg_buddy(&self) -> bool {
        self.blanked_by_lg_buddy
    }

    pub fn set_blanked_by_lg_buddy(&mut self, value: bool) {
        self.blanked_by_lg_buddy = value;
    }

    pub fn observe(&mut self, observation: InactivityObservation) -> InactivityDecision {
        match observation {
            InactivityObservation::ProviderIdle => {
                self.provider_idle = true;
                if self.blanked_by_lg_buddy {
                    InactivityDecision::NoOp
                } else {
                    self.blanked_by_lg_buddy = true;
                    InactivityDecision::BlankNow
                }
            }
            InactivityObservation::IdleTimeMs(idletime_ms) => {
                if !self.blanked_by_lg_buddy
                    && (self.provider_idle || idletime_ms >= self.thresholds.blank_threshold_ms)
                {
                    self.blanked_by_lg_buddy = true;
                    InactivityDecision::BlankNow
                } else if self.blanked_by_lg_buddy
                    && !self.provider_idle
                    && idletime_ms < self.thresholds.active_threshold_ms
                {
                    self.blanked_by_lg_buddy = false;
                    InactivityDecision::RestoreNow
                } else {
                    InactivityDecision::NoOp
                }
            }
            InactivityObservation::ProviderActive => {
                self.provider_idle = false;
                if self.blanked_by_lg_buddy {
                    self.blanked_by_lg_buddy = false;
                    InactivityDecision::RestoreNow
                } else {
                    InactivityDecision::NoOp
                }
            }
            InactivityObservation::WakeRequested | InactivityObservation::UserActivityObserved => {
                self.provider_idle = false;
                if self.blanked_by_lg_buddy {
                    self.blanked_by_lg_buddy = false;
                    InactivityDecision::RestoreNow
                } else {
                    InactivityDecision::NoOp
                }
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
        assert!(!engine.blanked_by_lg_buddy());

        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(5_000)),
            InactivityDecision::BlankNow
        );
        assert!(engine.blanked_by_lg_buddy());
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
        assert!(engine.blanked_by_lg_buddy());
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
        assert!(engine.blanked_by_lg_buddy());

        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(999)),
            InactivityDecision::RestoreNow
        );
        assert!(!engine.blanked_by_lg_buddy());
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
        assert!(!engine.blanked_by_lg_buddy());
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
        assert!(!engine.blanked_by_lg_buddy());
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
        assert!(!engine.blanked_by_lg_buddy());
    }

    #[test]
    fn provider_idle_blanks_immediately() {
        let mut engine = test_engine();

        assert_eq!(
            engine.observe(InactivityObservation::ProviderIdle),
            InactivityDecision::BlankNow
        );
        assert!(engine.blanked_by_lg_buddy());
    }

    #[test]
    fn provider_idle_is_a_first_class_blank_source() {
        let mut engine = test_engine();

        assert_eq!(
            engine.observe(InactivityObservation::ProviderIdle),
            InactivityDecision::BlankNow
        );
        assert!(engine.blanked_by_lg_buddy());
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
        assert!(engine.blanked_by_lg_buddy());
    }

    #[test]
    fn restore_signals_are_noops_before_lg_buddy_has_blanked_screen() {
        let mut engine = test_engine();

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
        assert_eq!(
            engine.observe(InactivityObservation::IdleTimeMs(250)),
            InactivityDecision::NoOp
        );
        assert!(!engine.blanked_by_lg_buddy());
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

    #[test]
    fn blanked_state_can_be_synchronized_externally() {
        let mut engine = test_engine();
        engine.set_blanked_by_lg_buddy(true);
        assert!(engine.blanked_by_lg_buddy());

        engine.set_blanked_by_lg_buddy(false);
        assert!(!engine.blanked_by_lg_buddy());
    }
}
