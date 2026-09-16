//! Pairing readiness is local: an offline TV does not require another pairing.
use super::{StepCancellation, StepFailure, StepInput, StepResponse};
use crate::config::TvPlatform;
use crate::pairing::{
    PairingError, PairingFailure, PairingOperation, PairingOutcome, PairingRequest, PairingStage,
};
use crate::presentation::brightness::UserFacingError;
use crate::settings::SettingsStore;
use crate::tvs::{read_profiles_from_store, TvCredentialState};
use std::path::Path;

pub(crate) fn inspect(path: &Path) -> StepResponse {
    let profiles = SettingsStore::load(path)
        .map_err(|e| e.to_string())
        .and_then(|store| read_profiles_from_store(path, &store).map_err(|e| e.to_string()));
    match profiles {
        Err(error) => failed("The saved TV configuration could not be read.", error),
        Ok(profiles) => match profiles.first() {
            None => StepResponse::InputRequired(StepInput::Pairing { saved: None }),
            Some(profile) => match profile.credentials() {
                TvCredentialState::Stored | TvCredentialState::LocalFile => StepResponse::Complete,
                TvCredentialState::Missing | TvCredentialState::Malformed
                    if profile.platform() == TvPlatform::LgWebOs =>
                {
                    match PairingRequest::parse(
                        &profile.address().to_string(),
                        &profile.mac().to_string(),
                        profile.input(),
                    ) {
                        Ok(request) => StepResponse::InputRequired(StepInput::Pairing {
                            saved: Some(request),
                        }),
                        Err(error) => failed(
                            "The saved TV configuration needs correction.",
                            error.detail(),
                        ),
                    }
                }
                _ => failed(
                    "The saved TV credential could not be checked.",
                    "local credential unavailable or unreadable",
                ),
            },
        },
    }
}

pub(crate) fn execute(
    path: &Path,
    request: Option<PairingRequest>,
    cancellation: &StepCancellation,
    progress: &mut dyn FnMut(StepResponse),
) -> StepResponse {
    execute_with(
        path,
        request,
        cancellation,
        progress,
        |operation, progress| {
            crate::pairing::pair_and_save_webos(
                operation,
                path,
                crate::web_os::WebOsEndpoint::wss(operation.request().address()),
                progress,
            )
        },
    )
}

fn execute_with(
    path: &Path,
    request: Option<PairingRequest>,
    cancellation: &StepCancellation,
    progress: &mut dyn FnMut(StepResponse),
    pair: impl FnOnce(
        &PairingOperation,
        &mut dyn FnMut(PairingStage),
    ) -> Result<PairingOutcome, PairingError>,
) -> StepResponse {
    if cancellation.is_cancelled() {
        return StepResponse::Cancelled;
    }
    let before = inspect(path);
    if !matches!(before, StepResponse::InputRequired(_)) {
        return before;
    }
    let Some(request) = request else {
        return before;
    };
    let operation = PairingOperation::for_setup(request, cancellation.clone());
    progress(StepResponse::Running {
        message: "Connecting to the TV…",
        cancelable: cancellation.can_cancel(),
    });
    let result = pair(&operation, &mut |stage| {
        progress(StepResponse::Running {
            message: match stage {
                PairingStage::WaitingForConfirmation => "Approve LG Buddy on the TV to continue.",
                PairingStage::Verifying => "Verifying TV access…",
                PairingStage::Saving => "Saving the TV…",
                _ => "Connecting to the TV…",
            },
            cancelable: cancellation.can_cancel(),
        })
    });
    cancellation.finish();
    match result {
        Ok(_) => match inspect(path) {
            StepResponse::Complete => StepResponse::Complete,
            other => match other {
                StepResponse::Failed(_) => other,
                _ => failed(
                    "Pairing did not save a usable TV profile and credential.",
                    "pairing verification did not complete",
                ),
            },
        },
        Err(error) if error.failure() == PairingFailure::Cancelled => StepResponse::Cancelled,
        Err(error) => StepResponse::Failed(StepFailure {
            presentation: error.presentation(),
            diagnostic: format!("pairing: {:?}", error.failure()),
            retryable: true,
        }),
    }
}

