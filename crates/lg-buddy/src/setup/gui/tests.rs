use super::*;
fn finish(
    app: &mut OnboardingApplication,
    update: OnboardingTransition,
    fixture: &fixtures::Fixture,
) -> OnboardingTransition {
    let mut update = update;
    loop {
        let operation = update.operation.unwrap();
        let result = operation.execute_with(fixture, &mut |progress| {
            assert!(app.progress(&operation, progress).is_some());
        });
        update = app.complete(&operation, result).unwrap();
        if update.operation.is_none() {
            return update;
        }
    }
}
#[test]
fn pairing_services_dependencies_and_verified_completion_share_one_modal() {
    let fixture = fixtures::Fixture::new(false, true);
    let mut app = OnboardingApplication::default();
    let opening = app.handle(OnboardingIntent::Open).unwrap();
    let ready = finish(&mut app, opening, &fixture);
    assert_eq!(ready.presentation.unwrap().title, "Pair a TV");
    let invalid = app.handle(OnboardingIntent::Submit).unwrap();
    assert!(invalid.presentation.unwrap().error.is_some());
    assert!(invalid.operation.is_none());
    app.handle(OnboardingIntent::SetAddress("192.0.2.1".into()));
    app.handle(OnboardingIntent::SetMac("02:11:22:33:44:55".into()));
    let pair = app.handle(OnboardingIntent::Submit).unwrap();
    let deps = finish(&mut app, pair, &fixture);
    assert_eq!(app.status(), SetupStatus::Incomplete);
    assert_eq!(
        deps.presentation.unwrap().action,
        Some("Install build tools")
    );
    let action = app.handle(OnboardingIntent::Submit).unwrap();
    let complete = finish(&mut app, action, &fixture);
    assert_eq!(complete.presentation.unwrap().title, "Setup complete");
    assert_eq!(app.status(), SetupStatus::Complete);
    assert!(app
        .handle(OnboardingIntent::Submit)
        .unwrap()
        .presentation
        .is_none());
    let calls = fixture.calls.lock().unwrap().len();
    let opening = app.handle(OnboardingIntent::Open).unwrap();
    finish(&mut app, opening, &fixture);
    assert_eq!(fixture.calls.lock().unwrap().len(), calls);
}
#[test]
fn cancel_reopen_rejects_old_results_and_only_repairs_remaining_steps() {
    let fixture = fixtures::Fixture::new(true, false);
    let mut app = OnboardingApplication::default();
    let opening = app.handle(OnboardingIntent::Open).unwrap();
    let old = opening.operation.clone().unwrap();
    let result = old.execute_with(&fixture, &mut |_| {});
    app.handle(OnboardingIntent::Cancel).unwrap();
    assert!(app.complete(&old, result).is_none());
    drop(old);
    drop(opening);
    let opening = app.handle(OnboardingIntent::Open).unwrap();
    let inspect = opening.operation.unwrap();
    let result = inspect.execute_with(&fixture, &mut |_| {});
    let ready = app.complete(&inspect, result).unwrap();
    let operation = ready.operation.unwrap();
    // The UI can still show the old cancelable state when the live step has
    // crossed into a mutation. It must not close or queue cancellation.
    let result = operation.execute_with(&fixture, &mut |_| {
        assert!(app.handle(OnboardingIntent::Cancel).is_none());
    });
    let done = app.complete(&operation, result).unwrap();
    assert_eq!(done.presentation.unwrap().title, "Setup complete");
    assert_eq!(*fixture.calls.lock().unwrap(), [SetupStep::Services]);
}
#[test]
fn worker_failure_retries_with_a_fresh_flow_and_read_only_errors_stay_visible() {
    let fixture = fixtures::Fixture::new(true, false);
    let mut app = OnboardingApplication::default();
    let opening = app.handle(OnboardingIntent::Open).unwrap();
    let failed = app.worker_stopped(&opening.operation.unwrap()).unwrap();
    assert!(failed.presentation.unwrap().error.is_some());
    let retry = app.handle(OnboardingIntent::Submit).unwrap();
    let inspect = retry.operation.unwrap();
    let result = inspect.execute_with(&fixture, &mut |_| {});
    let action = app.complete(&inspect, result).unwrap();
    drop(inspect);
    let failed = app.worker_stopped(&action.operation.unwrap()).unwrap();
    assert_eq!(failed.presentation.unwrap().action, Some("Retry"));
    let retry = app.handle(OnboardingIntent::Submit).unwrap();
    finish(&mut app, retry, &fixture);
    assert_eq!(app.status(), SetupStatus::Complete);
}

