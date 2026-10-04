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
    fn authorization_session(&self) -> Option<Arc<super::authorization::AuthorizationSession>> {
        None
    }
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
    authorization_session: Option<Arc<super::authorization::AuthorizationSession>>,
    lease: Option<FlowLock>,
    attempt: Option<StepCancellation>,
    cancelled: bool,
    closed: bool,
}

impl Control {
    fn close(&mut self) {
        self.closed = true;
        if let Some(session) = self.authorization_session.take() {
            session.close();
        }
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
        let observed = inspect_steps(backend.as_ref());
        let authorization_session = backend.authorization_session();
        let mut flow = Self {
            backend,
            control: Arc::new(Mutex::new(Control {
                authorization_session,
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

    /// Starting setup approves routine work for the flow, not additional input
    /// or build dependencies. Failures and blocked work always return to the caller.
    pub fn run(
        &mut self,
        token: FlowToken,
        progress: &mut dyn FnMut(FlowProgress),
    ) -> FlowSnapshot {
        self.try_run(token, &mut |event| {
            progress(event);
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap()
    }

    /// Progress failure cancels safe work if possible, or waits for the current
    /// mutation to finish. It never starts another step after that failure.
    pub fn try_run<E>(
        &mut self,
        token: FlowToken,
        progress: &mut dyn FnMut(FlowProgress) -> Result<(), E>,
    ) -> Result<FlowSnapshot, E> {
        if token != self.snapshot.token || self.control.lock().unwrap().closed {
            return Ok(self.snapshot());
        }
        if !matches!(
            self.snapshot().current(),
            Some((_, StepResponse::ActionRequired { .. }))
        ) {
            return Ok(self.snapshot());
        }
        let fresh = self.inspect();
        if fresh != self.observed {
            self.observed = fresh;
            self.publish(None);
        }
        let mut attempted = Vec::new();
        loop {
            // Execution already published a fresh aggregate verification. Do
            // not re-probe past a failed or blocked requirement without Retry.
            let snapshot = self.snapshot();
            let Some((step, StepResponse::ActionRequired { .. })) = snapshot.current() else {
                return Ok(snapshot);
            };
            let step = *step;
            // A successful command without verified readiness must not cause
            // an automatic retry (including after later inspections regress).
            if attempted.contains(&step) {
                self.publish(Some((
                    step,
                    StepResponse::Failed(StepFailure {
                        presentation: crate::presentation::brightness::UserFacingError::new(
                            "Setup could not be verified",
                            "The requirement is still incomplete. Retry to check and repair it.",
                        ),
                        diagnostic: format!("{step:?} remains incomplete after setup execution"),
                        recovery: super::recovery::SetupRecovery::new(
                            super::recovery::RecoveryCause::Unverified,
                            super::recovery::RepairBoundary::LocalSetup,
                            super::recovery::RecoveryAction::Retry,
                        ),
                        retryable: true,
                    }),
                )));
                return Ok(self.snapshot());
            }
            attempted.push(step);
            let cancellation = self.cancellation();
            let mut error = None;
            let result =
                self.execute_current(step, snapshot.token, StepAnswer::Continue, &mut |event| {
                    report_progress(progress, &cancellation, &mut error, event);
                });
            if let Some(error) = error {
                return Err(error);
            }
            if !satisfied(&result.steps[step as usize].1)
                && !matches!(
                    result.steps[step as usize].1,
                    StepResponse::ActionRequired { .. }
                )
            {
                return Ok(result);
            }
        }
    }

    /// Apply explicit input, then continue verified routine work without
    /// asking the frontend to select or approve each remaining step.
    pub fn advance_until_pause(
        &mut self,
        token: FlowToken,
        answer: StepAnswer,
        progress: &mut dyn FnMut(FlowProgress),
    ) -> FlowSnapshot {
        self.try_advance_until_pause(token, answer, &mut |event| {
            progress(event);
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap()
    }

    /// Submit input with the same progress-error boundary as routine work.
    pub fn try_advance_until_pause<E>(
        &mut self,
        token: FlowToken,
        answer: StepAnswer,
        progress: &mut dyn FnMut(FlowProgress) -> Result<(), E>,
    ) -> Result<FlowSnapshot, E> {
        if token != self.snapshot.token {
            return Ok(self.snapshot());
        }
        let step = self.snapshot().current().map(|(step, _)| *step);
        let cancellation = self.cancellation();
        let mut error = None;
        let snapshot = self.advance(token, answer, &mut |event| {
            report_progress(progress, &cancellation, &mut error, event);
        });
        if let Some(error) = error {
            return Err(error);
        }
        if step.is_some_and(|step| satisfied(&snapshot.steps[step as usize].1)) {
            self.try_run(snapshot.token, progress)
        } else {
            Ok(snapshot)
        }
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
        self.execute_current(step, token, answer, progress)
    }

    fn execute_current(
        &mut self,
        step: SetupStep,
        token: FlowToken,
        answer: StepAnswer,
        progress: &mut dyn FnMut(FlowProgress),
    ) -> FlowSnapshot {
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
        inspect_steps(self.backend.as_ref())
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

fn report_progress<E>(
    progress: &mut dyn FnMut(FlowProgress) -> Result<(), E>,
    cancellation: &FlowCancellation,
    error: &mut Option<E>,
    event: FlowProgress,
) {
    if error.is_none() {
        if let Err(failure) = progress(event) {
            cancellation.cancel();
            *error = Some(failure);
        }
    }
}

pub(super) fn inspect_steps(backend: &dyn SetupSteps) -> [(SetupStep, StepResponse); 3] {
    SetupStep::ORDER.map(|step| (step, backend.inspect(step)))
}

pub(super) fn satisfied(response: &StepResponse) -> bool {
    matches!(
        response,
        StepResponse::Complete | StepResponse::NotApplicable
    )
}

#[cfg(test)]
mod tests;
