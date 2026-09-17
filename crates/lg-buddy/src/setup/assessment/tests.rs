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
fn changes_coalesce_and_stale_completion_cannot_replace_flow_state() {
    let mut health = SetupHealth::default();
    let old = health.request().unwrap();
    assert!(health.request().is_none());
    assert!(health.set_paused(true).is_none());
    health.observe_flow(SetupStatus::Complete);
    assert!(health.request().is_none());
    assert!(health.set_paused(false).is_none());
    let next = health
        .complete(old, Err(worker_stopped()))
        .unwrap()
        .unwrap();
    assert_eq!(health.status(), SetupStatus::Complete);
    assert!(health
        .complete(old, snapshot(StepResponse::Complete))
        .is_none());
    assert_eq!(
        health.complete(next, snapshot(StepResponse::NotApplicable)),
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
        health.complete(old, snapshot(StepResponse::Complete)),
        Some(None)
    );
    assert_eq!(health.status(), SetupStatus::Unchecked);
    let fresh = health.set_paused(false).unwrap();
    assert_eq!(health.complete(fresh, Err(worker_stopped())), Some(None));
    assert_eq!(health.status(), SetupStatus::Incomplete);
}

#[test]
fn shutdown_rejects_late_results_and_new_work() {
    let mut health = SetupHealth::default();
    let old = health.request().unwrap();
    health.shutdown();
    assert!(health
        .complete(old, snapshot(StepResponse::Complete))
        .is_none());
    assert!(health.request().is_none());
}
