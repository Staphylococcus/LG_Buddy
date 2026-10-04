use super::*;
use crate::setup::{gui::fixtures::Fixture, StepInput};

fn snapshot(plasma: StepResponse) -> Result<SetupAssessment, StepFailure> {
    Ok(SetupAssessment {
        steps: [
            (SetupStep::Pairing, StepResponse::Complete),
            (SetupStep::Services, StepResponse::Complete),
            (SetupStep::Plasma, plasma),
        ],
    })
}

fn read(plasma: StepResponse) -> Result<AssessmentRead, StepFailure> {
    Ok(AssessmentRead {
        snapshot: super::super::published::SetupSnapshot::from_assessment(snapshot(plasma)),
        requested: None,
    })
}

#[test]
fn only_verified_or_inapplicable_steps_satisfy_setup() {
    for response in [StepResponse::Complete, StepResponse::NotApplicable] {
        assert_eq!(snapshot(response).unwrap().status(), SetupStatus::Complete);
    }
    for response in [
        StepResponse::InputRequired(StepInput::Pairing { saved: None }),
        StepResponse::ActionRequired {
            explanation: "Missing",
            requires_authorization: true,
        },
        StepResponse::Running {
            message: "Checking",
            cancelable: false,
        },
        StepResponse::Cancelled,
        StepResponse::Blocked(worker_stopped()),
        StepResponse::Failed(worker_stopped()),
    ] {
        for index in 0..3 {
            let mut result = snapshot(StepResponse::Complete).unwrap();
            result.steps[index].1 = response.clone();
            assert_eq!(result.status(), SetupStatus::Incomplete);
        }
    }
}

