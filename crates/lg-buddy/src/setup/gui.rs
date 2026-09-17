//! Toolkit-independent modal state. Workers run the same flow as the CLI;
//! the application only edits requested input and presents its responses.
use super::{
    flow::{
        AuthorizationMode, FlowCancellation, FlowOutcome, FlowProgress, FlowSnapshot, FlowToken,
        OnboardingFlow, SetupStep, StepAnswer,
    },
    StepFailure, StepInput, StepResponse,
};
use crate::{
    config::HdmiInput,
    pairing::{PairingDraft, PairingRequest, PairingStage},
    presentation::{brightness::UserFacingError, pairing::PairingPresentation},
};
use std::sync::{Arc, Mutex};

pub use super::assessment::SetupStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingIntent {
    Open,
    SetAddress(String),
    SetMac(String),
    SetInput(HdmiInput),
    Submit,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingPresentation {
    pub title: String,
    pub description: String,
    pub pairing: Option<PairingPresentation>,
    pub action: Option<&'static str>,
    pub can_cancel: bool,
    pub busy: bool,
    pub error: Option<UserFacingError>,
}

impl OnboardingPresentation {
    /// Render a backend response without deciding which step comes next.
    pub fn for_step(step: SetupStep, response: &StepResponse) -> Self {
        let mut view = Self {
            title: match step {
                SetupStep::Pairing => "Pair a TV",
                SetupStep::Services => "Background services",
                SetupStep::Plasma => "Plasma integration",
            }
            .into(),
            description: String::new(),
            pairing: None,
            action: None,
            can_cancel: true,
            busy: false,
            error: None,
        };
        match response {
            StepResponse::InputRequired(StepInput::Pairing { saved }) => {
                let draft = saved
                    .map(|r| PairingDraft {
                        address: r.address().to_string(),
                        mac: r.mac().to_string(),
                        input: r.input(),
                    })
                    .unwrap_or_default();
                let pairing = PairingPresentation::new(draft, PairingStage::Editing, None);
                view.description = pairing.description().into();
                view.pairing = Some(pairing);
                view.action = Some("Pair");
            }
            StepResponse::ActionRequired {
                explanation,
                requires_authorization,
            } => {
                view.description = (*explanation).into();
                if *requires_authorization {
                    view.description.push_str("\n\nA system authorization dialog may ask for your password to make these changes.");
                }
                view.action = Some("Continue");
            }
            StepResponse::InputRequired(StepInput::BuildDependencies { explanation }) => {
                view.description = (*explanation).into();
                view.action = Some("Install build tools");
            }
            StepResponse::Running {
                message,
                cancelable,
            } => {
                view.description = (*message).into();
                view.busy = true;
                view.can_cancel = *cancelable;
            }
            StepResponse::Failed(error) | StepResponse::Blocked(error) => {
                view.error = Some(error.presentation.clone());
                view.action = error.retryable.then_some("Retry");
            }
            _ => {}
        }
        view
    }
    fn opening() -> Self {
        Self {
            title: "Complete setup".into(),
            description: "Checking what needs to be set up…".into(),
            pairing: None,
            action: None,
            can_cancel: true,
            busy: true,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Open,
    Advance(FlowToken, StepAnswer),
    Refresh,
}

#[derive(Clone)]
pub struct OnboardingOperation {
    id: u64,
    flow: Arc<Mutex<Option<OnboardingFlow>>>,
    command: Command,
}
impl std::fmt::Debug for OnboardingOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnboardingOperation")
            .field("id", &self.id)
            .field("command", &self.command)
            .finish()
    }
}
impl PartialEq for OnboardingOperation {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && Arc::ptr_eq(&self.flow, &other.flow)
    }
}
impl Eq for OnboardingOperation {}

pub struct OnboardingResult {
    snapshot: FlowSnapshot,
    cancellation: FlowCancellation,
}

/// Environment selection is injected for isolated frontend integration tests.
pub trait OnboardingBackend: super::assessment::AssessmentBackend {
    fn open(&self) -> Result<OnboardingFlow, StepFailure>;
}
pub struct EnvironmentOnboardingBackend;
impl super::assessment::AssessmentBackend for EnvironmentOnboardingBackend {
    fn assess(&self) -> Result<super::assessment::SetupAssessment, StepFailure> {
        super::assessment::EnvironmentAssessmentBackend.assess()
    }
}
impl OnboardingBackend for EnvironmentOnboardingBackend {
    fn open(&self) -> Result<OnboardingFlow, StepFailure> {
        OnboardingFlow::open(AuthorizationMode::Interactive)
    }
}

