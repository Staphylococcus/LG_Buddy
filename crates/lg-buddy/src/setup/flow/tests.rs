use super::*;
use crate::{presentation::brightness::UserFacingError, setup::StepInput};
use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    process::{Command, Stdio},
};

mod helper_process;

fn action() -> StepResponse {
    StepResponse::ActionRequired {
        explanation: "Set up this component.",
        requires_authorization: true,
    }
}
fn failure(retryable: bool) -> StepFailure {
    StepFailure {
        presentation: UserFacingError::new("Incomplete", "Try again."),
        diagnostic: "fixture error".into(),
        retryable,
    }
}
fn index(step: SetupStep) -> usize {
    SetupStep::ORDER.iter().position(|id| *id == step).unwrap()
}

struct State {
    observed: [StepResponse; 3],
    calls: Vec<SetupStep>,
    results: VecDeque<StepResponse>,
    cancelable: bool,
    commit_after_progress: bool,
}
struct FakeSteps(Arc<Mutex<State>>);
impl SetupSteps for FakeSteps {
    fn inspect(&self, step: SetupStep) -> StepResponse {
        self.0.lock().unwrap().observed[index(step)].clone()
    }
    fn execute(
        &self,
        step: SetupStep,
        _: StepAnswer,
        cancellation: &StepCancellation,
        _: &FlowLock,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse {
        let cancelable = self.0.lock().unwrap().cancelable;
        if cancellation.is_cancelled() {
            return StepResponse::Cancelled;
        }
        if !cancelable {
            assert!(cancellation.begin());
        }
        self.0.lock().unwrap().calls.push(step);
        progress(StepResponse::Running {
            message: "Working…",
            cancelable,
        });
        if self.0.lock().unwrap().commit_after_progress {
            if !cancellation.begin() {
                return StepResponse::Cancelled;
            }
            progress(StepResponse::Running {
                message: "Saving…",
                cancelable: false,
            });
        }
        let response = if cancellation.is_cancelled() {
            StepResponse::Cancelled
        } else {
            let mut state = self.0.lock().unwrap();
            let response = state.results.pop_front().unwrap_or(StepResponse::Complete);
            if satisfied(&response) {
                state.observed[index(step)] = response.clone();
            }
            response
        };
        cancellation.finish();
        response
    }
}
struct Fixture {
    root: PathBuf,
    state: Arc<Mutex<State>>,
}
impl Fixture {
    fn new(observed: [StepResponse; 3]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-flow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            state: Arc::new(Mutex::new(State {
                observed,
                calls: vec![],
                results: VecDeque::new(),
                cancelable: false,
                commit_after_progress: false,
            })),
        }
    }
    fn lock(&self) -> PathBuf {
        self.root.join("setup.lock")
    }
    fn open(&self) -> Result<OnboardingFlow, StepFailure> {
        OnboardingFlow::with_backend(Box::new(FakeSteps(self.state.clone())), &self.lock())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn advance(flow: &mut OnboardingFlow) -> FlowSnapshot {
    flow.advance(flow.snapshot().token, StepAnswer::Continue, &mut |_| {})
}

#[test]
fn fresh_setup_runs_in_order_and_pairing_alone_is_not_completion() {
    let fixture = Fixture::new([action(), action(), action()]);
    let mut flow = fixture.open().unwrap();
    assert_eq!(flow.snapshot().current().unwrap().0, SetupStep::Pairing);
    for (expected, remaining) in [
        (SetupStep::Pairing, Some(SetupStep::Services)),
        (SetupStep::Services, Some(SetupStep::Plasma)),
        (SetupStep::Plasma, None),
    ] {
        let snapshot = advance(&mut flow);
        assert_eq!(fixture.state.lock().unwrap().calls.last(), Some(&expected));
        assert_eq!(snapshot.current().map(|(id, _)| *id), remaining);
        assert_eq!(
            snapshot.outcome,
            if remaining.is_none() {
                FlowOutcome::Complete
            } else {
                FlowOutcome::Incomplete
            }
        );
    }
    // A completed flow releases ownership immediately and re-entry only reads.
    let mut second = fixture.open().unwrap();
    assert_eq!(second.snapshot().outcome, FlowOutcome::Complete);
    advance(&mut second);
    assert_eq!(fixture.state.lock().unwrap().calls, SetupStep::ORDER);
}

#[test]
fn partial_setup_skips_verified_and_inapplicable_steps() {
    let fixture = Fixture::new([
        StepResponse::Complete,
        action(),
        StepResponse::NotApplicable,
    ]);
    let mut flow = fixture.open().unwrap();
    assert_eq!(flow.snapshot().current().unwrap().0, SetupStep::Services);
    assert_eq!(advance(&mut flow).outcome, FlowOutcome::Complete);
    assert_eq!(fixture.state.lock().unwrap().calls, [SetupStep::Services]);
}

#[test]
fn common_outcomes_have_identical_transitions_for_every_step() {
    let outcomes = [
        StepResponse::Failed(failure(true)),
        StepResponse::Blocked(failure(false)),
        StepResponse::InputRequired(StepInput::BuildDependencies {
            explanation: "Install tools?",
        }),
        action(),
        StepResponse::Cancelled,
    ];
    for step in SetupStep::ORDER {
        for outcome in &outcomes {
            let mut observed = std::array::from_fn(|_| StepResponse::Complete);
            observed[index(step)] = action();
            let fixture = Fixture::new(observed);
            fixture
                .state
                .lock()
                .unwrap()
                .results
                .push_back(outcome.clone());
            let mut flow = fixture.open().unwrap();
            let snapshot = advance(&mut flow);
            assert_eq!(snapshot.steps[index(step)].1, *outcome);
            assert_eq!(
                snapshot.outcome,
                if *outcome == StepResponse::Cancelled {
                    FlowOutcome::Cancelled
                } else {
                    FlowOutcome::Incomplete
                }
            );
            assert_eq!(fixture.state.lock().unwrap().calls, [step]);
            if let StepResponse::Blocked(_) = outcome {
                advance(&mut flow);
                assert_eq!(fixture.state.lock().unwrap().calls, [step]);
            }
        }
    }
}

#[test]
fn additional_input_is_preserved_until_explicit_reply() {
    let fixture = Fixture::new([StepResponse::Complete, StepResponse::Complete, action()]);
    fixture
        .state
        .lock()
        .unwrap()
        .results
        .push_back(StepResponse::InputRequired(StepInput::BuildDependencies {
            explanation: "Install build tools?",
        }));
    let mut flow = fixture.open().unwrap();
    let snapshot = advance(&mut flow);
    assert!(matches!(
        snapshot.current().unwrap().1,
        StepResponse::InputRequired(_)
    ));
    assert!(fixture.open().is_err());
    assert_eq!(
        flow.advance(
            snapshot.token,
            StepAnswer::InstallBuildDependencies,
            &mut |_| {}
        )
        .outcome,
        FlowOutcome::Complete
    );
    assert_eq!(
        fixture.state.lock().unwrap().calls,
        [SetupStep::Plasma, SetupStep::Plasma]
    );
}

#[test]
fn changed_requirements_and_stale_or_foreign_tokens_never_execute() {
    let fixture = Fixture::new([
        StepResponse::Complete,
        action(),
        StepResponse::NotApplicable,
    ]);
    let mut flow = fixture.open().unwrap();
    let old = flow.snapshot().token;
    fixture.state.lock().unwrap().observed[0] = action();
    assert_eq!(advance(&mut flow).current().unwrap().0, SetupStep::Pairing);
    flow.advance(old, StepAnswer::Continue, &mut |_| panic!("stale"));
    assert!(fixture.state.lock().unwrap().calls.is_empty());
    drop(flow);
    let mut reopened = fixture.open().unwrap();
    reopened.advance(old, StepAnswer::Continue, &mut |_| panic!("foreign"));
    assert!(fixture.state.lock().unwrap().calls.is_empty());
}

#[test]
fn completion_rechecks_earlier_steps_and_failures_are_not_hidden() {
    let fixture = Fixture::new([
        StepResponse::Complete,
        action(),
        StepResponse::NotApplicable,
    ]);
    let mut flow = fixture.open().unwrap();
    let state = fixture.state.clone();
    let snapshot = flow.advance(flow.snapshot().token, StepAnswer::Continue, &mut |_| {
        state.lock().unwrap().observed[0] = action();
    });
    assert_eq!(snapshot.current().unwrap().0, SetupStep::Pairing);
    assert_eq!(snapshot.outcome, FlowOutcome::Incomplete);
    fixture
        .state
        .lock()
        .unwrap()
        .results
        .push_back(StepResponse::Failed(failure(true)));
    let snapshot = flow.advance(snapshot.token, StepAnswer::Continue, &mut |_| {
        state.lock().unwrap().observed[0] = StepResponse::Complete;
    });
    assert_eq!(snapshot.outcome, FlowOutcome::Incomplete);
    assert!(matches!(
        snapshot.current().unwrap().1,
        StepResponse::Failed(_)
    ));
    assert_eq!(flow.refresh().outcome, FlowOutcome::Complete);
}

#[test]
fn cancellation_and_drop_release_waiting_flows_and_resume_from_current_facts() {
    let fixture = Fixture::new([action(), action(), action()]);
    let mut flow = fixture.open().unwrap();
    advance(&mut flow);
    let cancellation = flow.cancellation();
    assert!(fixture.open().is_err());
    assert!(cancellation.cancel());
    assert_eq!(flow.snapshot().outcome, FlowOutcome::Cancelled);
    let reopened = fixture.open().unwrap();
    assert_eq!(
        reopened.snapshot().current().unwrap().0,
        SetupStep::Services
    );
    let surviving_handle = reopened.cancellation();
    drop(reopened);
    assert!(!surviving_handle.can_cancel());
    assert!(fixture.open().is_ok());
}

#[test]
fn cancellation_respects_live_step_gate_and_keeps_ownership_until_work_returns() {
    for cancelable in [false, true] {
        let fixture = Fixture::new([action(), action(), action()]);
        fixture.state.lock().unwrap().cancelable = cancelable;
        let mut flow = fixture.open().unwrap();
        let cancellation = flow.cancellation();
        let snapshot = flow.advance(flow.snapshot().token, StepAnswer::Continue, &mut |event| {
            assert_eq!(event.step, SetupStep::Pairing);
            assert_eq!(cancellation.can_cancel(), cancelable);
            assert_eq!(cancellation.cancel(), cancelable);
            assert!(fixture.open().is_err());
            assert!(child(&fixture.lock(), "busy")
                .output()
                .unwrap()
                .status
                .success());
        });
        if cancelable {
            assert_eq!(snapshot.outcome, FlowOutcome::Cancelled);
            assert!(fixture.open().is_ok());
        } else {
            assert_eq!(snapshot.current().unwrap().0, SetupStep::Services);
            assert_eq!(snapshot.outcome, FlowOutcome::Incomplete);
            assert!(fixture.open().is_err());
            assert!(cancellation.can_cancel());
        }
    }
}

#[test]
fn cancellation_from_another_thread_rechecks_the_steps_commit_boundary() {
    let fixture = Fixture::new([action(), action(), StepResponse::NotApplicable]);
    {
        let mut state = fixture.state.lock().unwrap();
        state.cancelable = true;
        state.commit_after_progress = true;
    }
    let mut flow = fixture.open().unwrap();
    let cancellation = flow.cancellation();
    let (events, receive) = std::sync::mpsc::channel();
    let (resume, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        flow.advance(flow.snapshot().token, StepAnswer::Continue, &mut |event| {
            events.send(event).unwrap();
            wait.recv().unwrap();
        });
        flow
    });
    assert!(matches!(
        receive.recv().unwrap().response,
        StepResponse::Running {
            cancelable: true,
            ..
        }
    ));
    assert!(cancellation.can_cancel());
    resume.send(()).unwrap();
    assert!(matches!(
        receive.recv().unwrap().response,
        StepResponse::Running {
            cancelable: false,
            ..
        }
    ));
    assert!(!cancellation.cancel());
    assert!(fixture.open().is_err());
    resume.send(()).unwrap();
    let flow = worker.join().unwrap();
    assert_eq!(flow.snapshot().current().unwrap().0, SetupStep::Services);
    assert!(fixture.open().is_err());
    assert!(cancellation.cancel());
    assert!(fixture.open().is_ok());
}

fn child(path: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "setup::flow::tests::lock_probe_child",
            "--nocapture",
        ])
        .env("LG_BUDDY_FLOW_LOCK_TEST", path)
        .env("LG_BUDDY_FLOW_LOCK_MODE", mode);
    command
}

