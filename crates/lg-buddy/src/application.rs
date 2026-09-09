//! Toolkit-independent coordination between the desktop application's views.
//! Hosts execute the declared operations and return completions; cross-view
//! workflow decisions stay here alongside the individual application models.

use crate::audio::{AudioWriteError, AudioWriteOutcome};
use crate::brightness::{BrightnessReadError, BrightnessWriteError, BrightnessWriteOutcome};
use crate::navigation::Navigation;
use crate::overview::{
    AudioReadError, OverviewApplication, OverviewAudioReadOperation, OverviewAudioWriteOperation,
    OverviewBrightnessReadOperation, OverviewBrightnessWriteOperation, OverviewFrontendUpdate,
    OverviewIntent, OverviewSummaryError, OverviewSummaryOperation, OverviewTransition,
    OverviewTvIdentity,
};
use crate::pairing::{PairingError, PairingFailure, PairingOperation, PairingStage};
use crate::presentation::settings::SettingsGroup;
use crate::settings::{SettingsMutationFailure, SettingsMutationOutcome};
use crate::settings_view::{
    SettingsApplication, SettingsIntent, SettingsMutationOperation, SettingsReadError,
    SettingsReadOperation, SettingsTransition,
};
use crate::setup::{SetupApplication, SetupError, SetupOperation, SetupOutcome, SetupPresentation};
use crate::tv::{AudioStatus, OledBrightness};
use crate::tvs::{
    TvProfile, TvsApplication, TvsIntent, TvsManagementError, TvsManagementOperation,
    TvsManagementOutcome, TvsModelReadOperation, TvsReadError, TvsReadOperation, TvsTransition,
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
    settings: Option<SettingsTransition>,
    navigation: Navigation,
    setup: SetupPresentation,
    setup_operation: Option<SetupOperation>,
}

impl ApplicationTransition {
    pub fn navigation(&self) -> &Navigation {
        &self.navigation
    }

    pub fn setup(&self) -> &SetupPresentation {
        &self.setup
    }

    pub fn setup_operation(&self) -> Option<&SetupOperation> {
        self.setup_operation.as_ref()
    }
    pub fn overview(&self) -> Option<&OverviewTransition> {
        self.overview.as_ref()
    }

    pub fn tvs(&self) -> Option<&TvsTransition> {
        self.tvs.as_ref()
    }

    pub fn settings(&self) -> Option<&SettingsTransition> {
        self.settings.as_ref()
    }
}

pub struct Application {
    overview: OverviewApplication,
    tvs: TvsApplication,
    settings: SettingsApplication,
    navigation: Navigation,
    setup: SetupApplication,
    closed: bool,
}

impl Application {
    pub fn open() -> (Self, ApplicationTransition) {
        let (overview, overview_opening) = OverviewApplication::open();
        let (tvs, tvs_opening) = TvsApplication::open();
        let (settings, settings_opening) = SettingsApplication::open();
        let (setup, setup_operation) = SetupApplication::open();
        (
            Self {
                overview,
                tvs,
                settings,
                navigation: Navigation::default(),
                setup,
                closed: false,
            },
            ApplicationTransition {
                overview: Some(overview_opening),
                tvs: Some(tvs_opening),
                settings: Some(settings_opening),
                navigation: Navigation::default(),
                setup: SetupPresentation::Idle,
                setup_operation: Some(setup_operation),
            },
        )
    }

    pub fn handle_overview_intent(
        &mut self,
        intent: OverviewIntent,
    ) -> Option<ApplicationTransition> {
        if self.setup.presentation().busy() && intent != OverviewIntent::Cancel {
            return None;
        }
        if intent == OverviewIntent::Cancel && !self.settings.can_close() {
            return None;
        }
        if self.tvs.is_managing() && intent != OverviewIntent::Cancel {
            return None;
        }
        let transition = self.overview.handle_intent(intent)?;
        Some(self.overview_transition(transition))
    }

    pub fn handle_tvs_intent(&mut self, intent: TvsIntent) -> Option<ApplicationTransition> {
        if self.setup.presentation().busy() {
            return None;
        }
        let transition = self.tvs.handle_intent(intent)?;
        Some(self.tvs_transition(transition))
    }