fn failed(message: &str, diagnostic: impl ToString) -> StepResponse {
    StepResponse::Failed(StepFailure {
        presentation: UserFacingError::new("TV setup incomplete", message),
        diagnostic: diagnostic.to_string(),
        retryable: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::HdmiInput, platform_access_token::PlatformAccessToken};
    use std::{
        cell::Cell,
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    fn fixture() -> (std::path::PathBuf, PairingRequest) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir()
            .join(format!(
                "lg-buddy-setup-pair-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .join("config.env");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "screen_idle_blank=disabled\nsystem_sleep_wake_policy=enabled\n",
        )
        .unwrap();
        (
            path,
            PairingRequest::parse("192.0.2.42", "02:11:22:33:44:55", HdmiInput::Hdmi1).unwrap(),
        )
    }
    #[test]
    fn pairing_and_credential_repair_are_idempotent_and_preserve_preferences() {
        let (path, request) = fixture();
        let calls = Cell::new(0);
        for attempt in 0..3 {
            if attempt == 1 {
                fs::remove_file(path.parent().unwrap().join("tvs/primary/access-token.json"))
                    .unwrap();
            } else if attempt == 2 {
                fs::write(
                    path.parent().unwrap().join("tvs/primary/access-token.json"),
                    "broken",
                )
                .unwrap();
            }
            assert!(matches!(inspect(&path), StepResponse::InputRequired(_)));
            for _ in 0..2 {
                let response = execute_with(
                    &path,
                    Some(request),
                    &StepCancellation::default(),
                    &mut |_| {},
                    |op, progress| {
                        calls.set(calls.get() + 1);
                        crate::pairing::pair_and_save(op, &path, progress, |_| {
                            Ok(PlatformAccessToken::new("fixture-token").unwrap())
                        })
                    },
                );
                assert_eq!(response, StepResponse::Complete);
            }
            assert_eq!(calls.get(), attempt + 1);
            let config = fs::read_to_string(&path).unwrap();
            assert!(config.contains("screen_idle_blank=disabled\n"));
            assert!(config.contains("system_sleep_wake_policy=enabled\n"));
            assert_eq!(config.matches("tvs_primary_ip=").count(), 1);
        }
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn cancelled_pairing_does_not_publish_and_save_rejects_cancellation() {
        let (path, request) = fixture();
        let before = fs::read(&path).unwrap();
        let cancellation = StepCancellation::default();
        assert!(cancellation.cancel());
        assert_eq!(
            execute_with(
                &path,
                Some(request),
                &cancellation,
                &mut |_| {},
                |_, _| panic!("cancelled")
            ),
            StepResponse::Cancelled
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let cancellation = StepCancellation::default();
        assert_eq!(
            execute_with(
                &path,
                Some(request),
                &cancellation,
                &mut |response| {
                    if matches!(
                        response,
                        StepResponse::Running {
                            cancelable: false,
                            ..
                        }
                    ) {
                        assert!(!cancellation.cancel());
                    }
                },
                |op, progress| crate::pairing::pair_and_save(op, &path, progress, |_| Ok(
                    PlatformAccessToken::new("token").unwrap()
                ))
            ),
            StepResponse::Complete
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn missing_input_and_bad_config_never_initiate_pairing() {
        let (path, _) = fixture();
        assert!(matches!(
            execute_with(
                &path,
                None,
                &StepCancellation::default(),
                &mut |_| {},
                |_, _| panic!("needs input")
            ),
            StepResponse::InputRequired(_)
        ));
        fs::write(&path, "tvs_primary_ip=broken\n").unwrap();
        assert!(matches!(inspect(&path), StepResponse::Failed(_)));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn rejection_and_cancellation_while_pairing_preserve_desired_state() {
        let (path, request) = fixture();
        let original = fs::read(&path).unwrap();
        for failure in [PairingFailure::Rejected, PairingFailure::Cancelled] {
            let cancellation = StepCancellation::default();
            let result = execute_with(
                &path,
                Some(request),
                &cancellation,
                &mut |_| {},
                |op, progress| {
                    crate::pairing::pair_and_save(op, &path, progress, |progress| {
                        progress(PairingStage::WaitingForConfirmation);
                        if failure == PairingFailure::Cancelled {
                            assert!(cancellation.cancel());
                        }
                        Err(PairingError::new(failure))
                    })
                },
            );
            if failure == PairingFailure::Cancelled {
                assert_eq!(result, StepResponse::Cancelled);
            } else {
                assert!(matches!(result, StepResponse::Failed(_)));
            }
            assert_eq!(fs::read(&path).unwrap(), original);
            assert!(matches!(inspect(&path), StepResponse::InputRequired(_)));
        }
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
