use std::time::{Duration, Instant};

use super::AxisRange;

const PPM_DENOMINATOR: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivityPolicy {
    pub axis_movement_threshold_ppm: u32,
    pub minimum_axis_movement: i32,
    pub quiet_baseline_after: Duration,
    pub activity_cooldown: Duration,
}

impl Default for ActivityPolicy {
    fn default() -> Self {
        Self {
            axis_movement_threshold_ppm: 40_000,
            minimum_axis_movement: 2,
            quiet_baseline_after: Duration::from_secs(2),
            activity_cooldown: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AxisActivityState {
    pub baseline: i32,
    pub last_value: i32,
    pub baseline_updated_at: Instant,
    pub last_activity_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityDecision {
    Activity,
    NoActivity,
}

impl ActivityDecision {
    pub(crate) fn is_activity(self) -> bool {
        self == Self::Activity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AxisActivityEvaluation {
    pub next_state: AxisActivityState,
    pub decision: ActivityDecision,
}

pub(crate) fn evaluate_button_event() -> ActivityDecision {
    ActivityDecision::Activity
}

pub(crate) fn evaluate_axis_event(
    policy: &ActivityPolicy,
    previous: Option<&AxisActivityState>,
    value: i32,
    range: AxisRange,
    now: Instant,
) -> AxisActivityEvaluation {
    let Some(previous) = previous else {
        return AxisActivityEvaluation {
            next_state: AxisActivityState {
                baseline: value,
                last_value: value,
                baseline_updated_at: now,
                last_activity_at: None,
            },
            decision: ActivityDecision::NoActivity,
        };
    };

    let threshold = movement_threshold(policy, range);
    let delta_from_baseline = i64::from(value)
        .saturating_sub(i64::from(previous.baseline))
        .abs();
    let mut next_state = AxisActivityState {
        last_value: value,
        ..*previous
    };

    if delta_from_baseline >= threshold {
        next_state.baseline = value;
        next_state.baseline_updated_at = now;

        if activity_cooldown_elapsed(policy, previous.last_activity_at, now) {
            next_state.last_activity_at = Some(now);
            return AxisActivityEvaluation {
                next_state,
                decision: ActivityDecision::Activity,
            };
        }

        return AxisActivityEvaluation {
            next_state,
            decision: ActivityDecision::NoActivity,
        };
    }

    if elapsed_since(now, previous.baseline_updated_at) >= policy.quiet_baseline_after {
        next_state.baseline = value;
        next_state.baseline_updated_at = now;
    }

    AxisActivityEvaluation {
        next_state,
        decision: ActivityDecision::NoActivity,
    }
}

fn activity_cooldown_elapsed(
    policy: &ActivityPolicy,
    last_activity_at: Option<Instant>,
    now: Instant,
) -> bool {
    last_activity_at
        .map(|last_activity_at| elapsed_since(now, last_activity_at) >= policy.activity_cooldown)
        .unwrap_or(true)
}

fn movement_threshold(policy: &ActivityPolicy, range: AxisRange) -> i64 {
    let span = i64::from(range.maximum)
        .saturating_sub(i64::from(range.minimum))
        .abs();
    let range_threshold =
        span.saturating_mul(i64::from(policy.axis_movement_threshold_ppm)) / PPM_DENOMINATOR;
    let hardware_noise = i64::from(range.flat)
        .abs()
        .max(i64::from(range.fuzz).abs().saturating_mul(2));
    let minimum = i64::from(policy.minimum_axis_movement).max(1);

    range_threshold
        .max(hardware_noise.saturating_add(1))
        .max(minimum)
}

fn elapsed_since(now: Instant, earlier: Instant) -> Duration {
    now.checked_duration_since(earlier).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{
        evaluate_axis_event, evaluate_button_event, ActivityDecision, ActivityPolicy,
        AxisActivityState,
    };
    use crate::session::gamepad::AxisRange;
    use std::time::{Duration, Instant};

    fn test_policy() -> ActivityPolicy {
        ActivityPolicy {
            axis_movement_threshold_ppm: 100_000,
            minimum_axis_movement: 2,
            quiet_baseline_after: Duration::from_secs(2),
            activity_cooldown: Duration::from_millis(500),
        }
    }

    fn test_range() -> AxisRange {
        AxisRange {
            minimum: 0,
            maximum: 100,
            flat: 0,
            fuzz: 0,
        }
    }

    #[test]
    fn button_events_are_activity() {
        assert_eq!(evaluate_button_event(), ActivityDecision::Activity);
    }

    #[test]
    fn first_axis_sample_seeds_baseline_without_activity() {
        let now = Instant::now();

        let evaluation = evaluate_axis_event(&test_policy(), None, 42, test_range(), now);

        assert_eq!(evaluation.decision, ActivityDecision::NoActivity);
        assert_eq!(
            evaluation.next_state,
            AxisActivityState {
                baseline: 42,
                last_value: 42,
                baseline_updated_at: now,
                last_activity_at: None,
            }
        );
    }

    #[test]
    fn significant_axis_movement_produces_activity_and_reanchors_baseline() {
        let started = Instant::now();
        let previous =
            evaluate_axis_event(&test_policy(), None, 40, test_range(), started).next_state;
        let now = started + Duration::from_millis(100);

        let evaluation =
            evaluate_axis_event(&test_policy(), Some(&previous), 55, test_range(), now);

        assert_eq!(evaluation.decision, ActivityDecision::Activity);
        assert_eq!(evaluation.next_state.baseline, 55);
        assert_eq!(evaluation.next_state.last_activity_at, Some(now));
    }

    #[test]
    fn small_axis_jitter_is_ignored() {
        let started = Instant::now();
        let previous =
            evaluate_axis_event(&test_policy(), None, 40, test_range(), started).next_state;

        let evaluation = evaluate_axis_event(
            &test_policy(),
            Some(&previous),
            45,
            test_range(),
            started + Duration::from_millis(100),
        );

        assert_eq!(evaluation.decision, ActivityDecision::NoActivity);
        assert_eq!(evaluation.next_state.baseline, 40);
    }

    #[test]
    fn arbitrary_non_center_axis_position_becomes_resting_baseline() {
        let started = Instant::now();
        let mut state =
            evaluate_axis_event(&test_policy(), None, 87, test_range(), started).next_state;

        for offset in 1..5 {
            let evaluation = evaluate_axis_event(
                &test_policy(),
                Some(&state),
                87,
                test_range(),
                started + Duration::from_millis(offset * 100),
            );
            assert_eq!(evaluation.decision, ActivityDecision::NoActivity);
            state = evaluation.next_state;
        }

        assert_eq!(state.baseline, 87);
        assert_eq!(state.last_activity_at, None);
    }

    #[test]
    fn movement_away_from_arbitrary_resting_position_counts_as_activity() {
        let started = Instant::now();
        let previous =
            evaluate_axis_event(&test_policy(), None, 87, test_range(), started).next_state;

        let evaluation = evaluate_axis_event(
            &test_policy(),
            Some(&previous),
            70,
            test_range(),
            started + Duration::from_millis(100),
        );

        assert_eq!(evaluation.decision, ActivityDecision::Activity);
    }

    #[test]
    fn quiet_axis_values_converge_to_new_baseline_after_quiet_period() {
        let started = Instant::now();
        let previous =
            evaluate_axis_event(&test_policy(), None, 40, test_range(), started).next_state;

        let evaluation = evaluate_axis_event(
            &test_policy(),
            Some(&previous),
            45,
            test_range(),
            started + Duration::from_secs(3),
        );

        assert_eq!(evaluation.decision, ActivityDecision::NoActivity);
        assert_eq!(evaluation.next_state.baseline, 45);
    }

    #[test]
    fn slow_intentional_movement_accumulates_until_threshold_is_crossed() {
        let started = Instant::now();
        let mut state =
            evaluate_axis_event(&test_policy(), None, 40, test_range(), started).next_state;

        for (idx, value) in [43, 46, 49].into_iter().enumerate() {
            let evaluation = evaluate_axis_event(
                &test_policy(),
                Some(&state),
                value,
                test_range(),
                started + Duration::from_millis((idx as u64 + 1) * 100),
            );
            assert_eq!(evaluation.decision, ActivityDecision::NoActivity);
            state = evaluation.next_state;
        }

        let evaluation = evaluate_axis_event(
            &test_policy(),
            Some(&state),
            51,
            test_range(),
            started + Duration::from_millis(500),
        );

        assert_eq!(evaluation.decision, ActivityDecision::Activity);
    }

    #[test]
    fn cooldown_suppresses_repeated_activity_pulses() {
        let started = Instant::now();
        let mut state =
            evaluate_axis_event(&test_policy(), None, 0, test_range(), started).next_state;

        let first = evaluate_axis_event(
            &test_policy(),
            Some(&state),
            20,
            test_range(),
            started + Duration::from_millis(100),
        );
        assert_eq!(first.decision, ActivityDecision::Activity);
        state = first.next_state;

        let second = evaluate_axis_event(
            &test_policy(),
            Some(&state),
            35,
            test_range(),
            started + Duration::from_millis(200),
        );
        assert_eq!(second.decision, ActivityDecision::NoActivity);
        state = second.next_state;

        let third = evaluate_axis_event(
            &test_policy(),
            Some(&state),
            50,
            test_range(),
            started + Duration::from_millis(700),
        );
        assert_eq!(third.decision, ActivityDecision::Activity);
    }

    #[test]
    fn range_flat_and_fuzz_raise_the_movement_threshold() {
        let started = Instant::now();
        let range = AxisRange {
            minimum: 0,
            maximum: 100,
            flat: 20,
            fuzz: 5,
        };
        let previous = evaluate_axis_event(&test_policy(), None, 40, range, started).next_state;

        let evaluation = evaluate_axis_event(
            &test_policy(),
            Some(&previous),
            55,
            range,
            started + Duration::from_millis(100),
        );

        assert_eq!(evaluation.decision, ActivityDecision::NoActivity);
    }
}