#[test]
fn reassessment_can_gain_and_lose_requirements_without_execution() {
    let fixture = Fixture::new(true, false);
    let mut health = SetupHealth::default();
    assert_eq!(health.status(), SetupStatus::Unchecked);
    // GNOME still needs common services, but no KWin bridge.
    for (services, plasma, status) in [
        (
            StepResponse::Failed(worker_stopped()),
            StepResponse::NotApplicable,
            SetupStatus::Incomplete,
        ),
        (
            StepResponse::Complete,
            StepResponse::NotApplicable,
            SetupStatus::Complete,
        ),
        (
            StepResponse::Complete,
            StepResponse::Failed(worker_stopped()),
            SetupStatus::Incomplete,
        ),
        (
            StepResponse::Complete,
            StepResponse::Complete,
            SetupStatus::Complete,
        ),
        (
            StepResponse::Failed(worker_stopped()),
            StepResponse::Complete,
            SetupStatus::Incomplete,
        ),
    ] {
        fixture.responses.lock().unwrap()[1..].clone_from_slice(&[services, plasma]);
        fixture.publish();
        let operation = health.request().unwrap();
        assert_eq!(
            health.complete(operation, operation.execute(&fixture)),
            Some(None)
        );
        assert_eq!(health.status(), status);
    }
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[test]
fn failed_cached_reads_retain_the_last_published_status() {
    for status in [
        SetupStatus::Unchecked,
        SetupStatus::Incomplete,
        SetupStatus::Complete,
    ] {
        let mut health = SetupHealth::default();
        let operation = health.request().unwrap();
        let mut published = read(StepResponse::Complete).unwrap();
        published.snapshot.status = status;
        health.complete(operation, Ok(published)).unwrap();
        for _ in 0..3 {
            let operation = health.request().unwrap();
            assert!(!operation.1, "cached retries must not request assessment");
            assert_eq!(
                health.complete(operation, Err(worker_stopped())),
                Some(None)
            );
            assert_eq!(health.status(), status);
            assert!(!health.verification_pending());
        }
    }
}

#[test]
fn read_failure_and_unchecked_replacement_cannot_clear_a_verification_barrier() {
    let mut health = SetupHealth::default();
    let first = health.request().unwrap();
    let mut published = read(StepResponse::Complete).unwrap();
    published.snapshot.instance = "daemon-a".into();
    published.snapshot.revision = 1;
    health.complete(first, Ok(published.clone())).unwrap();
    health.changed();
    let requested = health.request().unwrap();
    published.requested = Some(("daemon-a".into(), 2));
    health.complete(requested, Ok(published)).unwrap();
    assert!(health.verification_pending());

    let failed = health.request().unwrap();
    health.complete(failed, Err(worker_stopped())).unwrap();
    assert_eq!(health.status(), SetupStatus::Complete);
    assert!(health.verification_pending());

    let replacement = health.request().unwrap();
    let mut starting = read(StepResponse::Complete).unwrap();
    starting.snapshot.instance = "daemon-b".into();
    starting.snapshot.status = SetupStatus::Unchecked;
    health.complete(replacement, Ok(starting)).unwrap();
    assert_eq!(health.status(), SetupStatus::Complete);
    assert!(health.verification_pending());

    let assessed = health.request().unwrap();
    let mut incomplete = read(StepResponse::Failed(worker_stopped())).unwrap();
    incomplete.snapshot.instance = "daemon-b".into();
    incomplete.snapshot.revision = 1;
    health.complete(assessed, Ok(incomplete)).unwrap();
    assert_eq!(health.status(), SetupStatus::Incomplete);
    assert!(!health.verification_pending());
}

#[test]
fn an_unchecked_daemon_replacement_is_not_a_new_assessment() {
    let mut health = SetupHealth::default();
    let first = health.request().unwrap();
    let mut published = read(StepResponse::Complete).unwrap();
    published.snapshot.instance = "daemon-a".into();
    published.snapshot.revision = 1;
    health.complete(first, Ok(published)).unwrap();

    let replacement = health.request().unwrap();
    let mut starting = read(StepResponse::Complete).unwrap();
    starting.snapshot.instance = "daemon-b".into();
    starting.snapshot.status = SetupStatus::Unchecked;
    health.complete(replacement, Ok(starting)).unwrap();
    assert_eq!(health.status(), SetupStatus::Complete);

    let assessed = health.request().unwrap();
    let mut incomplete = read(StepResponse::Failed(worker_stopped())).unwrap();
    incomplete.snapshot.instance = "daemon-b".into();
    incomplete.snapshot.revision = 1;
    health.complete(assessed, Ok(incomplete)).unwrap();
    assert_eq!(health.status(), SetupStatus::Incomplete);
}

#[test]
fn changes_coalesce_and_stale_completion_cannot_replace_flow_state() {
    let mut health = SetupHealth::default();
    let old = health.request().unwrap();
    assert!(health.request().is_none());
    assert!(health.set_paused(true).is_none());
    health.changed();
    assert!(health.request().is_none());
    assert!(health.set_paused(false).is_none());
    let next = health
        .complete(old, Err(worker_stopped()))
        .unwrap()
        .unwrap();
    assert_eq!(health.status(), SetupStatus::Unchecked);
    assert!(health.complete(old, read(StepResponse::Complete)).is_none());
    assert_eq!(
        health.complete(
            next,
            Ok(AssessmentRead {
                requested: Some((String::new(), 0)),
                ..read(StepResponse::NotApplicable).unwrap()
            })
        ),
        Some(None)
    );
    assert_eq!(health.status(), SetupStatus::Complete);
}

#[test]
fn check_finishing_during_setup_waits_for_setup_to_close() {
    let mut health = SetupHealth::default();
    let old = health.request().unwrap();
    health.set_paused(true);
    assert_eq!(
        health.complete(old, read(StepResponse::Complete)),
        Some(None)
    );
    assert_eq!(health.status(), SetupStatus::Unchecked);
    let fresh = health.set_paused(false).unwrap();
    assert_eq!(health.complete(fresh, Err(worker_stopped())), Some(None));
    assert_eq!(health.status(), SetupStatus::Unchecked);
}

#[test]
fn shutdown_rejects_late_results_and_new_work() {
    let mut health = SetupHealth::default();
    let old = health.request().unwrap();
    health.shutdown();
    assert!(health.complete(old, read(StepResponse::Complete)).is_none());
    assert!(health.request().is_none());
}

#[test]
fn only_the_requested_revision_or_a_new_daemon_can_verify_a_finished_repair() {
    let mut health = SetupHealth::default();
    health.changed();
    let first = health.request().unwrap();
    let mut pending = read(StepResponse::Complete).unwrap();
    pending.snapshot.instance = "daemon-a".into();
    pending.snapshot.revision = 10;
    pending.requested = Some(("daemon-a".into(), 12));
    health.complete(first, Ok(pending.clone())).unwrap();
    assert_ne!(health.status(), SetupStatus::Complete);
    pending.requested = None;
    pending.snapshot.revision = 11;
    let next = health.request().unwrap();
    health.complete(next, Ok(pending.clone())).unwrap();
    assert_ne!(health.status(), SetupStatus::Complete);
    pending.snapshot.revision = 12;
    let next = health.request().unwrap();
    health.complete(next, Ok(pending.clone())).unwrap();
    assert_eq!(health.status(), SetupStatus::Complete);
    health.changed();
    let next = health.request().unwrap();
    pending.requested = Some(("daemon-a".into(), 13));
    pending.snapshot.instance = "daemon-b".into();
    pending.snapshot.revision = 1;
    health.complete(next, Ok(pending)).unwrap();
    assert_eq!(health.status(), SetupStatus::Complete);
}
