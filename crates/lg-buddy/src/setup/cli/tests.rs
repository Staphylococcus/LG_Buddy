use super::*;
use crate::{parse_args, Command};
#[test]
fn parses_setup_and_scoped_help() {
    let ParseOutcome::Command(Command::Setup(options)) = parse_args([
        "setup",
        "--non-interactive",
        "--yes",
        "--tv-ip",
        "192.0.2.2",
        "--tv-mac",
        "02:11:22:33:44:55",
        "--input",
        "HDMI_2",
        "--allow-build-dependencies",
    ])
    .unwrap() else {
        panic!("setup command expected")
    };
    assert!(options.yes && options.noninteractive && options.allow_build_dependencies);
    assert_eq!(options.input, Some(HdmiInput::Hdmi2));
    for args in [["setup", "--help"], ["help", "setup"]] {
        assert_eq!(
            parse_args(args).unwrap(),
            ParseOutcome::Help(HelpTopic::Setup)
        );
    }
    for args in [
        vec!["setup", "--tv-ip"],
        vec!["setup", "--input", "5"],
        vec!["setup", "--tv-ip", "192.0.2.1", "--tv-ip", "192.0.2.2"],
        vec!["setup", "--bogus"],
    ] {
        assert_eq!(parse_args(args).unwrap_err().help_topic(), HelpTopic::Setup);
    }
}

use crate::setup::{
    flow::{SetupStep, SetupSteps},
    lock::FlowLock,
    StepCancellation,
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

#[derive(Default)]
struct State {
    paired: bool,
    services: bool,
    plasma: bool,
    needs_dependencies: bool,
    calls: Vec<SetupStep>,
    fail: bool,
}
struct Steps(Arc<Mutex<State>>);
impl SetupSteps for Steps {
    fn inspect(&self, step: SetupStep) -> StepResponse {
        let s = self.0.lock().unwrap();
        match step {
            SetupStep::Pairing if !s.paired => {
                StepResponse::InputRequired(StepInput::Pairing { saved: None })
            }
            SetupStep::Services if !s.services => StepResponse::ActionRequired {
                explanation: "Repair services.",
                requires_authorization: true,
            },
            SetupStep::Plasma if !s.plasma => StepResponse::ActionRequired {
                explanation: "Set up Plasma.",
                requires_authorization: true,
            },
            _ => StepResponse::Complete,
        }
    }
    fn execute(
        &self,
        step: SetupStep,
        answer: StepAnswer,
        _: &StepCancellation,
        _: &FlowLock,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse {
        let mut s = self.0.lock().unwrap();
        s.calls.push(step);
        if s.fail {
            return StepResponse::Failed(StepFailure {
                presentation: crate::presentation::brightness::UserFacingError::new(
                    "Setup failed",
                    "Try again.",
                ),
                diagnostic: "fixture failure".into(),
                retryable: true,
            });
        }
        progress(StepResponse::Running {
            message: "Applying setup…",
            cancelable: false,
        });
        match (step, answer) {
            (SetupStep::Pairing, StepAnswer::Pairing(_)) => s.paired = true,
            (SetupStep::Services, StepAnswer::Continue) => s.services = true,
            (SetupStep::Plasma, StepAnswer::Continue) if s.needs_dependencies => {
                return StepResponse::InputRequired(StepInput::BuildDependencies {
                    explanation: "Install development packages?",
                })
            }
            (SetupStep::Plasma, StepAnswer::Continue | StepAnswer::InstallBuildDependencies) => {
                s.plasma = true
            }
            _ => panic!("wrong input for step"),
        }
        StepResponse::Complete
    }
}
struct Fixture {
    root: PathBuf,
    state: Arc<Mutex<State>>,
}
impl Fixture {
    fn new(state: State) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            state: Arc::new(Mutex::new(state)),
        }
    }
    fn run(&self, args: &[&str], input: Option<&str>) -> (Result<(), SetupError>, String) {
        let ParseOutcome::Command(Command::Setup(options)) =
            parse_args(std::iter::once("setup").chain(args.iter().copied())).unwrap()
        else {
            panic!()
        };
        let mut flow = OnboardingFlow::with_backend(
            Box::new(Steps(self.state.clone())),
            &self.root.join("lock"),
        )
        .unwrap();
        let mut output = Vec::new();
        let result = render(
            &mut flow,
            &options,
            input.is_some(),
            &mut input.unwrap_or("").as_bytes(),
            &mut output,
        );
        (result, String::from_utf8(output).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn terminal_fresh_setup_and_repeat_use_the_same_flow() {
    let f = Fixture::new(State::default());
    let (result, output) = f.run(&[], Some("192.0.2.1\n02:11:22:33:44:55\nHDMI_2\ny\ny\ny\n"));
    result.unwrap();
    assert!(output.contains("Setup complete."));
    assert_eq!(f.state.lock().unwrap().calls, SetupStep::ORDER);
    f.state.lock().unwrap().calls.clear();
    let (result, output) = f.run(&[], None);
    result.unwrap();
    assert!(!output.contains("Administrator") && !output.contains("Pair a TV"));
    assert!(f.state.lock().unwrap().calls.is_empty());
}
#[test]
fn missing_input_cancellation_failure_and_partial_resume_remain_distinct() {
    let f = Fixture::new(State::default());
    let (result, _) = f.run(&["--non-interactive", "--yes"], None);
    assert_eq!(result.unwrap_err().exit_code(), 3);
    assert!(f.state.lock().unwrap().calls.is_empty());
    assert_eq!(f.run(&[], Some("q\n")).0.unwrap_err().exit_code(), 130);
    assert_eq!(f.run(&[], Some("")).0.unwrap_err().exit_code(), 130);
    let (result, output) = f.run(&[], Some("192.0.2.1\n02:11:22:33:44:55\n\ny\nn\n"));
    assert_eq!(result.unwrap_err().exit_code(), 130);
    assert!(!output.contains("Setup complete."));
    assert_eq!(f.state.lock().unwrap().calls, [SetupStep::Pairing]);
    f.state.lock().unwrap().fail = true;
    assert_eq!(f.run(&["--yes"], None).0.unwrap_err().exit_code(), 1);
    f.state.lock().unwrap().fail = false;
    let (result, output) = f.run(&["--yes"], None);
    result.unwrap();
    assert!(!output.contains("Pair a TV"));
}
#[test]
fn yes_never_approves_build_dependencies_implicitly() {
    let f = Fixture::new(State {
        paired: true,
        services: true,
        needs_dependencies: true,
        ..State::default()
    });
    let (result, output) = f.run(&["--yes"], None);
    assert_eq!(result.unwrap_err().exit_code(), 3);
    assert!(output.contains("Install development packages?"));
    assert!(!f.state.lock().unwrap().plasma);
    let (result, _) = f.run(&["--yes"], Some("n\n"));
    assert_eq!(result.unwrap_err().exit_code(), 130);
    let (result, _) = f.run(&["--yes", "--allow-build-dependencies"], None);
    result.unwrap();
    assert!(f.state.lock().unwrap().plasma);
}
#[test]
fn noninteractive_explicit_tv_inputs_complete_without_reading_stdin() {
    let f = Fixture::new(State::default());
    let (result, output) = f.run(
        &[
            "--non-interactive",
            "--yes",
            "--tv-ip",
            "192.0.2.1",
            "--tv-mac",
            "02:11:22:33:44:55",
        ],
        None,
    );
    result.unwrap();
    assert!(output.contains("Setup complete."));
    assert!(!output.contains("(q to cancel)"));
}
