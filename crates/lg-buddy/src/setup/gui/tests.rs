use super::*;
fn finish(
    app: &mut OnboardingApplication,
    update: OnboardingTransition,
    fixture: &fixtures::Fixture,
) -> OnboardingTransition {
    let operation = update.operation.unwrap();
    let result = operation.execute_with(fixture, &mut |progress| {
        app.progress(&operation, progress);
    });
    app.complete(&operation, result).unwrap()
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
    let services = finish(&mut app, pair, &fixture);
    assert_eq!(services.presentation.unwrap().title, "Background services");
    assert_eq!(app.status(), SetupStatus::Incomplete);
    let action = app.handle(OnboardingIntent::Submit).unwrap();
    finish(&mut app, action, &fixture);
    let action = app.handle(OnboardingIntent::Submit).unwrap();
    let deps = finish(&mut app, action, &fixture);
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
    let ready = finish(&mut app, opening, &fixture);
    assert_eq!(ready.presentation.unwrap().title, "Background services");
    let action = app.handle(OnboardingIntent::Submit).unwrap();
    let operation = action.operation.unwrap();
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
    finish(&mut app, retry, &fixture);
    let action = app.handle(OnboardingIntent::Submit).unwrap();
    let failed = app.worker_stopped(&action.operation.unwrap()).unwrap();
    assert_eq!(failed.presentation.unwrap().action, Some("Retry"));
    let retry = app.handle(OnboardingIntent::Submit).unwrap();
    finish(&mut app, retry, &fixture);
    assert_eq!(app.status(), SetupStatus::Incomplete);
}

#[test]
fn accepted_running_cancellation_waits_for_the_worker_and_preserves_exclusion() {
    use crate::setup::{flow::SetupSteps, lock::FlowLock, StepCancellation};
    use std::sync::mpsc;
    struct PairingWait(mpsc::Sender<()>, Mutex<mpsc::Receiver<()>>);
    impl SetupSteps for PairingWait {
        fn inspect(&self, step: SetupStep) -> StepResponse {
            if step == SetupStep::Pairing {
                StepResponse::InputRequired(StepInput::Pairing { saved: None })
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
    impl crate::setup::assessment::AssessmentBackend for Backend {
        fn assess(&self) -> Result<crate::setup::assessment::SetupAssessment, StepFailure> {
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
    let root = std::env::temp_dir().join(format!("lg-buddy-cancel-gui-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("lock");
    let flow = OnboardingFlow::with_backend(
        Box::new(PairingWait(started, Mutex::new(release_rx))),
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
    app.complete(&operation, result).unwrap();
    drop(operation);
    app.handle(OnboardingIntent::SetAddress("192.0.2.1".into()));
    app.handle(OnboardingIntent::SetMac("02:11:22:33:44:55".into()));
    let operation = app
        .handle(OnboardingIntent::Submit)
        .unwrap()
        .operation
        .unwrap();
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
