pub(crate) mod actions;
pub mod gamepad;
pub mod inactivity;
pub mod runner;

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

use std::time::Instant;

use crate::events::EventSource;
use crate::session::inactivity::InactivityObservation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    Idle,
    Active,
    WakeRequested,
    BeforeSleep,
    AfterResume,
    Lock,
    Unlock,
    UserActivity,
}

impl SessionEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
            Self::WakeRequested => "wake-requested",
            Self::BeforeSleep => "before-sleep",
            Self::AfterResume => "after-resume",
            Self::Lock => "lock",
            Self::Unlock => "unlock",
            Self::UserActivity => "user-activity",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionObservation {
    Event {
        event: SessionEvent,
        source: EventSource,
        observed_at: Instant,
    },
    Inactivity {
        observation: InactivityObservation,
        source: EventSource,
        observed_at: Instant,
    },
    /// Desktop permission for automatic blanking; never user activity or a
    /// request to restore the screen.
    IdleBlankingPermission {
        allowed: bool,
        source: EventSource,
        observed_at: Instant,
    },
    /// Suspend automatic blanking while the source refreshes permission.
    /// This is not an inhibitor transition and must not renew the deadline.
    IdleBlankingPermissionPending { source: EventSource },
}
