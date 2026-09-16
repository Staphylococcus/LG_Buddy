//! Backend-owned setup steps and their shared onboarding flow.

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use crate::presentation::brightness::UserFacingError;

pub mod assessment;
pub mod cli;
mod environment;
pub mod flow;
pub mod gui;
pub(crate) mod kwin;
pub(crate) mod lock;
pub(crate) mod pairing;
pub(crate) mod provision;
pub(crate) mod services;
mod terminal_signals;

/// Domain errors are normalized by the step, never interpreted by its caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepFailure {
    pub presentation: UserFacingError,
    pub diagnostic: String,
    pub retryable: bool,
}

/// The same outcomes are used for inspection and execution. A command exiting
/// successfully is not Complete until the step has verified its resulting state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepResponse {
    Complete,
    NotApplicable,
    InputRequired(StepInput),
    ActionRequired {
        explanation: &'static str,
        requires_authorization: bool,
    },
    Running {
        message: &'static str,
        cancelable: bool,
    },
    Cancelled,
    Blocked(StepFailure),
    Failed(StepFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepInput {
    Pairing {
        saved: Option<crate::pairing::PairingRequest>,
    },
    BuildDependencies {
        explanation: &'static str,
    },
}

const AVAILABLE: u8 = 0;
const CANCELLED: u8 = 1;
const RUNNING: u8 = 2;
const FINISHED: u8 = 3;

/// One attempt's cancellation boundary, not a lock for the whole setup flow.
/// Service activation becomes noncancelable once execution begins.
#[derive(Debug, Clone)]
pub struct StepCancellation(Arc<AtomicU8>);

impl Default for StepCancellation {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(AVAILABLE)))
    }
}

impl StepCancellation {
    pub fn can_cancel(&self) -> bool {
        self.0.load(Ordering::Acquire) == AVAILABLE
    }

    /// False means rejection; the request is not queued for later.
    pub fn cancel(&self) -> bool {
        self.0
            .compare_exchange(AVAILABLE, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == CANCELLED
    }

    pub(crate) fn begin(&self) -> bool {
        self.0
            .compare_exchange(AVAILABLE, RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn finish(&self) {
        self.0.store(FINISHED, Ordering::Release);
    }

    pub(crate) fn same_attempt(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