    pub fn handle_settings_intent(
        &mut self,
        intent: SettingsIntent,
    ) -> Option<ApplicationTransition> {
        if self.setup.presentation().busy() && intent != SettingsIntent::Refresh {
            return None;
        }
        let transition = self.settings.handle_intent(intent)?;
        Some(self.settings_transition(transition))
    }

    pub fn select_page(
        &mut self,
        page: crate::navigation::ApplicationPage,
    ) -> Option<ApplicationTransition> {
        if self.closed || !self.navigation.select(page) {
            return None;
        }
        match page {
            crate::navigation::ApplicationPage::Settings => self
                .handle_settings_intent(SettingsIntent::Refresh)
                .or_else(|| Some(self.transition(None, None, None, None))),
            _ => Some(self.transition(None, None, None, None)),
        }
    }

    pub fn complete_settings_read(
        &mut self,
        operation: SettingsReadOperation,
        result: Result<Vec<SettingsGroup>, SettingsReadError>,
    ) -> Option<ApplicationTransition> {
        let transition = self.settings.complete_read(operation, result)?;
        Some(self.settings_transition(transition))
    }

    pub fn complete_settings_mutation(
        &mut self,
        operation: &SettingsMutationOperation,
        result: Result<SettingsMutationOutcome, SettingsMutationFailure>,
    ) -> Option<ApplicationTransition> {
        let transition = self.settings.complete_mutation(operation, result)?;
        Some(self.settings_transition(transition))
    }

    pub fn settings_mutation_worker_stopped(
        &mut self,
        operation: &SettingsMutationOperation,
    ) -> Option<ApplicationTransition> {
        let transition = self.settings.mutation_worker_stopped(operation)?;
        Some(self.settings_transition(transition))
    }

    pub fn complete_update_check(
        &mut self,
        operation: crate::settings_view::UpdateCheckOperation,
        result: Result<
            crate::presentation::update_check::UpdateCheckReport,
            crate::settings_view::UpdateCheckError,
        >,
    ) -> Option<ApplicationTransition> {
        let transition = self.settings.complete_update_check(operation, result)?;
        Some(self.settings_transition(transition))
    }

    pub fn update_install_progress(
        &mut self,
        operation: &crate::update_flow::UpdateInstallOperation,
        stage: crate::update_install::UpdateInstallStage,
    ) -> Option<ApplicationTransition> {
        let transition = self.settings.update_install_progress(operation, stage)?;
        Some(self.settings_transition(transition))
    }

    pub fn complete_update_install(
        &mut self,
        operation: &crate::update_flow::UpdateInstallOperation,
        result: Result<
            crate::update_flow::UpdateInstallOutcome,
            crate::update_flow::UpdateInstallFailure,
        >,
    ) -> Option<ApplicationTransition> {
        let transition = self.settings.complete_update_install(operation, result)?;
        Some(self.settings_transition(transition))
    }

