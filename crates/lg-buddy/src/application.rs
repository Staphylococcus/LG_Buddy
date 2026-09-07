//! Toolkit-independent coordination between the desktop application's views.
//! Hosts execute the declared operations and return completions; cross-view
//! workflow decisions stay here alongside the individual application models.

use crate::audio::{AudioWriteError, AudioWriteOutcome};
use crate::brightness::{BrightnessReadError, BrightnessWriteError, BrightnessWriteOutcome};
use crate::overview::{
    AudioReadError, OverviewApplication, OverviewAudioReadOperation, OverviewAudioWriteOperation,
    OverviewBrightnessReadOperation, OverviewBrightnessWriteOperation, OverviewFrontendUpdate,
    OverviewIntent, OverviewSummaryError, OverviewSummaryOperation, OverviewTransition,
    OverviewTvIdentity,
};
use crate::pairing::{PairingError, PairingFailure, PairingOperation, PairingStage};
use crate::tv::{AudioStatus, OledBrightness};
use crate::tvs::{
    TvProfile, TvsApplication, TvsIntent, TvsModelReadOperation, TvsReadError, TvsReadOperation,
    TvsTransition,
};

pub enum OverviewCompletion {
    Summary(
        OverviewSummaryOperation,
        Result<OverviewTvIdentity, OverviewSummaryError>,
    ),
    BrightnessRead(
        OverviewBrightnessReadOperation,
        Result<OledBrightness, BrightnessReadError>,
    ),
    AudioRead(
        OverviewAudioReadOperation,
        Result<AudioStatus, AudioReadError>,
    ),
    BrightnessWrite(
        OverviewBrightnessWriteOperation,
        Result<BrightnessWriteOutcome, BrightnessWriteError>,
    ),
    AudioWrite(
        OverviewAudioWriteOperation,
        Result<AudioWriteOutcome, AudioWriteError>,
    ),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationTransition {
    overview: Option<OverviewTransition>,
    tvs: Option<TvsTransition>,
}

impl ApplicationTransition {
    pub fn overview(&self) -> Option<&OverviewTransition> {
        self.overview.as_ref()
    }

    pub fn tvs(&self) -> Option<&TvsTransition> {
        self.tvs.as_ref()
    }
}

pub struct Application {
    overview: OverviewApplication,
    tvs: TvsApplication,
}

impl Application {
    pub fn open() -> (Self, ApplicationTransition) {
        let (overview, overview_opening) = OverviewApplication::open();
        let (tvs, tvs_opening) = TvsApplication::open();
        (
            Self { overview, tvs },
            ApplicationTransition {
                overview: Some(overview_opening),
                tvs: Some(tvs_opening),
            },
        )
    }

    pub fn handle_overview_intent(
        &mut self,
        intent: OverviewIntent,
    ) -> Option<ApplicationTransition> {
        let transition = self.overview.handle_intent(intent)?;
        Some(self.overview_transition(transition))
    }

    pub fn handle_tvs_intent(&mut self, intent: TvsIntent) -> Option<ApplicationTransition> {
        let transition = self.tvs.handle_intent(intent)?;
        Some(self.tvs_transition(transition))
    }

    pub fn complete_overview(
        &mut self,
        completion: OverviewCompletion,
    ) -> Option<ApplicationTransition> {
        let transition = match completion {
            OverviewCompletion::Summary(operation, result) => {
                self.overview.complete_summary(operation, result)
            }
            OverviewCompletion::BrightnessRead(operation, result) => {
                self.overview.complete_brightness_read(operation, result)
            }
            OverviewCompletion::AudioRead(operation, result) => {
                self.overview.complete_audio_read(operation, result)
            }
            OverviewCompletion::BrightnessWrite(operation, result) => {
                self.overview.complete_brightness_write(operation, result)
            }
            OverviewCompletion::AudioWrite(operation, result) => {
                self.overview.complete_audio_write(operation, result)
            }
        }?;
        Some(self.overview_transition(transition))
    }

    pub fn complete_tvs_read(
        &mut self,
        operation: TvsReadOperation,
        result: Result<Vec<TvProfile>, TvsReadError>,
    ) -> Option<ApplicationTransition> {
        let transition = self.tvs.complete_read(operation, result)?;
        Some(self.tvs_transition(transition))
    }

    pub fn complete_tvs_model_read(
        &mut self,
        operation: TvsModelReadOperation,
        result: Result<String, TvsReadError>,
    ) -> Option<ApplicationTransition> {
        let transition = self.tvs.complete_model_read(operation, result)?;
        Some(self.tvs_transition(transition))
    }

    pub fn pairing_progress(
        &mut self,
        operation: &PairingOperation,
        stage: PairingStage,
    ) -> Option<ApplicationTransition> {
        let transition = self.tvs.pairing_progress(operation, stage)?;
        Some(self.tvs_transition(transition))
    }

    pub fn complete_pairing(
        &mut self,
        operation: &PairingOperation,
        result: Result<TvProfile, PairingError>,
    ) -> Option<ApplicationTransition> {
        let transition = self.tvs.complete_pairing(operation, result)?;
        Some(self.tvs_transition(transition))
    }

    pub fn pairing_worker_stopped(
        &mut self,
        operation: &PairingOperation,
    ) -> Option<ApplicationTransition> {
        self.complete_pairing(operation, Err(PairingError::new(PairingFailure::Internal)))
    }

    pub fn is_pairing(&self) -> bool {
        self.tvs.is_pairing()
    }

    pub fn shutdown(&mut self) {
        self.overview.shutdown();
        self.tvs.shutdown();
    }

    fn overview_transition(&mut self, transition: OverviewTransition) -> ApplicationTransition {
        if matches!(transition.update(), OverviewFrontendUpdate::Close) {
            self.tvs.shutdown();
        }
        ApplicationTransition {
            overview: Some(transition),
            tvs: None,
        }
    }

