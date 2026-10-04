//! Read-only setup health, composed from the same inspections as onboarding.
use super::{
    environment::{NativeSteps, SetupContext},
    flow::{inspect_steps, satisfied, AuthorizationMode, SetupStep, SetupSteps},
    StepFailure, StepResponse,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
pub struct AssessmentOperation(u64, bool, bool);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssessmentRead {
    pub snapshot: super::published::SetupSnapshot,
    pub requested: Option<(String, u64)>,
}
impl AssessmentOperation {
    pub fn execute(
        self,
        backend: &(impl super::published::SnapshotBackend + ?Sized),
    ) -> Result<AssessmentRead, StepFailure> {
        if self.2 {
            backend.restart_verifier()?;
        }
        let requested = if self.1 {
            Some(backend.request_reassessment()?)
        } else {
            None
        };
        Ok(AssessmentRead {
            snapshot: backend.snapshot()?,
            requested,
        })
    }
}

pub fn worker_stopped() -> StepFailure {
    StepFailure {
        presentation: crate::presentation::brightness::UserFacingError::new(
            "Setup could not be checked",
            "Open Complete setup to check the remaining requirements.",
        ),
        diagnostic: "setup assessment worker stopped without a result".into(),
        recovery: super::recovery::SetupRecovery::new(
            super::recovery::RecoveryCause::TemporaryFailure,
            super::recovery::RepairBoundary::SessionService,
            super::recovery::RecoveryAction::Retry,
        ),
        retryable: true,
    }
}

/// Keep one cached read in flight. Setup changes reject older reads and request
/// daemon verification after mutation settles, without replacing stored status.
#[derive(Default)]
pub(crate) struct SetupHealth {
    status: SetupStatus,
    next: u64,
    running: Option<AssessmentOperation>,
    stale: bool,
    paused: bool,
    verification_needed: bool,
    required: Option<(String, u64)>,
    verification_started: Option<std::time::Instant>,
    restart_requested: bool,
}
pub(crate) const VERIFICATION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
impl SetupHealth {
    pub fn accepts(&self, operation: AssessmentOperation) -> bool {
        self.running == Some(operation) && !self.stale && !self.paused
    }
    pub fn status(&self) -> SetupStatus {
        self.status
    }
    pub fn changed(&mut self) {
        self.verification_needed = true;
        self.required = None;
        self.stale = true;
        self.verification_started = None;
    }
    pub fn retry_verification(&mut self, restart: bool) {
        self.changed();
        self.restart_requested = restart;
    }
    pub fn verification_expired(&self) -> bool {
        self.verification_expired_at(std::time::Instant::now())
    }
    fn verification_expired_at(&self, now: std::time::Instant) -> bool {
        !self.paused
            && self.verification_pending()
            && self
                .verification_started
                .is_some_and(|started| now.saturating_duration_since(started) >= VERIFICATION_WAIT)
    }
    pub fn verification_pending(&self) -> bool {
        self.verification_needed || self.required.is_some()
    }
    pub fn request(&mut self) -> Option<AssessmentOperation> {
        if self.paused {
            return None;
        }
        if self.running.is_some() {
            return None;
        }
        self.next += 1;
        if self.verification_needed && self.verification_started.is_none() {
            self.verification_started = Some(std::time::Instant::now());
        }
        let operation = AssessmentOperation(
            self.next,
            self.verification_needed,
            std::mem::take(&mut self.restart_requested),
        );
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
        result: Result<AssessmentRead, StepFailure>,
    ) -> Option<Option<AssessmentOperation>> {
        if self.running != Some(operation) {
            return None;
        }
        self.running = None;
        if self.stale || self.paused {
            return Some(self.request());
        }
        // Availability is presented by the caller; it does not replace the
        // last published status or clear pending verification.
        if let Ok(read) = result {
            if let Some(required) = read.requested {
                self.required = Some(required);
                self.verification_needed = false;
            }
            let verified = !self.verification_needed
                && self.required.as_ref().is_none_or(|(instance, revision)| {
                    read.snapshot.instance != *instance || read.snapshot.revision >= *revision
                });
            // Unchecked is a daemon's startup placeholder, not a replacement
            // assessment or confirmation of a settled setup mutation.
            if verified && read.snapshot.status != SetupStatus::Unchecked {
                self.status = read.snapshot.status;
                self.required = None;
                self.verification_started = None;
            }
        }
        Some(None)
    }
    pub fn shutdown(&mut self) {
        self.paused = true;
        self.running = None;
    }
}

#[cfg(test)]
mod tests;
