//! Synchronous worker API shared by CLI and GUI. Frontends render snapshots,
//! submit answers, and may cancel through a separate thread-safe handle. They
//! never submit step completions or decide which step executes next.

use super::{lock::FlowLock, StepCancellation, StepFailure, StepResponse};
use crate::pairing::PairingRequest;
use std::path::Path;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStep {
    Pairing,
    Services,
    Plasma,
}

impl SetupStep {
    pub const ORDER: [Self; 3] = [Self::Pairing, Self::Services, Self::Plasma];
}

/// Answers are interpreted only by the receiving step's adapter. Continue is
/// explicit consent to the action described by the current snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepAnswer {
    Continue,
    Pairing(PairingRequest),
    InstallBuildDependencies,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationMode {
    Interactive,
    Terminal,
    Noninteractive,
}

/// An opaque identity prevents delayed frontend actions from applying to a
/// newer request, or to a different flow that happens to be at the same step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowToken {
    flow: u64,
    revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowOutcome {
    Incomplete,
    Complete,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowSnapshot {
    pub token: FlowToken,
    /// Current observations and outstanding requests, in fixed execution order.
    pub steps: [(SetupStep, StepResponse); 3],
    pub outcome: FlowOutcome,
}

impl FlowSnapshot {
    pub fn current(&self) -> Option<&(SetupStep, StepResponse)> {
        if self.outcome != FlowOutcome::Incomplete {
            return None;
        }
        self.steps.iter().find(|(_, response)| !satisfied(response))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowProgress {
    pub token: FlowToken,
    pub step: SetupStep,
    pub response: StepResponse,
}

/// Deliberately internal: non-pairing setup has no independent public executor.
pub(super) trait SetupSteps: Send {
    fn inspect(&self, step: SetupStep) -> StepResponse;
    fn execute(
        &self,
        step: SetupStep,
        answer: StepAnswer,
        cancellation: &StepCancellation,
        lease: &FlowLock,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse;
}

struct Control {
    lease: Option<FlowLock>,
    attempt: Option<StepCancellation>,
    cancelled: bool,
    closed: bool,
}

impl Control {
    fn close(&mut self) {
        self.closed = true;
        self.lease.take();
    }
}

/// Cancellation is decided against the step's live gate, not cached UI state.
#[derive(Clone)]
pub struct FlowCancellation(Arc<Mutex<Control>>);

impl FlowCancellation {
    pub fn can_cancel(&self) -> bool {
        let control = self.0.lock().unwrap();
        !control.closed
            && !control.cancelled
            && control
                .attempt
                .as_ref()
                .is_none_or(StepCancellation::can_cancel)
    }

    /// Rejection does not queue a later cancellation or release flow ownership.
    /// Accepted cancellation of running work retains ownership until it returns.
    pub fn cancel(&self) -> bool {
        let mut control = self.0.lock().unwrap();
        if control.closed || control.cancelled {
            return false;
        }
        if let Some(attempt) = &control.attempt {
            if !attempt.cancel() {
                return false;
            }
        } else {
            control.close();
        }
        control.cancelled = true;
        true
    }
}

pub struct OnboardingFlow {
    backend: Box<dyn SetupSteps>,
    control: Arc<Mutex<Control>>,
    snapshot: FlowSnapshot,
    // Keep read-only observations separate from a step's outstanding request
    // (e.g. additional input discovered during execution). Reinspection must
    // not discard that request while the observed requirements are unchanged.
    observed: [(SetupStep, StepResponse); 3],
}

impl OnboardingFlow {
    /// Resolve one configuration and installation context for the current user.
    /// Call from a worker: inspections may use D-Bus and subprocesses.
    pub fn open(mode: AuthorizationMode) -> Result<Self, StepFailure> {
        let context = super::environment::SetupContext::from_env(mode)?;
        let lock_path = context.lock_path.clone();
        Self::with_backend(
            Box::new(super::environment::NativeSteps::new(context)),
            &lock_path,
        )
    }

    pub(super) fn with_backend(
        backend: Box<dyn SetupSteps>,
        lock_path: &Path,
    ) -> Result<Self, StepFailure> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let lease = FlowLock::acquire(lock_path)?;
        let observed = SetupStep::ORDER.map(|step| (step, backend.inspect(step)));
        let mut flow = Self {
            backend,
            control: Arc::new(Mutex::new(Control {
                lease: Some(lease),
                attempt: None,
                cancelled: false,
                closed: false,
            })),
            snapshot: FlowSnapshot {
                token: FlowToken {
                    flow: NEXT.fetch_add(1, Ordering::Relaxed),
                    revision: 0,
                },
                steps: observed.clone(),
                outcome: FlowOutcome::Incomplete,
            },
            observed,
        };
        flow.publish(None);
        Ok(flow)
    }

    pub fn cancellation(&self) -> FlowCancellation {
        FlowCancellation(self.control.clone())
    }

    pub fn snapshot(&self) -> FlowSnapshot {
        let mut snapshot = self.snapshot.clone();
        if self.control.lock().unwrap().cancelled {
            snapshot.outcome = FlowOutcome::Cancelled;
        }
        snapshot
    }

    /// Reinspect an open flow without changing the system. Terminal flows stay
    /// closed; re-entry opens a new flow and inspects current facts again.
    pub fn refresh(&mut self) -> FlowSnapshot {
        if !self.control.lock().unwrap().closed {
            self.observed = self.inspect();
            self.publish(None);
        }
        self.snapshot()
    }

    /// Execute at most the current step. A new request always returns to the
    /// frontend; neither failures nor authorization requests auto-retry.
    pub fn advance(
        &mut self,
        token: FlowToken,
        answer: StepAnswer,
        progress: &mut dyn FnMut(FlowProgress),
    ) -> FlowSnapshot {
        if token != self.snapshot.token || self.control.lock().unwrap().closed {
            return self.snapshot();
        }
        let fresh = self.inspect();
        if fresh != self.observed {
            self.observed = fresh;
            self.publish(None);
            return self.snapshot();
        }
        let Some((step, response)) = self.snapshot.current() else {
            return self.snapshot();
        };
        let step = *step;
        let permitted = match response {
            StepResponse::ActionRequired { .. } | StepResponse::InputRequired(_) => true,
            StepResponse::Failed(failure) | StepResponse::Blocked(failure) => failure.retryable,
            _ => false,
        };
        if !permitted {
            return self.snapshot();
        }
        let cancellation = StepCancellation::default();
        let lease = {
            let mut control = self.control.lock().unwrap();
            if control.closed {
                drop(control);
                return self.snapshot();
            }
            control.attempt = Some(cancellation.clone());
            control.lease.as_ref().unwrap().clone()
        };
        let response = self
            .backend
            .execute(step, answer, &cancellation, &lease, &mut |response| {
                progress(FlowProgress {
                    token,
                    step,
                    response,
                });
            });
        {
            let mut control = self.control.lock().unwrap();
            control.attempt = None;
            control.cancelled |= response == StepResponse::Cancelled;
            if control.cancelled {
                control.close();
            }
        }
        self.observed = self.inspect();
        // Successful execution still needs a fresh aggregate inspection. Other
        // outcomes stay visible rather than being converted into completion.
        self.publish((!satisfied(&response)).then_some((step, response)));
        self.snapshot()
    }

    fn inspect(&self) -> [(SetupStep, StepResponse); 3] {
        SetupStep::ORDER.map(|step| (step, self.backend.inspect(step)))
    }

    fn publish(&mut self, result: Option<(SetupStep, StepResponse)>) {
        self.snapshot.steps = self.observed.clone();
        if let Some((step, response)) = result {
            self.snapshot
                .steps
                .iter_mut()
                .find(|(id, _)| *id == step)
                .unwrap()
                .1 = response;
        }
        self.snapshot.token.revision += 1;
        let mut control = self.control.lock().unwrap();
        self.snapshot.outcome = if control.cancelled {
            FlowOutcome::Cancelled
        } else if self
            .snapshot
            .steps
            .iter()
            .all(|(_, response)| satisfied(response))
        {
            control.close();
            FlowOutcome::Complete
        } else {
            FlowOutcome::Incomplete
        };
    }
}

impl Drop for OnboardingFlow {
    fn drop(&mut self) {
        // A cancellation handle may outlive its flow, but must not keep a lock.
        self.control.lock().unwrap().close();
    }
}

fn satisfied(response: &StepResponse) -> bool {
    matches!(
        response,
        StepResponse::Complete | StepResponse::NotApplicable
    )
}

#[cfg(test)]
mod tests;
