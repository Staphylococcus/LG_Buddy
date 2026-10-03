use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn complete() -> SetupAssessment {
    SetupAssessment {
        steps: [
            (SetupStep::Pairing, StepResponse::Complete),
            (SetupStep::Services, StepResponse::Complete),
            (SetupStep::Plasma, StepResponse::NotApplicable),
        ],
    }
}

fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
}

struct BlockingAssessment {
    calls: Arc<AtomicUsize>,
    release: Mutex<mpsc::Receiver<SetupAssessment>>,
}
impl AssessmentBackend for BlockingAssessment {
    fn assess(&self) -> Result<SetupAssessment, StepFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.release.lock().unwrap().recv().unwrap())
    }
}

#[test]
fn reads_keep_the_published_result_while_probes_run_and_old_results_are_rejected() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (release, incoming) = mpsc::channel();
    let worker = AssessmentWorker::spawn(
        BlockingAssessment {
            calls: calls.clone(),
            release: Mutex::new(incoming),
        },
        PathBuf::from("/config"),
    );
    until(|| calls.load(Ordering::SeqCst) == 1);
    assert_eq!(worker.published.snapshot().status, SetupStatus::Unchecked);
    release.send(complete()).unwrap();
    until(|| worker.published.snapshot().status == SetupStatus::Complete);
    let previous = worker.published.snapshot();
    worker.published.request().unwrap();
    until(|| calls.load(Ordering::SeqCst) == 2);
    let started = Instant::now();
    assert_eq!(worker.published.snapshot(), previous);
    assert!(started.elapsed() < Duration::from_millis(100));
    let expected = worker.published.request().unwrap();
    let mut missing = complete();
    missing.steps[2].1 = StepResponse::ActionRequired {
        explanation: "Install KWin bridge",
        requires_authorization: true,
    };
    release.send(missing).unwrap();
    until(|| calls.load(Ordering::SeqCst) == 3);
    assert_eq!(worker.published.snapshot(), previous);
    release.send(complete()).unwrap();
    until(|| worker.published.snapshot().revision == expected);
    assert_eq!(worker.published.snapshot().status, SetupStatus::Complete);
    // Other endpoint handles may outlive the worker without preventing shutdown.
    let endpoint = worker.published.clone();
    drop(worker);
    assert!(endpoint.request().is_err());
}

#[test]
fn applicable_missing_integration_is_incomplete_but_inapplicable_is_satisfied() {
    let mut assessment = complete();
    assert_eq!(
        SetupSnapshot::from_assessment(Ok(assessment.clone())).status,
        SetupStatus::Complete
    );
    assessment.steps[2].1 = StepResponse::ActionRequired {
        explanation: "Missing integration",
        requires_authorization: true,
    };
    let snapshot = SetupSnapshot::from_assessment(Ok(assessment));
    assert_eq!(snapshot.status, SetupStatus::Incomplete);
    assert_eq!(snapshot.requirements[0].step, "plasma");
    assert!(snapshot.requirements[0].actionable);
}

#[test]
fn a_panicking_probe_publishes_failure_and_can_be_retried() {
    struct PanicOnce(AtomicUsize);
    impl AssessmentBackend for PanicOnce {
        fn assess(&self) -> Result<SetupAssessment, StepFailure> {
            assert_ne!(self.0.fetch_add(1, Ordering::SeqCst), 0, "failed probe");
            Ok(complete())
        }
    }
    let worker = AssessmentWorker::spawn(PanicOnce(AtomicUsize::new(0)), PathBuf::from("/config"));
    until(|| worker.published.snapshot().revision == 1);
    assert_eq!(worker.published.snapshot().status, SetupStatus::Incomplete);
    let revision = worker.published.request().unwrap();
    until(|| worker.published.snapshot().revision == revision);
    assert_eq!(worker.published.snapshot().status, SetupStatus::Complete);
}
