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
        Self {
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
        }
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