#[test]
fn lock_probe_child() {
    let Some(path) = std::env::var_os("LG_BUDDY_FLOW_LOCK_TEST") else {
        return;
    };
    let mode = std::env::var("LG_BUDDY_FLOW_LOCK_MODE").unwrap();
    let result = FlowLock::acquire(Path::new(&path));
    if mode == "busy" {
        assert!(result.is_err());
        return;
    }
    let _lock = result.unwrap();
    if mode == "hold" {
        println!("LOCK_READY");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    }
}

#[test]
fn competing_processes_are_excluded_and_process_death_releases_the_same_inode() {
    let fixture = Fixture::new([action(), action(), action()]);
    let flow = fixture.open().unwrap();
    let inode = fs::metadata(fixture.lock()).unwrap().ino();
    assert!(child(&fixture.lock(), "busy")
        .output()
        .unwrap()
        .status
        .success());
    drop(flow);
    let mut process = child(&fixture.lock(), "hold")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(process.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child exited before acquiring lock"
        );
        if line.trim() == "LOCK_READY" {
            break;
        }
    }
    assert!(fixture.open().is_err());
    process.kill().unwrap();
    process.wait().unwrap();
    assert!(fixture.open().is_ok());
    assert_eq!(fs::metadata(fixture.lock()).unwrap().ino(), inode);
}

#[test]
fn symlink_lock_is_rejected_without_touching_its_target() {
    let fixture = Fixture::new([action(), action(), action()]);
    let target = fixture.root.join("untouched");
    fs::write(&target, "original").unwrap();
    std::os::unix::fs::symlink(&target, fixture.lock()).unwrap();
    assert!(fixture.open().is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "original");
}
