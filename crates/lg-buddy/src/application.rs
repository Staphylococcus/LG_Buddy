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
    SettingsApplication, SettingsIntent, SettingsMutationOperation, SettingsReadError,
    SettingsReadOperation, SettingsTransition,
};
use crate::tv::{AudioStatus, OledBrightness};
use crate::tvs::{
    TvProfile, TvsApplication, TvsIntent, TvsManagementError, TvsManagementOperation,
    TvsManagementOutcome, TvsModelReadOperation, TvsReadError, TvsReadOperation, TvsTransition,
};

use crate::setup::{
    assessment::{AssessmentOperation, SetupAssessment, SetupHealth},
    flow::FlowProgress,
    gui::{
        OnboardingApplication, OnboardingIntent, OnboardingOperation, OnboardingResult,
        OnboardingTransition, SetupStatus,
    },
    StepFailure,
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
    onboarding: Option<OnboardingTransition>,
    setup_status: SetupStatus,
    setup_available: bool,
    assessment: Option<AssessmentOperation>,
}

impl ApplicationTransition {
    pub fn assessment_operation(&self) -> Option<AssessmentOperation> {
        self.assessment
    }
    pub fn onboarding(&self) -> Option<&OnboardingTransition> {
        self.onboarding.as_ref()
    }
    pub fn setup_status(&self) -> SetupStatus {
        self.setup_status
    }
    pub fn setup_available(&self) -> bool {
        self.setup_available
    }

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
    onboarding: OnboardingApplication,
    setup_health: SetupHealth,
    close_after_setup: bool,
    closed: bool,
}

impl Application {
    pub fn open() -> (Self, ApplicationTransition) {
        let (overview, overview_opening) = OverviewApplication::open();
        let (tvs, tvs_opening) = TvsApplication::open();
        let (settings, settings_opening) = SettingsApplication::open();
        let mut setup_health = SetupHealth::default();
        let assessment = setup_health.request();
        (
            Self {
                overview,
                tvs,
                settings,
                diagnostics: DiagnosticsApplication::default(),
                navigation: Navigation::default(),
                onboarding: OnboardingApplication::default(),
                setup_health,
                close_after_setup: false,
                closed: false,
            },
            ApplicationTransition {
                overview: Some(overview_opening),
                tvs: Some(tvs_opening),
                settings: Some(settings_opening),
                diagnostics: None,
                navigation: Navigation::default(),
                onboarding: None,
                setup_status: SetupStatus::Unchecked,
                setup_available: true,
                assessment,
            },
        )
    }

