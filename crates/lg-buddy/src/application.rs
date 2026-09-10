//! Toolkit-independent coordination between the desktop application's views.
//! Hosts execute the declared operations and return completions; cross-view
//! workflow decisions stay here alongside the individual application models.

use crate::audio::{AudioWriteError, AudioWriteOutcome};
use crate::brightness::{BrightnessReadError, BrightnessWriteError, BrightnessWriteOutcome};
use crate::diagnostics::DiagnosticsReport;
use crate::diagnostics_view::{
    DiagnosticsApplication, DiagnosticsError, DiagnosticsIntent, DiagnosticsReadOperation,
    DiagnosticsSaveOperation, DiagnosticsTransition,
};
use crate::navigation::Navigation;
use crate::overview::{
    AudioReadError, OverviewApplication, OverviewAudioReadOperation, OverviewAudioWriteOperation,
    OverviewBrightnessReadOperation, OverviewBrightnessWriteOperation, OverviewFrontendUpdate,
    OverviewIntent, OverviewSummaryError, OverviewSummaryOperation, OverviewTransition,
    OverviewTvIdentity,
};
use crate::pairing::{
    PairingError, PairingFailure, PairingOperation, PairingOutcome, PairingStage,
};
use crate::presentation::settings::SettingsGroup;
use crate::settings::{SettingsMutationFailure, SettingsMutationOutcome};
use crate::settings_view::{
    BehaviorSetting, SettingsApplication, SettingsIntent, SettingsMutationOperation,
    SettingsReadError, SettingsReadOperation, SettingsTransition,
};
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
    diagnostics: Option<DiagnosticsTransition>,
    navigation: Navigation,
}

impl ApplicationTransition {
    pub fn navigation(&self) -> &Navigation {
        &self.navigation
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

    pub fn diagnostics(&self) -> Option<&DiagnosticsTransition> {
        self.diagnostics.as_ref()
    }
}

pub struct Application {
    overview: OverviewApplication,
    tvs: TvsApplication,
    settings: SettingsApplication,
    diagnostics: DiagnosticsApplication,
    navigation: Navigation,
    pairing_behaviors: Vec<BehaviorSetting>,
    pairing_settings_read: Option<SettingsReadOperation>,
    closed: bool,
}

impl Application {
    pub fn open() -> (Self, ApplicationTransition) {
        let (overview, overview_opening) = OverviewApplication::open();
        let (tvs, tvs_opening) = TvsApplication::open();
        let (settings, settings_opening) = SettingsApplication::open();
        (
            Self {
                overview,
                tvs,
                settings,
                diagnostics: DiagnosticsApplication::default(),
                navigation: Navigation::default(),
                pairing_behaviors: Vec::new(),
                pairing_settings_read: None,
                closed: false,
            },
            ApplicationTransition {
                overview: Some(overview_opening),
                tvs: Some(tvs_opening),
                settings: Some(settings_opening),
                diagnostics: None,
                navigation: Navigation::default(),
            },
        )
    }

    pub fn handle_overview_intent(
        &mut self,
        intent: OverviewIntent,
    ) -> Option<ApplicationTransition> {
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
        if self.pairing_settings_read.is_some() {
            return None;
        }
        let transition = self.tvs.handle_intent(intent)?;
        Some(self.tvs_transition(transition))
    }

    pub fn handle_settings_intent(
        &mut self,
        intent: SettingsIntent,
    ) -> Option<ApplicationTransition> {
        if self.pairing_settings_read.is_some()
            || (!self.pairing_behaviors.is_empty()
                && (self.tvs.is_managing() || self.tvs.is_pairing()))
        {
            return None;
        }
        let transition = self.settings.handle_intent(intent)?;
        if !self.pairing_behaviors.is_empty() && transition.read_operation().is_some() {
            self.pairing_settings_read = transition.read_operation();
        }
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
                .or_else(|| Some(self.transition(None, None, None))),
            _ => Some(self.transition(None, None, None)),
        }
    }

    pub fn handle_diagnostics_intent(
        &mut self,
        intent: DiagnosticsIntent,
    ) -> Option<ApplicationTransition> {
        if self.closed {
            return None;
        }
        if matches!(intent, DiagnosticsIntent::Open | DiagnosticsIntent::Refresh) {
            if let Some(details) = self
                .settings
                .presentation()
                .update_install()
                .failure_details()
            {
                self.diagnostics
                    .record_failure("Last update failure", details);
            }
        }
        let transition = self.diagnostics.handle_intent(intent)?;
        Some(self.diagnostics_transition(transition))
    }

