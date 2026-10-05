//! Isolated flow fixture shared by backend and GTK tests; excluded from releases.
use super::*;
use crate::setup::{flow::SetupSteps, lock::FlowLock, StepCancellation};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

pub struct Fixture {
    root: PathBuf,
    pub responses: Arc<Mutex<[StepResponse; 3]>>,
    pub calls: Arc<Mutex<Vec<SetupStep>>>,
    published: Mutex<super::super::published::SetupSnapshot>,
}
impl Fixture {
    pub fn new(paired: bool, plasma: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-gui-flow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let fixture = Self {
            root,
            responses: Arc::new(Mutex::new([
                if paired {
                    StepResponse::Complete
                } else {
                    StepResponse::InputRequired(StepInput::Pairing { saved: None })
                },
                StepResponse::ActionRequired {
                    explanation: "Install background services.",
                    requires_authorization: true,
                },
                if plasma {
                    StepResponse::ActionRequired {
                        explanation: "Install Plasma integration.",
                        requires_authorization: true,
                    }
                } else {
                    StepResponse::NotApplicable
                },
            ])),
            calls: Arc::new(Mutex::new(Vec::new())),
            published: Mutex::new(super::super::published::SetupSnapshot::from_assessment(
                Err(stopped()),
            )),
        };
        fixture.publish();
        fixture
    }
    pub fn publish(&self) {
        let result = super::super::assessment::AssessmentBackend::assess(self);
        let mut state = self.published.lock().unwrap();
        let mut snapshot = super::super::published::SetupSnapshot::from_assessment(result);
        snapshot.instance = self.root.to_string_lossy().into_owned();
        snapshot.revision = state.revision + 1;
        *state = snapshot;
    }
    pub fn managed() -> Self {
        use crate::setup::recovery::{
            RecoveryAction, RecoveryCause, RepairBoundary, SetupRecovery,
        };
        let fixture = Self::new(true, false);
        fixture.responses.lock().unwrap()[1] = StepResponse::Blocked(StepFailure {
            presentation: UserFacingError::new("Service setup incomplete", "Bind LG_Buddy_screen.service through NixOS configuration, build and activate it, then recheck."),
            diagnostic: "fixture managed binding".into(),
            recovery: SetupRecovery::new(RecoveryCause::ManagedInstallation, RepairBoundary::SystemConfiguration, RecoveryAction::RepairExternally),
            retryable: true,
        });
        fixture.publish();
        fixture
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
struct Steps {
    responses: Arc<Mutex<[StepResponse; 3]>>,
    calls: Arc<Mutex<Vec<SetupStep>>>,
}
impl SetupSteps for Steps {
    fn inspect(&self, step: SetupStep) -> StepResponse {
        self.responses.lock().unwrap()[step as usize].clone()
    }
    fn execute(
        &self,
        step: SetupStep,
        answer: StepAnswer,
        cancellation: &StepCancellation,
        _: &FlowLock,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse {
        self.calls.lock().unwrap().push(step);
        if !cancellation.begin() {
            return StepResponse::Cancelled;
        }
        progress(StepResponse::Running {
            message: "Applying setup…",
            cancelable: false,
        });
        let result = if step == SetupStep::Plasma && answer == StepAnswer::Continue {
            StepResponse::InputRequired(StepInput::BuildDependencies {
                explanation: "Install compiler packages?",
            })
        } else {
            self.responses.lock().unwrap()[step as usize] = StepResponse::Complete;
            StepResponse::Complete
        };
        cancellation.finish();
        result
    }
}
impl OnboardingBackend for Fixture {
    fn open(&self) -> Result<OnboardingFlow, StepFailure> {
        OnboardingFlow::with_backend(
            Box::new(Steps {
                responses: self.responses.clone(),
                calls: self.calls.clone(),
            }),
            &self.root.join("lock"),
        )
    }
}
impl super::super::published::SnapshotBackend for Fixture {
    fn snapshot(&self) -> Result<super::super::published::SetupSnapshot, StepFailure> {
        Ok(self.published.lock().unwrap().clone())
    }
    fn request_reassessment(&self) -> Result<(String, u64), StepFailure> {
        self.publish();
        let state = self.published.lock().unwrap();
        Ok((state.instance.clone(), state.revision))
    }
}
impl super::super::assessment::AssessmentBackend for Fixture {
    fn assess(&self) -> Result<super::super::assessment::SetupAssessment, StepFailure> {
        Ok(super::super::assessment::assess_steps(&Steps {
            responses: self.responses.clone(),
            calls: self.calls.clone(),
        }))
    }
}
