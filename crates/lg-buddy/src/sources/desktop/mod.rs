pub mod gnome;
pub mod powerdevil;
pub mod swayidle;
pub mod wayland;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use crate::session::SessionObservation;

pub(crate) type ActivityPublisher = Arc<dyn Fn(SessionObservation) + Send + Sync>;

/// Whether an adapter can currently observe activity. This is an assessment for
/// idle policy and diagnostics, never permission to deliver an observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivityStatus {
    Available,
    Unavailable(String),
}

impl Default for ActivityStatus {
    fn default() -> Self {
        Self::Unavailable("activity monitoring has not started".to_string())
    }
}

impl ActivityStatus {
    fn unavailable(reason: impl std::fmt::Display) -> Self {
        Self::Unavailable(reason.to_string().chars().take(512).collect())
    }

    pub(crate) fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// Runs for the application's lifetime. Connection recovery and protocol
/// validation stay inside the adapter; emitted observations are already valid.
pub(crate) trait ActivityAdapter: Send + Sync {
    fn run(&self, publish: ActivityPublisher, stop: &AtomicBool);
    fn status(&self) -> ActivityStatus;
}

fn wait_for_retry(stop: &AtomicBool) {
    for _ in 0..20 {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::ActivityStatus;

    #[test]
    fn unavailable_diagnostics_are_bounded() {
        let ActivityStatus::Unavailable(reason) = ActivityStatus::unavailable("x".repeat(1000))
        else {
            panic!("expected unavailable status");
        };
        assert_eq!(reason.len(), 512);
    }
}