    pub fn complete_diagnostics_read(
        &mut self,
        operation: &DiagnosticsReadOperation,
        result: Result<DiagnosticsReport, DiagnosticsError>,
    ) -> Option<ApplicationTransition> {
        let transition = self.diagnostics.complete_read(operation, result)?;
        Some(self.diagnostics_transition(transition))
    }

    pub fn complete_diagnostics_save(
        &mut self,
        operation: &DiagnosticsSaveOperation,
        result: Result<(), DiagnosticsError>,
    ) -> Option<ApplicationTransition> {
        let transition = self.diagnostics.complete_save(operation, result)?;
        Some(self.diagnostics_transition(transition))
    }

    fn diagnostics_transition(
        &mut self,
        diagnostics: DiagnosticsTransition,
    ) -> ApplicationTransition {
        let mut transition = self.transition(None, None, None);
        transition.diagnostics = Some(diagnostics);
        transition
    }

    pub fn complete_settings_read(
        &mut self,
        operation: SettingsReadOperation,
        result: Result<Vec<SettingsGroup>, SettingsReadError>,
    ) -> Option<ApplicationTransition> {
        let succeeded = result.is_ok();
        let mut transition = self.settings.complete_read(operation, result)?;
        if self.pairing_settings_read == Some(operation) {
            self.pairing_settings_read = None;
            if let Some(update) = self.settings.set_controls_available(true) {
                transition.update_presentation_from(update);
            }
            if succeeded && !self.closed {
                let requested = std::mem::take(&mut self.pairing_behaviors);
                for setting in requested {
                    if let Some(update) = self.settings.handle_intent(SettingsIntent::SetEnabled {
                        setting,
                        enabled: true,
                    }) {
                        if transition.mutation_operation().is_none() {
                            transition = update;
                        } else {
                            transition.update_presentation_from(update);
                        }
                    }
                }
            }
        }
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
        if let crate::presentation::settings::SettingsStatus::Failed(error) =
            transition.presentation().status()
        {
            self.diagnostics.record_failure("Settings", error.summary());
        }
        for row in transition
            .presentation()
            .groups()
            .iter()
            .flat_map(|group| group.rows())
        {
            if let Some(feedback) = row.feedback() {
                self.diagnostics
                    .record_failure(row.title(), feedback.message());
            }
        }
        if transition.diagnostic().is_some() {
            if let Some(notice) = transition.update_notice() {
                self.diagnostics.record_failure("Updates", notice.title());
            }
        }
        self.transition(None, None, Some(transition))
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
        result: Result<PairingOutcome, PairingError>,
    ) -> Option<ApplicationTransition> {
        let requested = result
            .as_ref()
            .map(|outcome| outcome.requested_behaviors().to_vec())
            .unwrap_or_default();
        let mut transition = self
            .tvs
            .complete_pairing(operation, result.map(PairingOutcome::into_profile))?;
        let paired = transition.profile_changed();
        if paired {
            transition.clear_toast();
        }
        let mut update = self.tvs_transition(transition);
        if paired {
            self.pairing_behaviors = requested;
            let settings = self.settings.profile_changed();
            self.pairing_settings_read = (!self.pairing_behaviors.is_empty())
                .then(|| settings.read_operation())
                .flatten();
            update = self.transition(update.overview, update.tvs, Some(settings));
        }
        Some(update)
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
        self.diagnostics.shutdown();
    }

    fn overview_transition(&mut self, transition: OverviewTransition) -> ApplicationTransition {
        if transition.diagnostic().is_some() {
            if let OverviewFrontendUpdate::Present(presentation) = transition.update() {
                use crate::presentation::brightness::BrightnessStatus;
                use crate::presentation::overview::{AudioStatus, TvSummaryStatus};
                for (context, error) in [
                    (
                        "TV connection",
                        match presentation.summary().status() {
                            TvSummaryStatus::Failed(error) => Some(error),
                            _ => None,
                        },
                    ),
                    (
                        "Brightness",
                        match presentation.brightness().status() {
                            BrightnessStatus::Failed(error) => Some(error),
                            _ => None,
                        },
                    ),
                    (
                        "Audio",
                        match presentation.audio().status() {
                            AudioStatus::Failed(error) => Some(error),
                            _ => None,
                        },
                    ),
                ] {
                    if let Some(error) = error {
                        self.diagnostics.record_failure(
                            context,
                            &format!("{} {}", error.summary(), error.detail()),
                        );
                    }
                }
            }
        }
        if matches!(transition.update(), OverviewFrontendUpdate::Close) {
            self.closed = true;
            self.tvs.shutdown();
            self.settings.shutdown();
            self.diagnostics.shutdown();
        }
        self.transition(Some(transition), None, None)
    }

