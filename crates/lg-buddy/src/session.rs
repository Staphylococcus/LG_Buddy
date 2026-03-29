use std::error::Error;
use std::fmt;

use crate::config::ScreenBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    Idle,
    Active,
    WakeRequested,
    UserActivity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionBackendCapabilities {
    pub early_user_activity: bool,
}

pub trait SessionBackend {
    fn backend(&self) -> ScreenBackend;
    fn capabilities(&self) -> Result<SessionBackendCapabilities, SessionBackendError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionBackendError {
    Unavailable {
        backend: ScreenBackend,
        reason: &'static str,
    },
}

impl fmt::Display for SessionBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable { backend, reason } => {
                write!(f, "backend `{}` is unavailable: {reason}", backend.as_str())
            }
        }
    }
}

impl Error for SessionBackendError {}