    pub fn handle_overview_intent(
        &mut self,
        intent: OverviewIntent,
    ) -> Option<ApplicationTransition> {
        if self.onboarding.is_open() {
            if intent != OverviewIntent::Cancel {
                return None;
            }
            let update = self.onboarding.handle(OnboardingIntent::Cancel)?;
            if self.onboarding.is_open() {
                self.close_after_setup = true;
                return Some(self.onboarding_transition(update));
            }
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
        if intent == TvsIntent::PairTv {
            if !self.tvs.can_pair() {
                return None;
            }
            return self.handle_onboarding_intent(OnboardingIntent::Open);
        }
        if self.onboarding.is_open() {
            if let TvsIntent::Pairing(intent) = intent {
                use crate::pairing::PairingIntent;
                return self.handle_onboarding_intent(match intent {
                    PairingIntent::SetAddress(value) => OnboardingIntent::SetAddress(value),
                    PairingIntent::SetMac(value) => OnboardingIntent::SetMac(value),
                    PairingIntent::SetInput(value) => OnboardingIntent::SetInput(value),
                    PairingIntent::Submit => OnboardingIntent::Submit,
                    PairingIntent::Cancel => OnboardingIntent::Cancel,
                });
            }
            return None;
        }
        let transition = self.tvs.handle_intent(intent)?;
        Some(self.tvs_transition(transition))
    }

    pub fn handle_settings_intent(
        &mut self,
        intent: SettingsIntent,
    ) -> Option<ApplicationTransition> {
        if intent == SettingsIntent::CompleteSetup {
            if self.setup_health.status() != SetupStatus::Incomplete {
                return None;
            }
            return self.handle_onboarding_intent(OnboardingIntent::Open);
        }
        if self.onboarding.is_open() {
            return None;
        }
        let transition = self.settings.handle_intent(intent)?;
        Some(self.settings_transition(transition))
    }

    pub fn handle_onboarding_intent(
        &mut self,
        intent: OnboardingIntent,
    ) -> Option<ApplicationTransition> {
        if self.closed {
            return None;
        }
        if intent == OnboardingIntent::Open
            && (self.settings.is_mutating()
                || self.tvs.is_managing()
                || self.tvs.is_pairing()
                || self.overview.has_pending_write())
        {
            return None;
        }
        let update = self.onboarding.handle(intent)?;
        Some(self.onboarding_transition(update))
    }
    pub fn onboarding_progress(
        &mut self,
        operation: &OnboardingOperation,
        progress: FlowProgress,
    ) -> Option<ApplicationTransition> {
        let update = self.onboarding.progress(operation, progress)?;
        Some(self.onboarding_transition(update))
    }
    pub fn complete_onboarding(
        &mut self,
        operation: &OnboardingOperation,
        result: Result<OnboardingResult, StepFailure>,
    ) -> Option<ApplicationTransition> {
        let update = self.onboarding.complete(operation, result)?;
        Some(self.onboarding_transition(update))
    }
    pub fn onboarding_worker_stopped(
        &mut self,
        operation: &OnboardingOperation,
    ) -> Option<ApplicationTransition> {
        let update = self.onboarding.worker_stopped(operation)?;
        Some(self.onboarding_transition(update))
    }
    pub fn refresh_setup(&mut self) -> Option<ApplicationTransition> {
        if self.closed {
            return None;
        }
        let operation = self.setup_health.request()?;
        let mut transition = self.transition(None, None, None);
        transition.assessment = Some(operation);
        Some(transition)
    }
    pub fn complete_setup_assessment(
        &mut self,
        operation: AssessmentOperation,
        result: Result<SetupAssessment, StepFailure>,
    ) -> Option<ApplicationTransition> {
        if self.closed {
            return None;
        }
        let next = self.setup_health.complete(operation, result)?;
        let mut transition = self.transition(None, None, None);
        transition.assessment = next;
        Some(transition)
    }
    fn onboarding_transition(&mut self, update: OnboardingTransition) -> ApplicationTransition {
        if update.presentation.is_some() && !self.onboarding.is_busy() {
            self.setup_health.observe_flow(self.onboarding.status());
        }
        if let Some(error) = update
            .presentation
            .as_ref()
            .and_then(|view| view.error.as_ref())
        {
            self.diagnostics
                .record_failure("Setup", &format!("{} {}", error.summary(), error.detail()));
        }
        let mut transition = if update.presentation.is_none() {
            // Pairing may have completed before a later step was cancelled.
            let tvs = self.tvs.refresh_after_setup();
            let settings = self.settings.profile_changed();
            let overview = self.overview.profile_changed();
            self.transition(overview, Some(tvs), Some(settings))
        } else {
            self.transition(None, None, None)
        };
        if self.close_after_setup && !self.onboarding.is_open() {
            self.close_after_setup = false;
            if let Some(close) = self.handle_overview_intent(OverviewIntent::Cancel) {
                transition = close;
            }
        }
        transition.onboarding = Some(update);
        transition
    }

    pub fn select_page(
        &mut self,
        page: crate::navigation::ApplicationPage,
    ) -> Option<ApplicationTransition> {
        if self.closed || !self.navigation.select(page) {
            return None;
        }
        let mut transition = match page {
            crate::navigation::ApplicationPage::Settings => self
                .handle_settings_intent(SettingsIntent::Refresh)
                .or_else(|| Some(self.transition(None, None, None))),
            _ => Some(self.transition(None, None, None)),
        }?;
        if page == crate::navigation::ApplicationPage::Settings {
            transition.assessment = self.setup_health.request();
        }
        Some(transition)
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
        let mut transition = self
            .tvs
            .complete_pairing(operation, result.map(PairingOutcome::into_profile))?;
        let paired = transition.profile_changed();
        if paired {
            transition.clear_toast();
        }
        let mut update = self.tvs_transition(transition);
        if paired {
            let settings = self.settings.profile_changed();
            let assessment = update.assessment;
            update = self.transition(update.overview, update.tvs, Some(settings));
            update.assessment = assessment.or(update.assessment);
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
        self.tvs.is_pairing() || self.onboarding.is_open()
    }

    pub fn shutdown(&mut self) {
        let _ = self.onboarding.handle(OnboardingIntent::Cancel);
        self.setup_health.shutdown();
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
        let assessment = self.setup_health.set_paused(
            self.closed
                || self.onboarding.is_open()
                || self.settings.is_mutating()
                || self.tvs.is_managing()
                || self.tvs.is_pairing(),
        );
        if let Some(update) = self.tvs.set_controls_available(
            !self.overview.has_pending_write()
                && !self.settings.is_mutating()
                && !self.onboarding.is_open(),
        ) {
            if let Some(tvs) = &mut tvs {
                tvs.update_presentation_from(update);
            } else {
                tvs = Some(update);
            }
        }
        if let Some(update) = self.settings.set_controls_available(
            !self.tvs.is_managing() && !self.tvs.is_pairing() && !self.onboarding.is_open(),
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
            onboarding: None,
            setup_status: self.setup_health.status(),
            assessment,
            setup_available: !self.onboarding.is_open()
                && !self.settings.is_mutating()
                && !self.tvs.is_managing()
                && !self.overview.has_pending_write(),
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
            let update = application.tvs.handle_intent(intent).unwrap();
            application.tvs_transition(update);
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
    fn startup_is_read_only_and_closing_onboarding_rechecks_after_stale_completion() {
        use crate::setup::gui::fixtures::Fixture;
        let fixture = Fixture::new(true, false);
        let (mut app, opening) = Application::open();
        let old = opening.assessment_operation().unwrap();
        assert!(opening.onboarding().is_none());
        let old_result = old.execute(&fixture);
        let opening = app
            .handle_onboarding_intent(OnboardingIntent::Open)
            .unwrap();
        assert!(opening.assessment_operation().is_none());
        let operation = opening.onboarding().unwrap().operation.clone().unwrap();
        let result = operation.execute_with(&fixture, &mut |_| {});
        app.complete_onboarding(&operation, result).unwrap();
        let apply = app
            .handle_onboarding_intent(OnboardingIntent::Submit)
            .unwrap();
        let operation = apply.onboarding().unwrap().operation.clone().unwrap();
        let result = operation.execute_with(&fixture, &mut |_| {});
        let complete = app.complete_onboarding(&operation, result).unwrap();
        assert_eq!(complete.setup_status(), SetupStatus::Complete);
        let stale = app.complete_setup_assessment(old, old_result).unwrap();
        assert_eq!(stale.setup_status(), SetupStatus::Complete);
        assert!(stale.assessment_operation().is_none());
        let close = app
            .handle_onboarding_intent(OnboardingIntent::Submit)
            .unwrap();
        let fresh = close.assessment_operation().unwrap();
        let verified = app
            .complete_setup_assessment(fresh, fresh.execute(&fixture))
            .unwrap();
        assert_eq!(verified.setup_status(), SetupStatus::Complete);
        assert!(verified.onboarding().is_none());
        assert!(app.refresh_setup().is_some());
        app.shutdown();
        assert!(app
            .complete_setup_assessment(fresh, fresh.execute(&fixture))
            .is_none());
        assert!(app.refresh_setup().is_none());
    }

    #[test]
    fn onboarding_owns_mutations_and_rejected_quit_is_not_queued() {
        use crate::setup::gui::fixtures::Fixture;
        let fixture = Fixture::new(true, false);
        let (mut app, _) = Application::open();
        let opening = app
            .handle_onboarding_intent(OnboardingIntent::Open)
            .unwrap();
        let operation = opening.onboarding().unwrap().operation.clone().unwrap();
        let result = operation.execute_with(&fixture, &mut |_| {});
        let ready = app.complete_onboarding(&operation, result).unwrap();
        assert_eq!(ready.setup_status(), SetupStatus::Incomplete);
        assert!(!ready.setup_available());
        assert!(app.handle_settings_intent(SettingsIntent::Retry).is_none());
        assert!(app.handle_tvs_intent(TvsIntent::UnpairTv).is_none());

        let action = app
            .handle_onboarding_intent(OnboardingIntent::Submit)
            .unwrap();
        let operation = action.onboarding().unwrap().operation.clone().unwrap();
        let result = operation.execute_with(&fixture, &mut |_| {
            // The live step is already noncancelable, even before GTK renders
            // the progress event that disables the Cancel button.
            assert!(app.handle_overview_intent(OverviewIntent::Cancel).is_none());
            assert!(!app.closed);
        });
        let done = app.complete_onboarding(&operation, result).unwrap();
        assert_eq!(done.setup_status(), SetupStatus::Complete);
        assert!(!app.closed);
        assert!(!app.close_after_setup);
        app.handle_overview_intent(OverviewIntent::Cancel).unwrap();
        assert!(app.closed);
    }

    #[test]
    fn standalone_pairing_retains_assessment_when_refreshing_settings() {
        let (mut app, opening, operation) = pairing();
        app.complete_setup_assessment(
            opening.assessment_operation().unwrap(),
            Err(crate::setup::assessment::worker_stopped()),
        )
        .unwrap();
        let done = app
            .complete_pairing(&operation, Ok(profile(&operation).into()))
            .unwrap();
        assert!(done.settings().is_some());
        assert!(done.assessment_operation().is_some());
    }

    #[test]
    fn standalone_pairing_completion_does_not_queue_behavior_changes() {
        use crate::presentation::settings::SettingsPresentation;
        use crate::settings::{ConfigEnvReader, SettingValue};
        let (mut app, _, operation) = pairing();
        let paired = app
            .complete_pairing(
                &operation,
                Ok(PairingOutcome::new(
                    profile(&operation),
                    vec![crate::settings_view::BehaviorSetting::ScreenIdleBlank],
                )),
            )
            .unwrap();
        let store = ConfigEnvReader::parse(
            "/unused/config.env",
            "screen_idle_blank=disabled\nsystem_sleep_wake_policy=enabled\n",
        )
        .into_store();
        let ready = app
            .complete_settings_read(
                paired.settings().unwrap().read_operation().unwrap(),
                Ok(SettingsPresentation::from_store(&store).groups().to_vec()),
            )
            .unwrap();
        assert!(ready.settings().unwrap().mutation_operation().is_none());
        assert_eq!(
            store
                .effective_by_name("screen.idle_blank")
                .unwrap()
                .value(),
            Some(SettingValue::Enum("disabled"))
        );
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
    fn entering_settings_rechecks_external_service_changes() {
        use crate::{
            navigation::ApplicationPage,
            setup::{gui::fixtures::Fixture, StepResponse},
        };
        let fixture = Fixture::new(true, false);
        fixture.responses.lock().unwrap()[1] = StepResponse::Complete;
        let (mut app, opening) = configured();
        let first = opening.assessment_operation().unwrap();
        let complete = app
            .complete_setup_assessment(first, first.execute(&fixture))
            .unwrap();
        assert_eq!(complete.setup_status(), SetupStatus::Complete);
        fixture.responses.lock().unwrap()[1] =
            StepResponse::Failed(crate::setup::assessment::worker_stopped());
        let refresh = app
            .select_page(ApplicationPage::Settings)
            .unwrap()
            .assessment_operation()
            .unwrap();
        let incomplete = app
            .complete_setup_assessment(refresh, refresh.execute(&fixture))
            .unwrap();
        assert_eq!(incomplete.setup_status(), SetupStatus::Incomplete);
        assert!(incomplete.onboarding().is_none());
        assert!(fixture.calls.lock().unwrap().is_empty());
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
    fn settings_mutations_discard_older_health_and_schedule_fresh_inspection() {
        use crate::setup::gui::fixtures::Fixture;
        let fixture = Fixture::new(true, false);
        let (mut app, opening) = Application::open();
        let old = opening.assessment_operation().unwrap();
        app.complete_settings_read(
            opening.settings().unwrap().read_operation().unwrap(),
            Ok(settings()),
        )
        .unwrap();
        let change = app
            .handle_settings_intent(SettingsIntent::SetEnabled {
                setting: BehaviorSetting::ScreenIdleBlank,
                enabled: false,
            })
            .unwrap();
        let stale = app
            .complete_setup_assessment(old, old.execute(&fixture))
            .unwrap();
        assert_eq!(stale.setup_status(), SetupStatus::Unchecked);
        assert!(stale.assessment_operation().is_none());
        let done = app
            .complete_settings_mutation(
                change.settings().unwrap().mutation_operation().unwrap(),
                Err(SettingsMutationFailure::Persistence(SettingsError::Apply {
                    message: "test failure".into(),
                })),
            )
            .unwrap();
        let fresh = done.assessment_operation().unwrap();
        assert_ne!(fresh, old);
        let inspected = app
            .complete_setup_assessment(fresh, fresh.execute(&fixture))
            .unwrap();
        assert_eq!(inspected.setup_status(), SetupStatus::Incomplete);
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
