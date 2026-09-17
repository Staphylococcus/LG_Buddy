//! Read-only setup health, composed from the same inspections as onboarding.
use super::{
    environment::{NativeSteps, SetupContext},
    flow::{inspect_steps, satisfied, AuthorizationMode, SetupStep, SetupSteps},
    StepFailure, StepResponse,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SetupStatus {
    #[default]
    Unchecked,
    Incomplete,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupAssessment {
    pub steps: [(SetupStep, StepResponse); 3],
}
impl SetupAssessment {
    pub fn status(&self) -> SetupStatus {
        if self.steps.iter().all(|(_, response)| satisfied(response)) {
            SetupStatus::Complete
        } else {
            SetupStatus::Incomplete
        }
    }
}

pub trait AssessmentBackend: Send + Sync {
    fn assess(&self) -> Result<SetupAssessment, StepFailure>;
}
pub struct EnvironmentAssessmentBackend;
impl AssessmentBackend for EnvironmentAssessmentBackend {
    fn assess(&self) -> Result<SetupAssessment, StepFailure> {
        // Resolve the current context on each check. Assessment never opens a
        // flow, takes its execution lock, or invokes a setup executor.
        let context = SetupContext::from_env(AuthorizationMode::Noninteractive)?;
        Ok(assess_steps(&NativeSteps::new(context)))
    }
}
pub(super) fn assess_steps(steps: &dyn SetupSteps) -> SetupAssessment {
    SetupAssessment {
        steps: inspect_steps(steps),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssessmentOperation(u64);
impl AssessmentOperation {
    pub fn execute(
        self,
        backend: &(impl AssessmentBackend + ?Sized),
    ) -> Result<SetupAssessment, StepFailure> {
        backend.assess()
    }
}

pub fn worker_stopped() -> StepFailure {
    StepFailure {
        presentation: crate::presentation::brightness::UserFacingError::new(
            "Setup could not be checked",
            "Open Complete setup to check the remaining requirements.",
        ),
        diagnostic: "setup assessment worker stopped without a result".into(),
        retryable: true,
    }
}

/// Keep at most one assessment worker in flight. Changes invalidate its result;
/// after it settles, one fresh assessment replaces any coalesced requests.
#[derive(Default)]
pub(crate) struct SetupHealth {
    status: SetupStatus,
    next: u64,
    running: Option<AssessmentOperation>,
    stale: bool,
    paused: bool,
}
impl SetupHealth {
    pub fn status(&self) -> SetupStatus {
        self.status
    }
    pub fn observe_flow(&mut self, status: SetupStatus) {
        self.status = status;
    }
    pub fn request(&mut self) -> Option<AssessmentOperation> {
        if self.paused {
            return None;
        }
        if self.running.is_some() {
            self.stale = true;
            return None;
        }
        self.next += 1;
        let operation = AssessmentOperation(self.next);
        self.running = Some(operation);
        self.stale = false;
        Some(operation)
    }
    pub fn set_paused(&mut self, paused: bool) -> Option<AssessmentOperation> {
        let was_paused = self.paused;
        self.paused = paused;
        if paused {
            self.stale = true;
            None
        } else if was_paused {
            self.request()
        } else {
            None
        }
    }
    pub fn complete(
        &mut self,
        operation: AssessmentOperation,
        result: Result<SetupAssessment, StepFailure>,
    ) -> Option<Option<AssessmentOperation>> {
        if self.running != Some(operation) {
            return None;
        }
        self.running = None;
        if self.stale || self.paused {
            return Some(self.request());
        }
        self.status = result
            .map(|assessment| assessment.status())
            .unwrap_or(SetupStatus::Incomplete);
        Some(None)
    }
    pub fn shutdown(&mut self) {
        self.paused = true;
        self.running = None;
    }
}

#[cfg(test)]
mod tests;