#[test]
fn recheck_returns_new_requirements_without_automatically_executing_them() {
    use crate::setup::recovery::{RecoveryAction, RecoveryCause, RepairBoundary, SetupRecovery};
    let fixture = fixtures::Fixture::new(true, false);
    fixture.responses.lock().unwrap()[1] = StepResponse::Blocked(StepFailure {
        presentation: UserFacingError::new(
            "Externally managed",
            "Repair externally, then recheck.",
        ),
        diagnostic: "fixture managed service".into(),
        recovery: SetupRecovery::new(
            RecoveryCause::ManagedInstallation,
            RepairBoundary::SystemConfiguration,
            RecoveryAction::RepairExternally,
        ),
        retryable: true,
    });
    let mut app = OnboardingApplication::default();
    let opening = app.handle(OnboardingIntent::Open).unwrap();
    let blocked = finish(&mut app, opening, &fixture);
    assert_eq!(blocked.presentation.unwrap().action, Some("Recheck"));
    fixture.responses.lock().unwrap()[1] = StepResponse::ActionRequired {
        explanation: "Repair services",
        requires_authorization: true,
    };
    let recheck = app.handle(OnboardingIntent::Submit).unwrap();
    assert!(!recheck.operation.as_ref().unwrap().changes_setup());
    let checked = finish(&mut app, recheck, &fixture);
    assert_eq!(checked.presentation.unwrap().action, Some("Continue"));
    assert!(fixture.calls.lock().unwrap().is_empty());
    let approved = app.handle(OnboardingIntent::Submit).unwrap();
    finish(&mut app, approved, &fixture);
    assert_eq!(*fixture.calls.lock().unwrap(), [SetupStep::Services]);
}

#[test]
fn accepted_running_cancellation_waits_for_the_worker_and_preserves_exclusion() {
    cancellation_waits_for_worker(SetupStep::Pairing);
    cancellation_waits_for_worker(SetupStep::Services);
}

