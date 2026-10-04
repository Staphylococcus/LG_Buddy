//! The session daemon's verified setup state. Probes never hold the snapshot lock.
use super::{
    assessment::{AssessmentBackend, SetupAssessment, SetupStatus},
    flow::SetupStep,
    StepFailure, StepInput, StepResponse,
};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
};

/// Frontends consume published state, never the daemon's probe backend.
pub trait SnapshotBackend: Send + Sync {
    fn snapshot(&self) -> Result<SetupSnapshot, StepFailure>;
    fn request_reassessment(&self) -> Result<(String, u64), StepFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupRequirement {
    pub step: String,
    pub reason: String,
    pub actionable: bool,
    /// Independent of whether this frontend can perform the remedy.
    #[serde(default)]
    pub needs_attention: bool,
    /// Older peers omit these facts; unknown facts never authorize repair.
    #[serde(default)]
    pub recovery: super::recovery::SetupRecovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSnapshot {
    pub instance: String,
    pub revision: u64,
    pub config: PathBuf,
    pub status: SetupStatus,
    pub requirements: Vec<SetupRequirement>,
}

impl SetupSnapshot {
    pub fn from_assessment(result: Result<SetupAssessment, StepFailure>) -> Self {
        let mut snapshot = Self {
            instance: String::new(),
            revision: 0,
            config: PathBuf::new(),
            status: SetupStatus::Unchecked,
            requirements: Vec::new(),
        };
        match result {
            Ok(assessment) => {
                snapshot.status = assessment.status();
                for (step, response) in assessment.steps {
                    let Some(recovery) = response.recovery() else {
                        continue;
                    };
                    let (reason, actionable) = match response {
                        StepResponse::Complete | StepResponse::NotApplicable => continue,
                        StepResponse::InputRequired(StepInput::Pairing { .. }) => {
                            ("Complete TV details and pairing.".into(), true)
                        }
                        StepResponse::InputRequired(StepInput::BuildDependencies {
                            explanation,
                        })
                        | StepResponse::ActionRequired { explanation, .. } => {
                            (explanation.into(), true)
                        }
                        StepResponse::Failed(error) | StepResponse::Blocked(error) => (
                            format!(
                                "{} {}",
                                error.presentation.summary(),
                                error.presentation.detail()
                            ),
                            false,
                        ),
                        _ => ("Setup has not been verified.".into(), false),
                    };
                    snapshot.requirements.push(SetupRequirement {
                        step: match step {
                            SetupStep::Pairing => "pairing",
                            SetupStep::Services => "services",
                            SetupStep::Plasma => "plasma",
                        }
                        .into(),
                        reason,
                        actionable,
                        needs_attention: recovery.needs_attention(),
                        recovery,
                    });
                }
            }
            Err(error) => {
                snapshot.status = SetupStatus::Incomplete;
                snapshot.requirements.push(SetupRequirement {
                    step: "assessment".into(),
                    reason: format!(
                        "{} {}",
                        error.presentation.summary(),
                        error.presentation.detail()
                    ),
                    actionable: false,
                    needs_attention: error.recovery.needs_attention(),
                    recovery: error.recovery,
                });
            }
        }
        snapshot
    }
}

#[derive(Clone)]
pub(crate) struct PublishedSetup {
    state: Arc<Mutex<(u64, SetupSnapshot)>>,
    requests: mpsc::Sender<WorkerMessage>,
}

enum WorkerMessage {
    Assess,
    Stop,
}

impl PublishedSetup {
    pub(crate) fn snapshot(&self) -> SetupSnapshot {
        self.state.lock().expect("setup snapshot").1.clone()
    }

    pub(crate) fn request(&self) -> Result<u64, String> {
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        state.0 += 1;
        self.requests
            .send(WorkerMessage::Assess)
            .map_err(|e| e.to_string())?;
        Ok(state.0)
    }
}

pub(crate) struct AssessmentWorker {
    pub(crate) published: PublishedSetup,
    worker: Option<JoinHandle<()>>,
}

impl AssessmentWorker {
    pub(crate) fn spawn(backend: impl AssessmentBackend + 'static, config: PathBuf) -> Self {
        let initial = SetupSnapshot {
            instance: format!("{}-{:?}", std::process::id(), std::time::SystemTime::now()),
            config: config.clone(),
            status: SetupStatus::Unchecked,
            revision: 0,
            requirements: Vec::new(),
        };
        let state = Arc::new(Mutex::new((0, initial)));
        let (requests, incoming) = mpsc::channel();
        let published = PublishedSetup {
            state: state.clone(),
            requests,
        };
        let worker = thread::spawn(move || {
            while let Ok(WorkerMessage::Assess) = incoming.recv() {
                while let Ok(message) = incoming.try_recv() {
                    if matches!(message, WorkerMessage::Stop) {
                        return;
                    }
                }
                let revision = state.lock().expect("setup snapshot").0;
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend.assess()))
                        .unwrap_or_else(|_| Err(super::assessment::worker_stopped()));
                let mut snapshot = SetupSnapshot::from_assessment(result);
                snapshot.revision = revision;
                snapshot.config = config.clone();
                let mut state = state.lock().expect("setup snapshot");
                if state.0 == revision {
                    snapshot.instance.clone_from(&state.1.instance);
                    state.1 = snapshot;
                }
            }
        });
        published.request().expect("new assessment worker");
        Self {
            published,
            worker: Some(worker),
        }
    }
}

impl Drop for AssessmentWorker {
    fn drop(&mut self) {
        let _ = self.published.requests.send(WorkerMessage::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn unavailable(error: impl ToString) -> StepFailure {
    StepFailure {
        presentation: crate::presentation::brightness::UserFacingError::new(
            "Setup state unavailable",
            "Start or restart LG Buddy's session service, then retry setup.",
        ),
        diagnostic: error.to_string(),
        recovery: super::recovery::SetupRecovery::new(
            super::recovery::RecoveryCause::VerifierUnavailable,
            super::recovery::RepairBoundary::SessionService,
            super::recovery::RecoveryAction::RestartSession,
        ),
        retryable: true,
    }
}

#[cfg(test)]
mod tests;