    fn tvs_transition(&mut self, transition: TvsTransition) -> ApplicationTransition {
        if let crate::presentation::tvs::TvsStatus::Failed(error) =
            transition.presentation().status()
        {
            self.diagnostics
                .record_failure("TV profiles", error.summary());
        }
        if let Some(error) = transition
            .presentation()
            .pairing()
            .and_then(|pairing| pairing.error())
        {
            self.diagnostics.record_failure(
                "Pairing",
                &format!("{} {}", error.summary(), error.detail()),
            );
        }
        if let Some(error) = transition.presentation().management_error() {
            self.diagnostics.record_failure(
                "TV settings",
                &format!("{} {}", error.summary(), error.detail()),
            );
        }
        if transition.profile_changed() && transition.presentation().profiles().is_empty() {
            self.pairing_behaviors.clear();
            self.pairing_settings_read = None;
        }
        self.navigation
            .update_profiles(transition.presentation().status());
        let overview = if transition.management_operation().is_some() {
            self.overview.profile_change_started()
        } else if transition.profile_changed() {
            self.overview.profile_changed()
        } else {
            None
        };
        self.transition(overview, Some(transition), None)
    }

    fn transition(
        &mut self,
        overview: Option<OverviewTransition>,
        mut tvs: Option<TvsTransition>,
        mut settings: Option<SettingsTransition>,
    ) -> ApplicationTransition {
        if let Some(update) = self.tvs.set_controls_available(
            !self.overview.has_pending_write()
                && !self.settings.is_mutating()
                && self.pairing_settings_read.is_none(),
        ) {
            if let Some(tvs) = &mut tvs {
                tvs.update_presentation_from(update);
            } else {
                tvs = Some(update);
            }
        }
        if let Some(update) = self.settings.set_controls_available(
            !self.tvs.is_managing()
                && !self.tvs.is_pairing()
                && self.pairing_settings_read.is_none(),
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
            diagnostics: None,
            navigation: self.navigation.clone(),
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

    #[test]
    fn diagnostics_is_available_before_pairing_and_does_not_change_navigation() {
        let (mut app, opening) = Application::open();
        assert!(opening.diagnostics().is_none());
        let diagnostics = app
            .handle_diagnostics_intent(DiagnosticsIntent::Open)
            .unwrap();
        assert_eq!(diagnostics.navigation(), opening.navigation());
        assert!(!diagnostics.navigation().tabs_visible());
        assert!(diagnostics.overview().is_none());
        assert!(diagnostics.settings().is_none());
        assert!(diagnostics.tvs().is_none());
        let operation = diagnostics.diagnostics().unwrap().read_operation().unwrap();
        app.shutdown();
        assert!(app
            .complete_diagnostics_read(operation, Ok(DiagnosticsReport::new(0, vec![])))
            .is_none());
        assert!(app
            .handle_diagnostics_intent(DiagnosticsIntent::Open)
            .is_none());
    }

    #[test]
    fn diagnostics_retains_safe_failure_summaries_without_raw_worker_output() {
        let (mut app, opening) = Application::open();
        let operation = opening
            .overview()
            .unwrap()
            .operations()
            .iter()
            .find_map(|operation| match operation {
                OverviewOperation::ReadSummary(operation) => Some(*operation),
                _ => None,
            })
            .unwrap();
        app.complete_overview(OverviewCompletion::Summary(
            operation,
            Err(OverviewSummaryError::new(
                OverviewSummaryFailure::Internal,
                "raw private protocol payload that must stay out of the report",
            )),
        ))
        .unwrap();
        let opened = app
            .handle_diagnostics_intent(DiagnosticsIntent::Open)
            .unwrap();
        let report = app
            .complete_diagnostics_read(
                opened.diagnostics().unwrap().read_operation().unwrap(),
                Ok(DiagnosticsReport::new(0, vec![])),
            )
            .unwrap();
        let text = report
            .diagnostics()
            .unwrap()
            .presentation()
            .report_text()
            .unwrap();
        assert!(text.contains("TV connection"));
        assert!(!text.contains("raw private protocol"));
    }

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
            .complete_pairing(&operation, Ok(profile(&operation).into()))
            .unwrap();
        let tvs = completed.tvs().unwrap();
        assert_eq!(tvs.presentation().profiles(), &[profile(&operation)]);
        assert!(tvs.presentation().pairing().is_none());
        assert!(tvs.toast_message().is_none());
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
            .complete_pairing(&operation, Ok(profile(&operation).into()))
            .unwrap();
        assert!(paired.navigation().tabs_visible());
        assert_eq!(paired.navigation().selected(), ApplicationPage::Overview);
        assert!(paired.tvs().unwrap().toast_message().is_none());
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
    fn pairing_queues_independent_activation_after_reading_saved_off_policies() {
        use crate::presentation::settings::{SettingsEditor, SettingsPresentation};
        use crate::settings::{ConfigEnvReader, SettingsError};
        let groups = || {
            SettingsPresentation::from_store(
                &ConfigEnvReader::parse(
                    "/unused/config.env",
                    "screen_idle_blank=disabled\nsystem_sleep_wake_policy=disabled\n",
                )
                .into_store(),
            )
            .groups()
            .to_vec()
        };
        let requested = vec![
            BehaviorSetting::ScreenIdleBlank,
            BehaviorSetting::SystemSleepWakePolicy,
        ];
        let (mut app, opening, operation) = pairing();
        let paired = app
            .complete_pairing(
                &operation,
                Ok(PairingOutcome::new(profile(&operation), requested.clone())),
            )
            .unwrap();
        assert!(paired.navigation().tabs_visible());
        assert!(paired.tvs().unwrap().presentation().pairing().is_none());
        assert!(app.handle_tvs_intent(TvsIntent::UnpairTv).is_none());
        assert!(app
            .complete_settings_read(
                opening.settings().unwrap().read_operation().unwrap(),
                Ok(groups())
            )
            .is_none());
        let mut update = app
            .complete_settings_read(
                paired.settings().unwrap().read_operation().unwrap(),
                Ok(groups()),
            )
            .unwrap();
        for setting in requested {
            let activation = update
                .settings()
                .unwrap()
                .mutation_operation()
                .unwrap()
                .clone();
            assert_eq!(activation.setting(), setting);
            // Failure to activate one behavior must not prevent the other attempt.
            update = app
                .complete_settings_mutation(
                    &activation,
                    Err(SettingsMutationFailure::Activation(
                        SettingsError::ActivationCancelled,
                    )),
                )
                .unwrap();
            let row = update
                .settings()
                .unwrap()
                .presentation()
                .groups()
                .iter()
                .flat_map(|group| group.rows())
                .find(|row| row.setting() == setting)
                .unwrap();
            assert!(matches!(
                row.editor(),
                SettingsEditor::Toggle { value: Some(false) }
            ));
            assert!(row.retry_apply_action().is_none());
            assert!(update.navigation().tabs_visible());
        }
        assert!(update.settings().unwrap().mutation_operation().is_none());
        assert!(app.handle_tvs_intent(TvsIntent::UnpairTv).is_some());
    }

    #[test]
    fn failed_post_pair_read_keeps_requests_for_retry_and_unpair_discards_them() {
        use crate::presentation::settings::SettingsPresentation;
        use crate::settings::ConfigEnvReader;
        for unpair in [false, true] {
            let (mut app, _, operation) = pairing();
            let paired = app
                .complete_pairing(
                    &operation,
                    Ok(PairingOutcome::new(
                        profile(&operation),
                        vec![BehaviorSetting::ScreenIdleBlank],
                    )),
                )
                .unwrap();
            let failed = app
                .complete_settings_read(
                    paired.settings().unwrap().read_operation().unwrap(),
                    Err(SettingsReadError::unreadable("temporarily unreadable")),
                )
                .unwrap();
            assert!(failed.navigation().tabs_visible());
            if unpair {
                app.handle_tvs_intent(TvsIntent::UnpairTv).unwrap();
                let confirm = app.handle_tvs_intent(TvsIntent::ConfirmUnpair).unwrap();
                assert!(app.handle_settings_intent(SettingsIntent::Retry).is_none());
                app.complete_tvs_management(
                    confirm.tvs().unwrap().management_operation().unwrap(),
                    Ok(TvsManagementOutcome::Unpaired),
                )
                .unwrap();
            }
            let retry = app.handle_settings_intent(SettingsIntent::Retry).unwrap();
            let ready = app
                .complete_settings_read(
                    retry.settings().unwrap().read_operation().unwrap(),
                    Ok(SettingsPresentation::from_store(
                        &ConfigEnvReader::parse(
                            "/unused/config.env",
                            "screen_idle_blank=disabled\n",
                        )
                        .into_store(),
                    )
                    .groups()
                    .to_vec()),
                )
                .unwrap();
            assert_eq!(
                ready
                    .settings()
                    .unwrap()
                    .mutation_operation()
                    .map(|op| op.setting()),
                (!unpair).then_some(BehaviorSetting::ScreenIdleBlank)
            );
        }
    }

    #[test]
    fn configured_offline_tv_keeps_navigation() {
        let (_, _, pairing_operation) = pairing();
        let (mut app, opening) = Application::open();
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
            .complete_pairing(&operation, Ok(profile(&operation).into()))
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
            .complete_pairing(&operation, Ok(profile(&operation).into()))
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