    fn settings_transition(&mut self, transition: SettingsTransition) -> ApplicationTransition {
        self.transition(None, None, Some(transition), None)
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

    pub fn complete_tvs_management(
        &mut self,
        operation: &TvsManagementOperation,
        result: Result<TvsManagementOutcome, TvsManagementError>,
    ) -> Option<ApplicationTransition> {
        let transition = self.tvs.complete_management(operation, result)?;
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
        let mut transition = self.tvs.complete_pairing(operation, result)?;
        if transition.profile_changed() {
            self.setup.paired();
            // Pairing is durable, but setup is not complete until activation succeeds.
            transition.clear_toast();
        }
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
        self.closed = true;
        self.overview.shutdown();
        self.tvs.shutdown();
        self.settings.shutdown();
    }

    fn overview_transition(&mut self, transition: OverviewTransition) -> ApplicationTransition {
        if matches!(transition.update(), OverviewFrontendUpdate::Close) {
            self.closed = true;
            self.tvs.shutdown();
            self.settings.shutdown();
        }
        self.transition(Some(transition), None, None, None)
    }

    fn tvs_transition(&mut self, transition: TvsTransition) -> ApplicationTransition {
        self.navigation
            .update_profiles(transition.presentation().status());
        if transition.profile_changed() && transition.presentation().profiles().is_empty() {
            self.setup.unpaired();
        }
        let overview = if transition.management_operation().is_some() {
            self.overview.profile_change_started()
        } else if transition.profile_changed() {
            self.overview.profile_changed()
        } else {
            None
        };
        self.transition(overview, Some(transition), None, None)
    }

    pub fn complete_setup(
        &mut self,
        operation: &SetupOperation,
        result: Result<SetupOutcome, SetupError>,
    ) -> Option<ApplicationTransition> {
        if self.closed || !self.setup.complete(operation, result) {
            return None;
        }
        Some(self.transition(None, None, None, None))
    }

    pub fn retry_setup(&mut self) -> Option<ApplicationTransition> {
        if self.closed
            || self.tvs.is_managing()
            || self.tvs.is_pairing()
            || self.settings.is_mutating()
            || self.overview.has_pending_write()
        {
            return None;
        }
        let has_profile = !self.tvs.presentation().profiles().is_empty();
        let operation = self.setup.retry(has_profile)?;
        Some(self.transition(None, None, None, Some(operation)))
    }

    fn transition(
        &mut self,
        overview: Option<OverviewTransition>,
        mut tvs: Option<TvsTransition>,
        mut settings: Option<SettingsTransition>,
        setup_operation: Option<SetupOperation>,
    ) -> ApplicationTransition {
        let setup_operation = setup_operation.or_else(|| {
            if self.closed
                || self.tvs.is_managing()
                || self.tvs.is_pairing()
                || self.settings.is_mutating()
                || self.overview.has_pending_write()
            {
                return None;
            }
            self.setup
                .start_if_needed(!self.tvs.presentation().profiles().is_empty())
        });
        let activating = self.setup.presentation().busy();
        if let Some(update) = self.tvs.set_controls_available(
            !activating && !self.overview.has_pending_write() && !self.settings.is_mutating(),
        ) {
            if let Some(tvs) = &mut tvs {
                tvs.update_presentation_from(update);
            } else {
                tvs = Some(update);
            }
        }
        if let Some(update) = self.settings.set_controls_available(
            !activating && !self.tvs.is_managing() && !self.tvs.is_pairing(),
        ) {
            if let Some(settings) = &mut settings {
                settings.update_presentation_from(update);
            } else {
                settings = Some(update);
            }
        }
        ApplicationTransition {
            overview,
            tvs,
            settings,
            navigation: self.navigation.clone(),
            setup: self.setup.presentation().clone(),
            setup_operation,
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
        assert!(tvs.toast_message().is_none());
        assert!(completed.setup().busy());
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
    fn empty_profile_hides_navigation_and_pairing_reveals_normal_destinations() {
        use crate::navigation::ApplicationPage;
        let (mut app, opening, operation) = pairing();
        assert!(!opening.navigation().tabs_visible());
        assert_eq!(opening.navigation().selected(), ApplicationPage::Tvs);
        assert!(app.select_page(ApplicationPage::Settings).is_none());
        assert!(app.select_page(ApplicationPage::Overview).is_none());
        let paired = app
            .complete_pairing(&operation, Ok(profile(&operation)))
            .unwrap();
        assert!(paired.navigation().tabs_visible());
        assert_eq!(paired.navigation().selected(), ApplicationPage::Overview);
        assert!(paired.setup().busy());
        assert!(paired.tvs().unwrap().toast_message().is_none());
        let activated = app
            .complete_setup(
                paired.setup_operation().unwrap(),
                Ok(SetupOutcome::Activated),
            )
            .unwrap();
        assert_eq!(activated.setup(), &SetupPresentation::Idle);
        app.handle_tvs_intent(TvsIntent::UnpairTv).unwrap();
        let unpair = app.handle_tvs_intent(TvsIntent::ConfirmUnpair).unwrap();
        let removed = app
            .complete_tvs_management(
                unpair.tvs().unwrap().management_operation().unwrap(),
                Ok(TvsManagementOutcome::Unpaired),
            )
            .unwrap();
        assert!(!removed.navigation().tabs_visible());
        assert_eq!(removed.navigation().selected(), ApplicationPage::Tvs);
        assert!(removed
            .tvs()
            .unwrap()
            .presentation()
            .pair_action()
            .is_some());
    }

    #[test]
    fn pending_activation_resumes_after_profile_and_marker_reads_in_either_order() {
        let (_, _, pairing_operation) = pairing();
        for profiles_first in [true, false] {
            let (mut app, opening) = Application::open();
            let first;
            let second;
            if profiles_first {
                first = app
                    .complete_tvs_read(
                        opening.tvs().unwrap().read_operation().unwrap(),
                        Ok(vec![profile(&pairing_operation)]),
                    )
                    .unwrap();
                second = app
                    .complete_setup(
                        opening.setup_operation().unwrap(),
                        Ok(SetupOutcome::Inspected { pending: true }),
                    )
                    .unwrap();
            } else {
                first = app
                    .complete_setup(
                        opening.setup_operation().unwrap(),
                        Ok(SetupOutcome::Inspected { pending: true }),
                    )
                    .unwrap();
                second = app
                    .complete_tvs_read(
                        opening.tvs().unwrap().read_operation().unwrap(),
                        Ok(vec![profile(&pairing_operation)]),
                    )
                    .unwrap();
            }
            assert!(first.setup_operation().is_none());
            assert_eq!(
                second.setup_operation().unwrap().task(),
                crate::setup::SetupTask::Activate
            );
            assert!(second.setup().busy());
            assert!(app.retry_setup().is_none());
            assert!(app.handle_tvs_intent(TvsIntent::UnpairTv).is_none());
            assert!(app
                .handle_settings_intent(SettingsIntent::CheckForUpdates)
                .is_none());
        }
    }

    #[test]
    fn configured_offline_tv_keeps_navigation_without_reactivating_existing_installation() {
        let (_, _, pairing_operation) = pairing();
        let (mut app, opening) = Application::open();
        app.complete_setup(
            opening.setup_operation().unwrap(),
            Ok(SetupOutcome::Inspected { pending: false }),
        )
        .unwrap();
        let loaded = app
            .complete_tvs_read(
                opening.tvs().unwrap().read_operation().unwrap(),
                Ok(vec![profile(&pairing_operation)]),
            )
            .unwrap();
        assert!(loaded.navigation().tabs_visible());
        assert_eq!(
            loaded.navigation().selected(),
            crate::navigation::ApplicationPage::Overview
        );
        assert!(loaded.setup_operation().is_none());
        let offline = app
            .complete_tvs_model_read(
                loaded
                    .tvs()
                    .unwrap()
                    .model_read_operation()
                    .unwrap()
                    .clone(),
                Err(TvsReadError::internal("offline")),
            )
            .unwrap();
        assert!(offline.navigation().tabs_visible());
        assert_eq!(offline.tvs().unwrap().presentation().profiles().len(), 1);
        assert!(offline.setup_operation().is_none());
        assert_eq!(offline.setup(), &SetupPresentation::Idle);
    }

    #[test]
    fn activation_failure_retries_without_pairing_and_rejects_stale_or_closed_completions() {
        let (mut app, opening, operation) = pairing();
        let paired = app
            .complete_pairing(&operation, Ok(profile(&operation)))
            .unwrap();
        let activation = *paired.setup_operation().unwrap();
        // A late initial marker read cannot erase the new pairing's activation intent.
        assert!(app
            .complete_setup(
                opening.setup_operation().unwrap(),
                Ok(SetupOutcome::Inspected { pending: false })
            )
            .is_none());
        let failed = app
            .complete_setup(&activation, Err(SetupError::stopped()))
            .unwrap();
        assert!(failed.setup().retry_available());
        assert_eq!(
            failed.tvs().unwrap().presentation().profiles(),
            &[profile(&operation)]
        );
        let retry = app.retry_setup().unwrap();
        assert!(retry.tvs().unwrap().pairing_operation().is_none());
        assert_ne!(retry.setup_operation().unwrap(), &activation);
        assert!(app
            .complete_setup(&activation, Ok(SetupOutcome::Activated))
            .is_none());
        app.shutdown();
        assert!(app
            .complete_setup(
                retry.setup_operation().unwrap(),
                Ok(SetupOutcome::Activated)
            )
            .is_none());
    }

    #[test]
    fn unknown_configuration_keeps_settings_reachable_without_presenting_pairing() {
        let (mut app, opening) = Application::open();
        let failed = app
            .complete_tvs_read(
                opening.tvs().unwrap().read_operation().unwrap(),
                Err(TvsReadError::internal("cannot read configuration")),
            )
            .unwrap();
        assert!(failed.navigation().tabs_visible());
        assert!(failed.tvs().unwrap().presentation().pair_action().is_none());
        assert!(app
            .select_page(crate::navigation::ApplicationPage::Settings)
            .is_some());
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

#[cfg(test)]
mod management_tests {
    use super::*;
    use crate::brightness::BrightnessWriteOutcome;
    use crate::config::{HdmiInput, TvPlatform};
    use crate::overview::{OverviewIntent, OverviewOperation};
    use crate::tvs::{TvCredentialState, TvId};

    fn configured() -> (Application, ApplicationTransition) {
        let (mut app, opening) = Application::open();
        let profile = TvProfile::new(
            TvId::primary(),
            "TV",
            "192.0.2.10".parse().unwrap(),
            "02:11:22:33:44:55".parse().unwrap(),
            HdmiInput::Hdmi1,
            TvPlatform::LgWebOs,
            TvCredentialState::Stored,
        );
        app.complete_tvs_read(
            opening.tvs().unwrap().read_operation().unwrap(),
            Ok(vec![profile]),
        )
        .unwrap();
        (app, opening)
    }

    #[test]
    fn profile_change_invalidates_overview_reads_and_blocks_new_writes_until_reload() {
        let (mut app, opening) = configured();
        let change = app
            .handle_tvs_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
            .unwrap();
        assert!(change.overview().unwrap().operations().is_empty());
        assert!(app
            .handle_overview_intent(OverviewIntent::SetBrightness(50))
            .is_none());
        for operation in opening.overview().unwrap().operations() {
            if let OverviewOperation::ReadBrightness(read) = operation {
                assert!(app
                    .complete_overview(OverviewCompletion::BrightnessRead(
                        *read,
                        Ok(OledBrightness::new(50).unwrap())
                    ))
                    .is_none());
            }
        }
        let done = app
            .complete_tvs_management(
                change.tvs().unwrap().management_operation().unwrap(),
                Err(TvsManagementError::stopped()),
            )
            .unwrap();
        assert_eq!(done.overview().unwrap().operations().len(), 3);
        assert!(done.tvs().unwrap().presentation().input_enabled());
    }

    #[test]
    fn active_overview_write_disables_profile_changes_until_it_finishes() {
        let (mut app, opening) = configured();
        for operation in opening.overview().unwrap().operations() {
            if let OverviewOperation::ReadBrightness(read) = operation {
                app.complete_overview(OverviewCompletion::BrightnessRead(
                    *read,
                    Ok(OledBrightness::new(50).unwrap()),
                ))
                .unwrap();
            }
        }
        let writing = app
            .handle_overview_intent(OverviewIntent::SetBrightness(60))
            .unwrap();
        assert!(!writing.tvs().unwrap().presentation().input_enabled());
        assert!(!writing
            .tvs()
            .unwrap()
            .presentation()
            .unpair_action()
            .unwrap()
            .enabled());
        assert!(app
            .handle_tvs_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
            .is_none());
        assert!(app.handle_tvs_intent(TvsIntent::UnpairTv).is_none());
        let operation = writing
            .overview()
            .unwrap()
            .operations()
            .iter()
            .find_map(|op| match op {
                OverviewOperation::WriteBrightness(write) => Some(*write),
                _ => None,
            })
            .unwrap();
        let done = app
            .complete_overview(OverviewCompletion::BrightnessWrite(
                operation,
                Ok(BrightnessWriteOutcome::applied()),
            ))
            .unwrap();
        assert!(done.tvs().unwrap().presentation().input_enabled());
        assert!(app.handle_tvs_intent(TvsIntent::UnpairTv).is_some());
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::pairing::PairingIntent;
    use crate::settings::{ConfigEnvReader, SettingsError};
    use crate::settings_view::BehaviorSetting;

    fn settings() -> Vec<SettingsGroup> {
        crate::presentation::settings::SettingsPresentation::from_store(
            &ConfigEnvReader::parse("/unused/config.env", "").into_store(),
        )
        .groups()
        .to_vec()
    }

    fn editable(transition: &ApplicationTransition) -> bool {
        transition
            .settings()
            .unwrap()
            .presentation()
            .groups()
            .iter()
            .flat_map(|group| group.rows())
            .all(|row| row.editor_enabled())
    }

    #[test]
    fn settings_write_blocks_pairing_and_close_rejects_late_completions() {
        let (mut app, opening) = Application::open();
        app.complete_tvs_read(opening.tvs().unwrap().read_operation().unwrap(), Ok(vec![]))
            .unwrap();
        app.complete_settings_read(
            opening.settings().unwrap().read_operation().unwrap(),
            Ok(settings()),
        )
        .unwrap();
        let write = app
            .handle_settings_intent(SettingsIntent::SetEnabled {
                setting: BehaviorSetting::ScreenIdleBlank,
                enabled: false,
            })
            .unwrap();
        assert!(!write
            .tvs()
            .unwrap()
            .presentation()
            .pair_action()
            .unwrap()
            .enabled());
        assert!(app.handle_tvs_intent(TvsIntent::PairTv).is_none());
        assert!(editable(&write));
        let operation = write.settings().unwrap().mutation_operation().unwrap();
        let done = app
            .complete_settings_mutation(
                operation,
                Err(SettingsMutationFailure::Persistence(SettingsError::Apply {
                    message: "test persistence failure".into(),
                })),
            )
            .unwrap();
        assert!(done
            .tvs()
            .unwrap()
            .presentation()
            .pair_action()
            .unwrap()
            .enabled());
        assert!(editable(&done));
        let write = app
            .handle_settings_intent(SettingsIntent::SetEnabled {
                setting: BehaviorSetting::ScreenIdleBlank,
                enabled: false,
            })
            .unwrap();
        let operation = write.settings().unwrap().mutation_operation().unwrap();
        let close = app.handle_overview_intent(OverviewIntent::Cancel).unwrap();
        assert!(matches!(
            close.overview().unwrap().update(),
            OverviewFrontendUpdate::Close
        ));
        assert!(app.settings_mutation_worker_stopped(operation).is_none());
        assert!(app
            .handle_settings_intent(SettingsIntent::Reset(BehaviorSetting::UpdatesChannel))
            .is_none());
    }

    #[test]
    fn pairing_blocks_settings_even_if_the_settings_read_finishes_later() {
        let (mut app, opening) = Application::open();
        app.complete_tvs_read(opening.tvs().unwrap().read_operation().unwrap(), Ok(vec![]))
            .unwrap();
        app.handle_tvs_intent(TvsIntent::PairTv).unwrap();
        let loaded = app
            .complete_settings_read(
                opening.settings().unwrap().read_operation().unwrap(),
                Ok(settings()),
            )
            .unwrap();
        assert!(!editable(&loaded));
        assert!(app
            .handle_settings_intent(SettingsIntent::Reset(BehaviorSetting::UpdatesChannel))
            .is_none());
        let cancelled = app
            .handle_tvs_intent(TvsIntent::Pairing(PairingIntent::Cancel))
            .unwrap();
        assert!(editable(&cancelled));
        assert!(app
            .handle_settings_intent(SettingsIntent::Reset(BehaviorSetting::UpdatesChannel))
            .is_some());
    }
}