    fn tvs_transition(&mut self, transition: TvsTransition) -> ApplicationTransition {
        let overview = if transition.profile_created() {
            self.overview.profile_created()
        } else {
            None
        };
        ApplicationTransition {
            overview,
            tvs: Some(transition),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HdmiInput, TvPlatform};
    use crate::overview::{OverviewOperation, OverviewSummaryFailure};
    use crate::pairing::PairingIntent;
    use crate::tvs::{TvCredentialState, TvId};

    fn pairing() -> (Application, ApplicationTransition, PairingOperation) {
        let (mut application, opening) = Application::open();
        application
            .complete_tvs_read(opening.tvs().unwrap().read_operation().unwrap(), Ok(vec![]))
            .unwrap();
        for intent in [
            TvsIntent::PairTv,
            TvsIntent::Pairing(PairingIntent::SetAddress("192.0.2.10".into())),
            TvsIntent::Pairing(PairingIntent::SetMac("02:11:22:33:44:55".into())),
        ] {
            application.handle_tvs_intent(intent).unwrap();
        }
        let start = application
            .handle_tvs_intent(TvsIntent::Pairing(PairingIntent::Submit))
            .unwrap();
        let operation = start.tvs().unwrap().pairing_operation().unwrap().clone();
        (application, opening, operation)
    }

    fn profile(operation: &PairingOperation) -> TvProfile {
        let request = operation.request();
        TvProfile::new(
            TvId::primary(),
            "Primary TV",
            request.address(),
            request.mac(),
            HdmiInput::Hdmi1,
            TvPlatform::LgWebOs,
            TvCredentialState::Stored,
        )
    }

    #[test]
    fn pairing_success_refreshes_overview_and_rejects_previous_results() {
        let (mut application, opening, operation) = pairing();
        let completed = application
            .complete_pairing(&operation, Ok(profile(&operation)))
            .unwrap();
        let tvs = completed.tvs().unwrap();
        assert_eq!(tvs.presentation().profiles(), &[profile(&operation)]);
        assert!(tvs.presentation().pairing().is_none());
        assert_eq!(tvs.toast_message(), Some("TV paired successfully"));
        let refreshed = completed
            .overview()
            .expect("pairing must refresh Overview without a renderer");
        assert!(matches!(
            refreshed.operations(),
            [
                OverviewOperation::ReadSummary(_),
                OverviewOperation::ReadBrightness(_),
                OverviewOperation::ReadAudio(_)
            ]
        ));
        for old in opening.overview().unwrap().operations() {
            assert!(!refreshed.operations().contains(old));
            let result = match *old {
                OverviewOperation::ReadSummary(op) => OverviewCompletion::Summary(
                    op,
                    Err(OverviewSummaryError::new(
                        OverviewSummaryFailure::NotConfigured,
                        "old configuration",
                    )),
                ),
                OverviewOperation::ReadBrightness(op) => {
                    OverviewCompletion::BrightnessRead(op, Ok(OledBrightness::new(30).unwrap()))
                }
                OverviewOperation::ReadAudio(op) => OverviewCompletion::AudioRead(
                    op,
                    Ok(AudioStatus::new(crate::tv::CurrentVolume::Unknown, false)),
                ),
                _ => unreachable!(),
            };
            assert!(application.complete_overview(result).is_none());
        }
    }

    #[test]
    fn worker_loss_reports_an_internal_failure_without_refreshing_overview() {
        let (mut application, _, operation) = pairing();
        let failed = application.pairing_worker_stopped(&operation).unwrap();
        assert!(failed.overview().is_none());
        let tvs = failed.tvs().unwrap();
        assert_eq!(tvs.toast_message(), Some("Pairing stopped unexpectedly"));
        assert!(tvs.presentation().profiles().is_empty());
        let pairing = tvs.presentation().pairing().unwrap();
        assert_eq!(pairing.address(), "192.0.2.10");
        assert!(pairing
            .error()
            .unwrap()
            .detail()
            .contains("Restart LG Buddy"));
        assert!(!pairing.error().unwrap().detail().contains("IP address"));
        assert!(!pairing
            .description()
            .contains("No TV configuration was saved"));
        assert!(pairing.can_cancel());
        assert!(application
            .handle_tvs_intent(TvsIntent::Pairing(PairingIntent::Submit))
            .unwrap()
            .tvs()
            .unwrap()
            .pairing_operation()
            .is_some());
    }

    #[test]
    fn cancellation_and_application_close_reject_late_pairing_results() {
        let (mut application, _, operation) = pairing();
        let cancelled = application
            .handle_tvs_intent(TvsIntent::Pairing(PairingIntent::Cancel))
            .unwrap();
        assert!(cancelled.overview().is_none());
        assert!(cancelled.tvs().unwrap().presentation().pairing().is_none());
        assert!(operation.is_cancelled());
        assert!(application
            .complete_pairing(&operation, Ok(profile(&operation)))
            .is_none());
        assert!(application.pairing_worker_stopped(&operation).is_none());

        let (mut application, _, operation) = pairing();
        let closed = application
            .handle_overview_intent(OverviewIntent::Cancel)
            .unwrap();
        assert!(matches!(
            closed.overview().unwrap().update(),
            OverviewFrontendUpdate::Close
        ));
        assert!(operation.is_cancelled());
        assert!(!application.is_pairing());
        assert!(application
            .pairing_progress(&operation, PairingStage::Verifying)
            .is_none());
        assert!(application
            .complete_pairing(&operation, Ok(profile(&operation)))
            .is_none());
        assert!(application.handle_tvs_intent(TvsIntent::PairTv).is_none());
    }
}