fn cancellation_waits_for_worker(step: SetupStep) {
    use crate::setup::{flow::SetupSteps, lock::FlowLock, StepCancellation};
    use std::sync::mpsc;
    struct StepWait(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>, SetupStep);
    impl SetupSteps for StepWait {
        fn inspect(&self, step: SetupStep) -> StepResponse {
            if step == self.2 {
                if step == SetupStep::Pairing {
                    StepResponse::InputRequired(StepInput::Pairing { saved: None })
                } else {
                    StepResponse::ActionRequired {
                        explanation: "Repair services.",
                        requires_authorization: false,
                    }
                }
            } else {
                StepResponse::NotApplicable
            }
        }
        fn execute(
            &self,
            _: SetupStep,
            _: StepAnswer,
            cancellation: &StepCancellation,
            _: &FlowLock,
            _: &mut dyn FnMut(StepResponse),
        ) -> StepResponse {
            self.0.send(()).unwrap();
            self.1.lock().unwrap().recv().unwrap();
            assert!(cancellation.is_cancelled());
            StepResponse::Cancelled
        }
    }
    struct Backend(Mutex<Option<OnboardingFlow>>);
    impl crate::setup::published::SnapshotBackend for Backend {
        fn snapshot(&self) -> Result<crate::setup::published::SetupSnapshot, StepFailure> {
            unreachable!("this test drives the flow directly")
        }
        fn request_reassessment(&self) -> Result<(String, u64), StepFailure> {
            unreachable!("this test drives the flow directly")
        }
    }
    impl OnboardingBackend for Backend {
        fn open(&self) -> Result<OnboardingFlow, StepFailure> {
            Ok(self.0.lock().unwrap().take().unwrap())
        }
    }
    let (started, start_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let root = std::env::temp_dir().join(format!(
        "lg-buddy-cancel-gui-{}-{step:?}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("lock");
    let flow = OnboardingFlow::with_backend(
        Box::new(StepWait(started, Mutex::new(release_rx), step)),
        &path,
    )
    .unwrap();
    let backend = Backend(Mutex::new(Some(flow)));
    let mut app = OnboardingApplication::default();
    let operation = app
        .handle(OnboardingIntent::Open)
        .unwrap()
        .operation
        .unwrap();
    let result = operation.execute_with(&backend, &mut |_| {});
    let ready = app.complete(&operation, result).unwrap();
    drop(operation);
    let operation = if step == SetupStep::Pairing {
        app.handle(OnboardingIntent::SetAddress("192.0.2.1".into()));
        app.handle(OnboardingIntent::SetMac("02:11:22:33:44:55".into()));
        app.handle(OnboardingIntent::Submit)
            .unwrap()
            .operation
            .unwrap()
    } else {
        ready.operation.unwrap()
    };
    let worker = operation.clone();
    let join = std::thread::spawn(move || worker.execute(&mut |_| {}));
    start_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let cancelled = app.handle(OnboardingIntent::Cancel).unwrap();
    assert!(cancelled.presentation.unwrap().busy);
    assert!(app.is_open());
    assert!(app.handle(OnboardingIntent::Open).is_none());
    assert!(FlowLock::acquire(&path).is_err());
    release.send(()).unwrap();
    assert!(app
        .complete(&operation, join.join().unwrap())
        .unwrap()
        .presentation
        .is_none());
    assert!(FlowLock::acquire(&path).is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cancelling_before_automatic_work_is_dispatched_never_executes_repairs() {
    let fixture = fixtures::Fixture::new(true, false);
    let mut app = OnboardingApplication::default();
    let inspect = app
        .handle(OnboardingIntent::Open)
        .unwrap()
        .operation
        .unwrap();
    assert!(!inspect.changes_setup());
    let result = inspect.execute_with(&fixture, &mut |_| {});
    let ready = app.complete(&inspect, result).unwrap();
    let run = ready.operation.unwrap();
    assert!(run.changes_setup());
    app.handle(OnboardingIntent::Cancel).unwrap();
    let result = run.execute_with(&fixture, &mut |_| panic!("cancelled work ran"));
    assert!(app.complete(&run, result).unwrap().presentation.is_none());
    assert!(fixture.calls.lock().unwrap().is_empty());
    assert!(fixture.open().is_ok());
}

#[test]
fn retry_resumes_routine_work_without_a_second_continue_prompt() {
    let fixture = fixtures::Fixture::new(true, false);
    fixture.responses.lock().unwrap()[1] = StepResponse::Failed(stopped());
    let mut app = OnboardingApplication::default();
    let opening = app.handle(OnboardingIntent::Open).unwrap();
    let failed = finish(&mut app, opening, &fixture);
    assert_eq!(failed.presentation.unwrap().action, Some("Retry"));
    assert!(fixture.calls.lock().unwrap().is_empty());
    fixture.responses.lock().unwrap()[1] = StepResponse::ActionRequired {
        explanation: "Repair services.",
        requires_authorization: true,
    };
    let retry = app.handle(OnboardingIntent::Submit).unwrap();
    let done = finish(&mut app, retry, &fixture);
    assert_eq!(done.presentation.unwrap().title, "Setup complete");
    assert_eq!(*fixture.calls.lock().unwrap(), [SetupStep::Services]);
}