impl OnboardingOperation {
    pub fn execute(
        &self,
        progress: &mut dyn FnMut(FlowProgress),
    ) -> Result<OnboardingResult, StepFailure> {
        self.execute_with(&EnvironmentOnboardingBackend, progress)
    }
    pub fn execute_with(
        &self,
        backend: &dyn OnboardingBackend,
        progress: &mut dyn FnMut(FlowProgress),
    ) -> Result<OnboardingResult, StepFailure> {
        let mut slot = self.flow.lock().map_err(|_| stopped())?;
        if self.command == Command::Open {
            *slot = Some(backend.open()?);
        }
        let flow = slot.as_mut().ok_or_else(stopped)?;
        let snapshot = match &self.command {
            Command::Open => flow.snapshot(),
            Command::Refresh => flow.refresh(),
            Command::Advance(token, answer) => flow.advance(*token, answer.clone(), progress),
        };
        Ok(OnboardingResult {
            snapshot,
            cancellation: flow.cancellation(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingTransition {
    pub diagnostic: Option<String>,
    pub presentation: Option<OnboardingPresentation>,
    pub operation: Option<OnboardingOperation>,
}

#[derive(Default)]
pub struct OnboardingApplication {
    flow: Option<Arc<Mutex<Option<OnboardingFlow>>>>,
    cancellation: Option<FlowCancellation>,
    active: Option<OnboardingOperation>,
    snapshot: Option<FlowSnapshot>,
    presentation: Option<OnboardingPresentation>,
    draft: PairingDraft,
    draft_loaded: bool,
    next: u64,
    cancelling: bool,
    status: SetupStatus,
    diagnostic: Option<String>,
}

impl OnboardingApplication {
    pub fn is_open(&self) -> bool {
        self.presentation.is_some()
    }
    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn status(&self) -> SetupStatus {
        self.status
    }
    pub fn handle(&mut self, intent: OnboardingIntent) -> Option<OnboardingTransition> {
        if intent == OnboardingIntent::Open {
            if self.is_open() {
                return None;
            }
            self.flow = Some(Arc::new(Mutex::new(None)));
            self.draft = PairingDraft::default();
            self.draft_loaded = false;
            self.snapshot = None;
            self.cancellation = None;
            self.cancelling = false;
            self.presentation = Some(OnboardingPresentation::opening());
            return Some(self.start(Command::Open));
        }
        if !self.is_open() {
            return None;
        }
        if intent == OnboardingIntent::Cancel {
            // Consult the live gate even when a queued progress event says this
            // operation is still cancelable. Rejection is never queued.
            if self.cancelling {
                return None;
            }
            let complete = self
                .snapshot
                .as_ref()
                .is_some_and(|s| s.outcome == FlowOutcome::Complete);
            if !complete && self.cancellation.as_ref().is_some_and(|c| !c.cancel()) {
                return None;
            }
            if self.active.is_some() && self.cancellation.is_some() {
                self.cancelling = true;
                let view = self.presentation.as_mut().unwrap();
                view.description = "Cancelling setup…".into();
                view.can_cancel = false;
                return Some(self.transition(None));
            }
            return Some(self.close());
        }
        if self.is_busy() {
            return None;
        }
        if intent == OnboardingIntent::Submit {
            self.presentation.as_ref()?.action?;
            let Some(snapshot) = self.snapshot.as_ref() else {
                return Some(self.start(Command::Open));
            };
            if snapshot.outcome == FlowOutcome::Complete {
                return Some(self.close());
            }
            let (_, response) = snapshot.current()?;
            let answer = match response {
                StepResponse::InputRequired(StepInput::Pairing { .. }) => {
                    match PairingRequest::parse(
                        &self.draft.address,
                        &self.draft.mac,
                        self.draft.input,
                    ) {
                        Ok(request) => StepAnswer::Pairing(request),
                        Err(error) => {
                            self.presentation.as_mut().unwrap().error = Some(error);
                            return Some(self.transition(None));
                        }
                    }
                }
                StepResponse::ActionRequired { .. } => StepAnswer::Continue,
                StepResponse::InputRequired(StepInput::BuildDependencies { .. }) => {
                    StepAnswer::InstallBuildDependencies
                }
                StepResponse::Failed(error) | StepResponse::Blocked(error) if error.retryable => {
                    return Some(self.start(Command::Refresh))
                }
                _ => return None,
            };
            return Some(self.start(Command::Advance(snapshot.token, answer)));
        }
        self.presentation.as_ref()?.pairing.as_ref()?;
        match intent {
            OnboardingIntent::SetAddress(value) => self.draft.address = value,
            OnboardingIntent::SetMac(value) => self.draft.mac = value,
            OnboardingIntent::SetInput(value) => self.draft.input = value,
            _ => return None,
        }
        self.present_snapshot();
        Some(self.transition(None))
    }
    pub fn progress(
        &mut self,
        operation: &OnboardingOperation,
        progress: FlowProgress,
    ) -> Option<OnboardingTransition> {
        if self.active.as_ref() != Some(operation)
            || self.cancelling
            || !self
                .snapshot
                .as_ref()
                .is_some_and(|s| s.token == progress.token)
        {
            return None;
        }
        self.presentation = Some(OnboardingPresentation::for_step(
            progress.step,
            &progress.response,
        ));
        Some(self.transition(None))
    }
    pub fn complete(
        &mut self,
        operation: &OnboardingOperation,
        result: Result<OnboardingResult, StepFailure>,
    ) -> Option<OnboardingTransition> {
        if self.active.as_ref() != Some(operation) {
            return None;
        }
        self.active = None;
        if self.cancelling {
            return Some(self.close());
        }
        match result {
            Ok(result) => {
                self.diagnostic =
                    result
                        .snapshot
                        .current()
                        .and_then(|(_, response)| match response {
                            StepResponse::Failed(error) | StepResponse::Blocked(error) => {
                                Some(error.diagnostic.clone())
                            }
                            _ => None,
                        });
                self.cancellation = Some(result.cancellation);
                self.status = if result.snapshot.outcome == FlowOutcome::Complete {
                    SetupStatus::Complete
                } else {
                    SetupStatus::Incomplete
                };
                if result.snapshot.outcome == FlowOutcome::Cancelled {
                    return Some(self.close());
                }
                self.snapshot = Some(result.snapshot);
                self.present_snapshot();
            }
            Err(error) => {
                self.diagnostic = Some(error.diagnostic.clone());
                self.status = SetupStatus::Incomplete;
                // A stopped worker may have poisoned its slot. Drop that
                // session; retry opens a new flow and inspects current facts.
                self.flow = Some(Arc::new(Mutex::new(None)));
                self.snapshot = None;
                self.cancellation = None;
                self.presentation = Some(OnboardingPresentation::for_step(
                    SetupStep::Services,
                    &StepResponse::Failed(error),
                ));
            }
        }
        Some(self.transition(None))
    }
    pub fn worker_stopped(
        &mut self,
        operation: &OnboardingOperation,
    ) -> Option<OnboardingTransition> {
        self.complete(operation, Err(stopped()))
    }
    fn start(&mut self, command: Command) -> OnboardingTransition {
        self.diagnostic = None;
        self.next += 1;
        let operation = OnboardingOperation {
            id: self.next,
            flow: self.flow.as_ref().unwrap().clone(),
            command,
        };
        self.active = Some(operation.clone());
        if let Some(view) = &mut self.presentation {
            view.action = None;
            view.busy = true;
            view.error = None;
        }
        self.transition(Some(operation))
    }
    fn close(&mut self) -> OnboardingTransition {
        self.diagnostic = None;
        self.presentation = None;
        self.active = None;
        self.flow = None;
        self.cancellation = None;
        self.snapshot = None;
        self.cancelling = false;
        self.transition(None)
    }
    fn transition(&self, operation: Option<OnboardingOperation>) -> OnboardingTransition {
        OnboardingTransition {
            diagnostic: self.diagnostic.clone(),
            presentation: self.presentation.clone(),
            operation,
        }
    }
    fn present_snapshot(&mut self) {
        let snapshot = self.snapshot.as_ref().unwrap();
        if snapshot.outcome == FlowOutcome::Complete {
            self.presentation = Some(OnboardingPresentation {
                title: "Setup complete".into(),
                description: "Your TV and the required background services are ready.".into(),
                pairing: None,
                action: Some("Done"),
                can_cancel: true,
                busy: false,
                error: None,
            });
        } else if let Some((step, response)) = snapshot.current() {
            let mut view = OnboardingPresentation::for_step(*step, response);
            if let Some(pairing) = &view.pairing {
                if !self.draft_loaded {
                    self.draft = PairingDraft {
                        address: pairing.address().into(),
                        mac: pairing.mac().into(),
                        input: pairing.input(),
                    };
                    self.draft_loaded = true;
                }
                view.pairing = Some(PairingPresentation::new(
                    self.draft.clone(),
                    PairingStage::Editing,
                    None,
                ));
            }
            self.presentation = Some(view);
        }
    }
}

fn stopped() -> StepFailure {
    StepFailure {
        presentation: UserFacingError::new(
            "Setup stopped unexpectedly",
            "Retry to check what remains to be set up.",
        ),
        diagnostic: "onboarding worker stopped".into(),
        retryable: true,
    }
}

#[cfg(any(test, feature = "gui-test-fixtures"))]
pub mod fixtures;
#[cfg(test)]
mod tests;
