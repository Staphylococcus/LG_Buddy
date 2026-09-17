//! Read-only native setup probe for desktop-session validation. Run as the
//! desktop user, with the same environment/configuration as the application.
use lg_buddy::setup::assessment::{AssessmentBackend, EnvironmentAssessmentBackend};

fn main() {
    match EnvironmentAssessmentBackend.assess() {
        Ok(assessment) => {
            println!("Setup: {:?}", assessment.status());
            for (step, response) in assessment.steps {
                println!("{step:?}: {response:?}");
            }
        }
        Err(failure) => {
            eprintln!("Setup inspection failed: {}", failure.diagnostic);
            std::process::exit(1);
        }
    }
}
