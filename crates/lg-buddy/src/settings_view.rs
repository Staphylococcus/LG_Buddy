//! Toolkit-independent coordination for the Settings view.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;

use crate::presentation::brightness::UserFacingError;
use crate::presentation::settings::{
    SettingsEditStatus, SettingsEditor, SettingsFeedback, SettingsFeedbackSeverity, SettingsGroup,
    SettingsPresentation, SettingsRow,
};
use crate::presentation::update_check::{AvailableUpdate, UpdateCheckReport};
use crate::settings::{
    execute_settings_mutation, retry_settings_apply, ConfigPathResolver, SettingsApplier,
    SettingsApplyOutcome, SettingsError, SettingsMutation, SettingsMutationFailure,
    SettingsMutationOutcome, SettingsMutationStage, SettingsStore,
};
use crate::update_flow::{
    UpdateInstallApplication, UpdateInstallFailure, UpdateInstallOperation, UpdateInstallOutcome,
    UpdateInstallTask,
};
use crate::update_install::UpdateInstallStage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BehaviorSetting {
    ScreenBackend,
    ScreenIdleBlank,
    ScreenIdleTimeout,
    ScreenRestorePolicy,
    SystemSleepWakePolicy,
    UpdatesAutoCheck,
    UpdatesChannel,
}

impl BehaviorSetting {
    pub fn key_name(self) -> &'static str {
        match self {
            Self::ScreenBackend => "screen.backend",
            Self::ScreenIdleBlank => "screen.idle_blank",
            Self::ScreenIdleTimeout => "screen.idle_timeout",
            Self::ScreenRestorePolicy => "screen.restore_policy",
            Self::SystemSleepWakePolicy => "system.sleep_wake_policy",
            Self::UpdatesAutoCheck => "updates.auto_check",
            Self::UpdatesChannel => "updates.channel",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "screen.backend" => Self::ScreenBackend,
            "screen.idle_blank" => Self::ScreenIdleBlank,
            "screen.idle_timeout" => Self::ScreenIdleTimeout,
            "screen.restore_policy" => Self::ScreenRestorePolicy,
            "system.sleep_wake_policy" => Self::SystemSleepWakePolicy,
            "updates.auto_check" => Self::UpdatesAutoCheck,
            "updates.channel" => Self::UpdatesChannel,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsIntent {
    Retry,
    Refresh,
    CheckForUpdates,
    PrepareUpdateInstall,
    ConfirmUpdateInstall,
    CancelUpdateInstall,
    RelaunchUpdatedApplication,
    SetEnabled {
        setting: BehaviorSetting,
        enabled: bool,
    },
    Commit {
        setting: BehaviorSetting,
        value: String,
    },
    Reset(BehaviorSetting),
    RetryApply(BehaviorSetting),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsMutationRequest {
    Set(String),
    Reset,
    RetryApply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsMutationOperation {
    id: u64,
    setting: BehaviorSetting,
    request: SettingsMutationRequest,
}

impl SettingsMutationOperation {
    fn new(id: u64, setting: BehaviorSetting, request: SettingsMutationRequest) -> Self {
        Self {
            id,
            setting,
            request,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn setting(&self) -> BehaviorSetting {
        self.setting
    }

    pub fn key_name(&self) -> &'static str {
        self.setting.key_name()
    }

    pub fn request(&self) -> &SettingsMutationRequest {
        &self.request
    }
}

/// Opaque identity for one asynchronous settings read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsReadOperation(u64);

/// Opaque identity for a manual check, independent of settings reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateCheckOperation(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckError {
    presentation: UserFacingError,
    diagnostic: String,
}

impl UpdateCheckError {
    pub fn stopped() -> Self {
        Self {
            presentation: UserFacingError::new(
                "Update check stopped",
                "The check did not finish. Try again.",
            ),
            diagnostic: "the update check worker stopped before returning a result".into(),
        }
    }
}

impl From<crate::updates::UpdatesError> for UpdateCheckError {
    fn from(error: crate::updates::UpdatesError) -> Self {
        use crate::updates::UpdatesError;
        let detail = match &error {
            UpdatesError::Settings(_) | UpdatesError::SettingsInvariant(_) => {
                "Check the saved Update channel in Settings, then try again."
            }
            UpdatesError::Http { .. } => "Check your internet connection, then try again.",
            UpdatesError::ApiStatus { status: 403 | 429, .. } => {
                "The release service refused the request. Try again later."
            }
            UpdatesError::InvalidLocalVersion { .. } => {
                "The installed version could not be compared. Install a supported LG Buddy release."
            }
            _ => "Release information could not be retrieved. Try again; if this continues, check the LG Buddy logs.",
        };
        Self {
            presentation: UserFacingError::new("Could not check for updates", detail),
            diagnostic: error.to_string(),
        }
    }
}

/// A one-shot update result for a toolkit renderer to show as a toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    title: String,
    details: Option<String>,
}

impl UpdateNotice {
    fn new(title: impl Into<String>, details: Option<String>) -> Self {
        Self {
            title: title.into(),
            details,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn details(&self) -> Option<&str> {
        self.details.as_deref()
    }
}

impl SettingsReadOperation {
    pub(crate) fn new(id: u64) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsTransition {
    presentation: SettingsPresentation,
    read_operation: Option<SettingsReadOperation>,
    mutation_operation: Option<SettingsMutationOperation>,
    update_check_operation: Option<UpdateCheckOperation>,
    update_install_operation: Option<UpdateInstallOperation>,
    update_notice: Option<UpdateNotice>,
    diagnostic: Option<String>,
}

impl SettingsTransition {
    pub fn presentation(&self) -> &SettingsPresentation {
        &self.presentation
    }

    pub fn read_operation(&self) -> Option<SettingsReadOperation> {
        self.read_operation
    }

    pub fn mutation_operation(&self) -> Option<&SettingsMutationOperation> {
        self.mutation_operation.as_ref()
    }

    pub fn update_check_operation(&self) -> Option<UpdateCheckOperation> {
        self.update_check_operation
    }

    pub fn update_install_operation(&self) -> Option<&UpdateInstallOperation> {
        self.update_install_operation.as_ref()
    }

    pub fn update_notice(&self) -> Option<&UpdateNotice> {
        self.update_notice.as_ref()
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsReadFailure {
    Stopped,
    Unreadable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsReadError {
    failure: SettingsReadFailure,
    diagnostic: String,
}

impl SettingsReadError {
    pub fn new(failure: SettingsReadFailure, diagnostic: impl Into<String>) -> Self {
        Self {
            failure,
            diagnostic: diagnostic.into(),
        }
    }

    pub fn stopped() -> Self {
        Self::new(SettingsReadFailure::Stopped, "settings read stopped")
    }

    pub fn unreadable(diagnostic: impl Into<String>) -> Self {
        Self::new(SettingsReadFailure::Unreadable, diagnostic)
    }

    pub fn internal(diagnostic: impl Into<String>) -> Self {
        Self::new(SettingsReadFailure::Internal, diagnostic)
    }

    pub fn failure(&self) -> SettingsReadFailure {
        self.failure
    }

    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

impl fmt::Display for SettingsReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic)
    }
}

impl Error for SettingsReadError {}

pub trait SettingsBackend: Send + Sync + 'static {
    fn read_settings(&self) -> Result<Vec<SettingsGroup>, SettingsReadError>;

    fn check_for_updates(&self) -> Result<UpdateCheckReport, UpdateCheckError>;

    fn write_setting(
        &self,
        operation: SettingsMutationOperation,
        progress: &mut dyn FnMut(SettingsMutationStage),
    ) -> Result<SettingsMutationOutcome, SettingsMutationFailure>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentSettingsBackend;

impl SettingsBackend for EnvironmentSettingsBackend {
    fn check_for_updates(&self) -> Result<UpdateCheckReport, UpdateCheckError> {
        let outcome = crate::updates::check_for_updates()?;
        let result = outcome.result();
        Ok(UpdateCheckReport {
            installed_version: result.current_version().to_string(),
            channel: result.check_channel(),
            available_release: result.update_available().then(|| AvailableUpdate {
                version: result.latest().version().to_string(),
                url: result.latest().url().to_owned(),
            }),
            warning: (!outcome.warnings().is_empty()).then(|| {
                "The release check succeeded, but its cache could not be read or saved. A later check may need to download the release information again.".into()
            }),
        })
    }

    fn read_settings(&self) -> Result<Vec<SettingsGroup>, SettingsReadError> {
        let store = SettingsStore::load_from_env()
            .map_err(|error| SettingsReadError::unreadable(error.to_string()))?;
        Ok(SettingsPresentation::from_store(&store).groups().to_vec())
    }

    fn write_setting(
        &self,
        operation: SettingsMutationOperation,
        progress: &mut dyn FnMut(SettingsMutationStage),
    ) -> Result<SettingsMutationOutcome, SettingsMutationFailure> {
        let path =
            ConfigPathResolver::resolve_from_env().map_err(SettingsMutationFailure::Persistence)?;
        let store = SettingsStore::load(&path).map_err(SettingsMutationFailure::Persistence)?;
        let applier = SettingsApplier::from_env();
        match operation.request() {
            SettingsMutationRequest::Set(value) => {
                progress(SettingsMutationStage::Validating);
                let mutation = SettingsMutation::set(&store, operation.key_name(), value)
                    .map_err(SettingsMutationFailure::Validation)?;
                execute_settings_mutation(&path, mutation, &applier, progress)
            }
            SettingsMutationRequest::Reset => {
                progress(SettingsMutationStage::Validating);
                let mutation = SettingsMutation::unset(&store, operation.key_name())
                    .map_err(SettingsMutationFailure::Validation)?;
                execute_settings_mutation(&path, mutation, &applier, progress)
            }
            SettingsMutationRequest::RetryApply => {
                retry_settings_apply(&store, operation.key_name(), &applier, progress)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SettingsApplicationState {
    Loading(SettingsReadOperation),
    Ready,
    Failed,
}

#[derive(Debug, Clone)]
struct PendingMutation {
    operation: SettingsMutationOperation,
    previous_row: SettingsRow,
}

#[derive(Debug)]
pub struct SettingsApplication {
    state: SettingsApplicationState,
    next_operation_id: u64,
    presentation: SettingsPresentation,
    controls_available: bool,
    closed: bool,
    pending_mutation: Option<PendingMutation>,
    queued_mutations: VecDeque<PendingMutation>,
    reconcile_mutation: Option<PendingMutation>,
    pending_update_check: Option<UpdateCheckOperation>,
    update_install: UpdateInstallApplication,
}

impl SettingsApplication {
    pub fn open() -> (Self, SettingsTransition) {
        let operation = SettingsReadOperation::new(0);
        let presentation = SettingsPresentation::loading();
        (
            Self {
                state: SettingsApplicationState::Loading(operation),
                next_operation_id: 1,
                presentation: presentation.clone(),
                controls_available: true,
                closed: false,
                pending_mutation: None,
                queued_mutations: VecDeque::new(),
                reconcile_mutation: None,
                pending_update_check: None,
                update_install: UpdateInstallApplication::default(),
            },
            SettingsTransition {
                presentation,
                read_operation: Some(operation),
                mutation_operation: None,
                update_check_operation: None,
                update_install_operation: None,
                update_notice: None,
                diagnostic: None,
            },
        )
    }

    pub fn handle_intent(&mut self, intent: SettingsIntent) -> Option<SettingsTransition> {
        if self.closed {
            return None;
        }
        self.sync_update_install();
        if matches!(
            intent,
            SettingsIntent::PrepareUpdateInstall
                | SettingsIntent::ConfirmUpdateInstall
                | SettingsIntent::CancelUpdateInstall
                | SettingsIntent::RelaunchUpdatedApplication
        ) {
            let report = self.presentation.update_check().result().cloned();
            let operation = self.update_install.handle(intent, report.as_ref())?;
            let mut transition = self.transition(None, None, None);
            transition.update_install_operation = operation;
            return Some(transition);
        }
        match intent {
            SettingsIntent::CheckForUpdates
                if self.pending_update_check.is_none()
                    && !self.update_install.active()
                    && !self.presentation.groups().is_empty() =>
            {
                let operation = UpdateCheckOperation(self.next_operation_id);
                self.next_operation_id += 1;
                self.pending_update_check = Some(operation);
                self.presentation.update_check_mut().start();
                let mut transition = self.transition(None, None, None);
                transition.update_check_operation = Some(operation);
                Some(transition)
            }
            SettingsIntent::Retry if matches!(self.state, SettingsApplicationState::Failed) => {
                Some(self.begin_read())
            }
            SettingsIntent::Refresh
                if self.pending_mutation.is_none()
                    && matches!(
                        self.state,
                        SettingsApplicationState::Ready | SettingsApplicationState::Failed
                    ) =>
            {
                Some(self.begin_read())
            }
            SettingsIntent::SetEnabled { setting, enabled } if self.can_edit() => {
                let is_toggle = self
                    .presentation
                    .row(setting)
                    .is_some_and(|row| matches!(row.editor(), SettingsEditor::Toggle { .. }));
                is_toggle
                    .then(|| {
                        self.begin_mutation(
                            setting,
                            SettingsMutationRequest::Set(if enabled {
                                "enabled".to_string()
                            } else {
                                "disabled".to_string()
                            }),
                        )
                    })
                    .flatten()
            }
            SettingsIntent::Commit { setting, value } if self.can_edit() => {
                self.begin_mutation(setting, SettingsMutationRequest::Set(value))
            }
            SettingsIntent::Reset(setting) if self.can_edit() => {
                self.begin_mutation(setting, SettingsMutationRequest::Reset)
            }
            SettingsIntent::RetryApply(setting) if self.can_edit() => {
                let retryable = self
                    .presentation
                    .row(setting)
                    .and_then(SettingsRow::retry_apply_action)
                    .is_some_and(|action| action.enabled());
                retryable
                    .then(|| self.begin_mutation(setting, SettingsMutationRequest::RetryApply))
                    .flatten()
            }
            _ => None,
        }
    }

    pub fn complete_read(
        &mut self,
        operation: SettingsReadOperation,
        result: Result<Vec<SettingsGroup>, SettingsReadError>,
    ) -> Option<SettingsTransition> {
        if !matches!(self.state, SettingsApplicationState::Loading(active) if active == operation) {
            return None;
        }

        if self.closed {
            return self
                .start_queued_mutation()
                .map(|next| self.transition(None, Some(next), None));
        }

        let (presentation, diagnostic) = match result {
            Ok(groups) => {
                let previous = self.presentation.clone();
                let mut presentation = SettingsPresentation::ready(groups);
                presentation.preserve_apply_failures_from(&previous);
                if let Some(pending) = self.reconcile_mutation.take() {
                    let changed =
                        presentation
                            .row(pending.operation.setting())
                            .is_some_and(|row| {
                                row.value_label() != pending.previous_row.value_label()
                                    || row.source_label() != pending.previous_row.source_label()
                            });
                    presentation.set_row_state(
                        pending.operation.setting(),
                        if changed {
                            SettingsEditStatus::ApplyFailed
                        } else {
                            SettingsEditStatus::PersistenceFailed
                        },
                        Some(SettingsFeedback::new(
                            SettingsFeedbackSeverity::Warning,
                            if changed {
                                "The setting was saved, but runtime apply could not be confirmed. Retry apply."
                            } else {
                                "The saved value is unchanged; runtime apply could not be confirmed. Retry apply."
                            },
                        )),
                        true,
                    );
                }
                presentation.set_controls_available(self.controls_available);
                self.state = SettingsApplicationState::Ready;
                (presentation, None)
            }
            Err(error) => {
                self.state = SettingsApplicationState::Failed;
                let diagnostic = error.diagnostic().to_string();
                let previous_groups = self.presentation.groups().to_vec();
                (
                    SettingsPresentation::failed_with_groups(
                        user_facing_read_error(error.failure()),
                        previous_groups,
                    ),
                    Some(diagnostic),
                )
            }
        };
        let update_check = self.presentation.update_check().clone();
        self.presentation = presentation;
        *self.presentation.update_check_mut() = update_check;
        if matches!(self.state, SettingsApplicationState::Ready) {
            self.finish_mutation(None, diagnostic)
        } else {
            Some(self.transition(None, None, diagnostic))
        }
    }

    pub fn complete_mutation(
        &mut self,
        operation: &SettingsMutationOperation,
        result: Result<SettingsMutationOutcome, SettingsMutationFailure>,
    ) -> Option<SettingsTransition> {
        let pending = match &self.pending_mutation {
            Some(pending) if pending.operation == *operation => pending.clone(),
            _ => return None,
        };
        if self.closed {
            self.pending_mutation = None;
            return self
                .start_queued_mutation()
                .map(|next| self.transition(None, Some(next), None));
        }
        self.pending_mutation = None;

        match result {
            Ok(outcome) => {
                let effective = outcome.change().effective_setting();
                let mut replacement = crate::presentation::settings::row_from_effective(effective);
                replacement.set_editor_enabled(self.controls_available);
                self.presentation.replace_row(replacement);
                self.presentation
                    .set_controls_available(self.controls_available);
                self.state = SettingsApplicationState::Ready;
                match outcome.apply() {
                    Ok(apply_outcome) => self.presentation.set_row_state(
                        operation.setting(),
                        SettingsEditStatus::Applied,
                        apply_feedback(apply_outcome),
                        false,
                    ),
                    Err(_error) => self.presentation.set_row_state(
                        operation.setting(),
                        SettingsEditStatus::ApplyFailed,
                        Some(SettingsFeedback::new(
                            SettingsFeedbackSeverity::Warning,
                            "Saved, but could not apply yet. Retry apply.",
                        )),
                        true,
                    ),
                }
                let diagnostic = outcome.apply().as_ref().err().map(ToString::to_string);
                self.finish_mutation(Some(operation.setting()), diagnostic)
            }
            Err(failure) => {
                let status = match &failure {
                    SettingsMutationFailure::Validation(_) => SettingsEditStatus::ValidationFailed,
                    SettingsMutationFailure::Persistence(_) => {
                        SettingsEditStatus::PersistenceFailed
                    }
                };
                let preserve_retry_apply = pending.previous_row.retry_apply_action().is_some();
                self.presentation.replace_row(pending.previous_row);
                self.presentation
                    .set_controls_available(self.controls_available);
                self.presentation.set_row_state(
                    operation.setting(),
                    status,
                    Some(SettingsFeedback::new(
                        SettingsFeedbackSeverity::Error,
                        mutation_failure_message(&failure),
                    )),
                    preserve_retry_apply,
                );
                self.state = SettingsApplicationState::Ready;
                self.finish_mutation(Some(operation.setting()), Some(failure.error().to_string()))
            }
        }
    }

    pub fn complete_update_check(
        &mut self,
        operation: UpdateCheckOperation,
        result: Result<UpdateCheckReport, UpdateCheckError>,
    ) -> Option<SettingsTransition> {
        if self.closed || self.pending_update_check != Some(operation) {
            return None;
        }
        self.pending_update_check = None;
        let update_notice = update_check_notice(&result);
        let diagnostic = result.as_ref().err().map(|error| error.diagnostic.clone());
        self.presentation
            .update_check_mut()
            .complete(result.map_err(|error| error.presentation));
        let mut transition = self.transition(None, None, diagnostic);
        transition.update_notice = update_notice;
        Some(transition)
    }

    pub fn mutation_worker_stopped(
        &mut self,
        operation: &SettingsMutationOperation,
    ) -> Option<SettingsTransition> {
        let pending = match &self.pending_mutation {
            Some(pending) if pending.operation == *operation => pending.clone(),
            _ => return None,
        };
        if self.closed {
            self.pending_mutation = None;
            return self
                .start_queued_mutation()
                .map(|next| self.transition(None, Some(next), None));
        }

        self.pending_mutation = None;
        self.presentation.set_row_state(
            operation.setting(),
            SettingsEditStatus::PersistenceFailed,
            Some(SettingsFeedback::new(
                SettingsFeedbackSeverity::Warning,
                "Refreshing settings to reconcile the stopped write…",
            )),
            false,
        );
        self.reconcile_mutation = Some(pending);
        self.presentation.set_controls_available(false);
        Some(self.begin_read())
    }

    pub fn is_mutating(&self) -> bool {
        self.pending_mutation.is_some()
            || !self.queued_mutations.is_empty()
            || self.update_install.active()
    }

    pub fn set_controls_available(&mut self, available: bool) -> Option<SettingsTransition> {
        if self.controls_available == available {
            return None;
        }
        self.controls_available = available;
        self.presentation.set_controls_available(available);
        Some(self.transition(None, None, None))
    }

    pub fn update_install_active(&self) -> bool {
        self.update_install.active()
    }

    pub fn can_close(&self) -> bool {
        self.update_install.can_close()
    }

    pub fn update_install_progress(
        &mut self,
        operation: &UpdateInstallOperation,
        stage: UpdateInstallStage,
    ) -> Option<SettingsTransition> {
        if self.closed || !self.update_install.progress(operation, stage) {
            return None;
        }
        Some(self.transition(None, None, None))
    }

    pub fn complete_update_install(
        &mut self,
        operation: &UpdateInstallOperation,
        result: Result<UpdateInstallOutcome, UpdateInstallFailure>,
    ) -> Option<SettingsTransition> {
        if self.closed {
            return None;
        }
        let (next, diagnostic) = self.update_install.complete(operation, result)?;
        let notice = self.update_install.presentation().error().map(|error| {
            let title = if matches!(operation.task(), UpdateInstallTask::Relaunch(_)) {
                "Restart required"
            } else {
                error.summary()
            };
            UpdateNotice::new(
                title,
                notice_details(error, diagnostic.as_deref().unwrap_or("")),
            )
        });
        let mut transition = self.transition(None, None, diagnostic);
        transition.update_install_operation = next;
        transition.update_notice = notice;
        Some(transition)
    }

    pub fn shutdown(&mut self) {
        self.update_install.can_close();
        self.closed = true;
    }

    pub fn presentation(&self) -> &SettingsPresentation {
        &self.presentation
    }

    fn can_edit(&self) -> bool {
        self.controls_available
            && !self.update_install.active()
            && self.reconcile_mutation.is_none()
            && !self.presentation.groups().is_empty()
            && !matches!(self.state, SettingsApplicationState::Failed)
    }

    fn begin_read(&mut self) -> SettingsTransition {
        let operation = SettingsReadOperation::new(self.next_operation_id);
        self.next_operation_id += 1;
        self.state = SettingsApplicationState::Loading(operation);
        self.presentation.mark_loading();
        self.transition(Some(operation), None, None)
    }

    fn begin_mutation(
        &mut self,
        setting: BehaviorSetting,
        request: SettingsMutationRequest,
    ) -> Option<SettingsTransition> {
        let previous_row = self.presentation.row(setting)?.clone();
        if matches!(self.state, SettingsApplicationState::Loading(_)) {
            let update_check = self.presentation.update_check().clone();
            self.presentation = SettingsPresentation::ready(self.presentation.groups().to_vec());
            *self.presentation.update_check_mut() = update_check;
        }
        let operation = SettingsMutationOperation::new(self.next_operation_id, setting, request);
        self.next_operation_id += 1;
        self.queued_mutations.push_back(PendingMutation {
            operation,
            previous_row,
        });
        // ponytail: one FIFO keeps writes ordered without disabling the form.
        let operation = if self.pending_mutation.is_none() {
            self.start_queued_mutation()
        } else {
            None
        };
        self.presentation
            .set_row_state(setting, SettingsEditStatus::Saving, None, false);
        Some(self.transition(None, operation, None))
    }

    fn start_queued_mutation(&mut self) -> Option<SettingsMutationOperation> {
        let pending = self.queued_mutations.pop_front()?;
        let operation = pending.operation.clone();
        self.pending_mutation = Some(pending);
        self.state = SettingsApplicationState::Ready;
        Some(operation)
    }

    fn finish_mutation(
        &mut self,
        completed: Option<BehaviorSetting>,
        diagnostic: Option<String>,
    ) -> Option<SettingsTransition> {
        for queued in &mut self.queued_mutations {
            if completed.is_none_or(|setting| setting == queued.operation.setting()) {
                queued.previous_row = self.presentation.row(queued.operation.setting())?.clone();
            }
        }
        let operation = self.start_queued_mutation();
        // A completion must not replace a newer submitted value in the editor.
        for queued in operation.iter().chain(
            self.queued_mutations
                .iter()
                .map(|pending| &pending.operation),
        ) {
            self.presentation.set_row_state(
                queued.setting(),
                SettingsEditStatus::Saving,
                None,
                false,
            );
        }
        Some(self.transition(None, operation, diagnostic))
    }

    fn sync_update_install(&mut self) {
        let channel = self
            .presentation
            .row(BehaviorSetting::UpdatesChannel)
            .and_then(|row| match row.editor() {
                SettingsEditor::Choice {
                    options,
                    selected: Some(selected),
                } => options
                    .get(*selected)
                    .and_then(|choice| match choice.value() {
                        "stable" => Some(crate::updates::UpdateChannel::Stable),
                        "prerelease" => Some(crate::updates::UpdateChannel::Prerelease),
                        _ => None,
                    }),
                _ => None,
            });
        let available = self.controls_available
            && self.pending_mutation.is_none()
            && self.queued_mutations.is_empty()
            && self.reconcile_mutation.is_none()
            && self.pending_update_check.is_none()
            && matches!(self.state, SettingsApplicationState::Ready);
        self.update_install.refresh_offer(
            self.presentation.update_check().result(),
            channel,
            available,
        );
        *self.presentation.update_install_mut() = self.update_install.presentation().clone();
        self.presentation
            .set_controls_available(self.controls_available && !self.update_install.active());
        self.presentation
            .update_check_mut()
            .set_install_active(self.update_install.active());
    }

    fn transition(
        &mut self,
        read_operation: Option<SettingsReadOperation>,
        mutation_operation: Option<SettingsMutationOperation>,
        diagnostic: Option<String>,
    ) -> SettingsTransition {
        self.sync_update_install();
        SettingsTransition {
            presentation: self.presentation.clone(),
            read_operation,
            mutation_operation,
            update_check_operation: None,
            update_install_operation: None,
            update_notice: None,
            diagnostic,
        }
    }
}

fn update_check_notice(
    result: &Result<UpdateCheckReport, UpdateCheckError>,
) -> Option<UpdateNotice> {
    match result {
        Ok(report) => {
            if report.available_release.is_none() {
                Some(UpdateNotice::new(
                    "Already up to date",
                    report.warning.as_deref().and_then(sanitized_notice_detail),
                ))
            } else {
                report.warning.as_deref().map(|warning| {
                    UpdateNotice::new("Update check warning", sanitized_notice_detail(warning))
                })
            }
        }
        Err(error) => Some(UpdateNotice::new(
            error.presentation.summary(),
            notice_details(&error.presentation, &error.diagnostic),
        )),
    }
}

fn sanitized_notice_detail(detail: &str) -> Option<String> {
    let detail = crate::update_flow::retained_failure_details(detail);
    (!detail.is_empty()).then_some(detail)
}

fn notice_details(error: &UserFacingError, diagnostic: &str) -> Option<String> {
    let diagnostic = crate::update_flow::retained_failure_details(diagnostic);
    match (error.detail().is_empty(), diagnostic.is_empty()) {
        (true, true) => None,
        (false, true) => Some(error.detail().to_string()),
        (true, false) => Some(diagnostic),
        (false, false) => Some(format!("{}\n\n{}", error.detail(), diagnostic)),
    }
}

fn mutation_failure_message(failure: &SettingsMutationFailure) -> String {
    match failure {
        SettingsMutationFailure::Validation(error) => match error {
            SettingsError::InvalidValue { .. } => {
                "That value is not valid. Choose one of the accepted values.".to_string()
            }
            _ => "That change is not valid for this setting.".to_string(),
        },
        SettingsMutationFailure::Persistence(_) => {
            "LG Buddy could not save this setting. Your previous value was kept.".to_string()
        }
    }
}

fn apply_feedback(outcome: &SettingsApplyOutcome) -> Option<SettingsFeedback> {
    let message = match outcome {
        SettingsApplyOutcome::NotInstalled { service } => format!(
            "Saved; {} is not installed yet.",
            apply_target_label(service)
        ),
        SettingsApplyOutcome::InactiveDisabled { service } => format!(
            "Saved; {} is inactive and disabled. It will apply when started.",
            apply_target_label(service)
        ),
        SettingsApplyOutcome::Skipped { .. } => {
            "Saved; runtime apply was skipped by configuration.".to_string()
        }
        SettingsApplyOutcome::NoActionRequired
        | SettingsApplyOutcome::Enabled { .. }
        | SettingsApplyOutcome::Restarted { .. }
        | SettingsApplyOutcome::EnabledStarted { .. }
        | SettingsApplyOutcome::DisabledStopped { .. } => return None,
    };
    Some(SettingsFeedback::new(
        SettingsFeedbackSeverity::Warning,
        message,
    ))
}

fn apply_target_label(service: &str) -> &'static str {
    if service.contains("screen") {
        "the screen integration"
    } else if service.contains("update_check") {
        "automatic update checks"
    } else {
        "the related service"
    }
}

fn user_facing_read_error(failure: SettingsReadFailure) -> UserFacingError {
    match failure {
        SettingsReadFailure::Stopped => UserFacingError::new(
            "Settings loading stopped",
            "The current settings could not be loaded. Retry to try again.",
        ),
        SettingsReadFailure::Unreadable => UserFacingError::new(
            "LG Buddy could not read its settings",
            "Check the settings configuration, then retry.",
        ),
        SettingsReadFailure::Internal => UserFacingError::new(
            "LG Buddy could not load settings",
            "Retry. If this continues, check the LG Buddy logs.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::presentation::settings::{
        SettingsEditStatus, SettingsFeedback, SettingsFeedbackSeverity, SettingsRow, SettingsStatus,
    };
    use crate::settings::{ConfigEnvReader, SettingValue, SettingsStore, SETTINGS_REGISTRY};

    fn groups(contents: &str) -> Vec<SettingsGroup> {
        let store = ConfigEnvReader::parse("/tmp/config.env", contents).into_store();
        SettingsPresentation::from_store(&store).groups().to_vec()
    }

    fn row<'a>(groups: &'a [SettingsGroup], group: &str, title: &str) -> &'a SettingsRow {
        groups
            .iter()
            .find(|item| item.title() == group)
            .unwrap()
            .rows()
            .iter()
            .find(|item| item.title() == title)
            .unwrap()
    }

    fn rows(groups: &[SettingsGroup]) -> Vec<&SettingsRow> {
        groups.iter().flat_map(|group| group.rows()).collect()
    }

    fn test_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "lg-buddy-settings-view-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn default_settings_are_ready_and_grouped() {
        let groups = groups("");
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].rows().len(), 4);
        assert_eq!(groups[1].rows().len(), 1);
        assert_eq!(groups[2].rows().len(), 2);

        let backend = row(&groups, "Screen", "Desktop integration");
        assert_eq!(backend.value_label(), "Automatic");
        assert_eq!(backend.source_label(), "Default");
        assert_eq!(backend.default_label(), "Automatic");
        assert!(backend.problem().is_none());
    }

    #[test]
    fn idle_controls_are_hidden_only_for_explicitly_disabled_blanking() {
        for (config, visible) in [
            ("", true),
            ("screen_idle_blank=enabled\n", true),
            ("screen_idle_blank=disabled\n", false),
            ("screen_idle_blank=invalid\n", true),
        ] {
            let presentation = SettingsPresentation::ready(groups(config));
            for setting in [
                BehaviorSetting::ScreenBackend,
                BehaviorSetting::ScreenIdleTimeout,
            ] {
                assert_eq!(presentation.row_visible(setting), visible, "{config}");
            }
            assert!(presentation.row_visible(BehaviorSetting::ScreenIdleBlank));
            assert!(presentation.row_visible(BehaviorSetting::ScreenRestorePolicy));
        }
    }

    fn update_report(channel: crate::updates::UpdateChannel) -> UpdateCheckReport {
        UpdateCheckReport {
            installed_version: "1.6.0".into(),
            channel,
            available_release: Some(AvailableUpdate {
                version: "1.7.0".into(),
                url: "https://github.com/Staphylococcus/LG_Buddy/releases/tag/v1.7.0".into(),
            }),
            warning: None,
        }
    }

    #[test]
    fn manual_check_is_independent_of_auto_checks_edits_and_refreshes() {
        use crate::updates::UpdateChannel;
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(
            opening.read_operation().unwrap(),
            Ok(groups(
                "updates_auto_check=disabled\nupdates_channel=stable\n",
            )),
        )
        .unwrap();
        let checking = app.handle_intent(SettingsIntent::CheckForUpdates).unwrap();
        let operation = checking.update_check_operation().unwrap();
        assert!(checking.presentation().update_check().checking());
        assert!(!checking
            .presentation()
            .update_check()
            .check_action()
            .enabled());
        assert!(checking.mutation_operation().is_none());
        assert!(!app.is_mutating());
        assert!(app.handle_intent(SettingsIntent::CheckForUpdates).is_none());

        // A refresh can observe a newer saved channel while discovery is in flight.
        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        app.complete_read(
            refresh.read_operation().unwrap(),
            Ok(groups(
                "updates_auto_check=disabled\nupdates_channel=prerelease\n",
            )),
        )
        .unwrap();
        assert!(app.presentation().update_check().checking());
        let completed = app
            .complete_update_check(operation, Ok(update_report(UpdateChannel::Stable)))
            .unwrap();
        let result = completed.presentation().update_check().result().unwrap();
        assert_eq!(result.channel, UpdateChannel::Stable);
        assert!(result.description().contains("stable channel"));
        assert_eq!(
            completed
                .presentation()
                .row(BehaviorSetting::UpdatesChannel)
                .unwrap()
                .value_label(),
            "Prerelease"
        );
        assert_eq!(
            completed
                .presentation()
                .row(BehaviorSetting::UpdatesAutoCheck)
                .unwrap()
                .value_label(),
            "Disabled"
        );

        let previous = completed.presentation().update_check().clone();
        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let edit = app
            .handle_intent(SettingsIntent::Commit {
                setting: BehaviorSetting::UpdatesChannel,
                value: "stable".into(),
            })
            .unwrap();
        assert_eq!(edit.presentation().update_check(), &previous);
        assert!(app
            .complete_read(refresh.read_operation().unwrap(), Ok(groups("")))
            .is_none());
        assert!(app
            .complete_update_check(operation, Ok(update_report(UpdateChannel::Prerelease)))
            .is_none());
    }

    #[test]
    fn retry_retains_last_channel_result_and_ignores_stale_or_closed_checks() {
        use crate::updates::UpdateChannel;
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let first = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        app.complete_update_check(first, Ok(update_report(UpdateChannel::Stable)))
            .unwrap();
        let second = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        assert!(app.presentation().update_check().result().is_some());
        assert!(app
            .complete_update_check(first, Err(UpdateCheckError::stopped()))
            .is_none());
        let failed = app
            .complete_update_check(second, Err(UpdateCheckError::stopped()))
            .unwrap();
        assert!(failed.presentation().update_check().error().is_some());
        assert!(failed
            .presentation()
            .update_check()
            .check_action()
            .enabled());
        assert_eq!(
            failed
                .presentation()
                .update_check()
                .result()
                .unwrap()
                .channel,
            UpdateChannel::Stable
        );

        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        app.complete_read(
            refresh.read_operation().unwrap(),
            Err(SettingsReadError::unreadable("test")),
        )
        .unwrap();
        let retry = app.handle_intent(SettingsIntent::Retry).unwrap();
        app.complete_read(retry.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        assert!(app.presentation().update_check().error().is_some());
        let third = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        let mut current = update_report(UpdateChannel::Prerelease);
        current.available_release = None;
        current.warning = Some("Cache could not be saved.".into());
        let done = app
            .complete_update_check(third, Ok(current.clone()))
            .unwrap();
        assert_eq!(done.presentation().update_check().result(), Some(&current));
        assert!(done.presentation().update_check().error().is_none());
        assert_eq!(current.title(), "No newer release available");
        let last = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        app.shutdown();
        assert!(app
            .complete_update_check(last, Err(UpdateCheckError::stopped()))
            .is_none());
        assert!(app.handle_intent(SettingsIntent::CheckForUpdates).is_none());
    }

    #[test]
    fn persisted_values_use_friendly_labels_and_sources() {
        let groups = groups(
            "screen_backend=wayland\nscreen_idle_blank=disabled\nscreen_idle_timeout=450\nscreen_restore_policy=aggressive\nsystem_sleep_wake_policy=disabled\nupdates_auto_check=disabled\nupdates_channel=prerelease\n",
        );
        assert_eq!(
            row(&groups, "Screen", "Desktop integration").value_label(),
            "Wayland"
        );
        assert_eq!(
            row(&groups, "Screen", "Desktop integration").source_label(),
            "Saved configuration"
        );
        assert_eq!(
            row(&groups, "Screen", "Idle timeout").value_label(),
            "450 seconds"
        );
        assert_eq!(
            row(&groups, "Updates", "Update channel").value_label(),
            "Prerelease"
        );
    }

    #[test]
    fn aliases_are_canonicalized_and_explained_as_accepted_values() {
        let groups = groups("screen_restore_policy=marker_only\n");
        let restore = row(&groups, "Screen", "Restore policy");
        assert_eq!(restore.value_label(), "Conservative");
        assert_eq!(restore.accepted_values_label(), "Conservative, Aggressive");
        assert!(restore.problem().is_none());
    }

    #[test]
    fn invalid_values_are_visible_and_never_replaced_by_defaults() {
        let groups = groups("screen_backend=not-a-backend\n");
        let backend = row(&groups, "Screen", "Desktop integration");
        assert_eq!(backend.value_label(), "Invalid value");
        assert_eq!(backend.source_label(), "Invalid configuration");
        assert_eq!(
            backend.problem(),
            Some(
                "Invalid configured value \"not-a-backend\". Accepted values: Automatic, GNOME, Wayland, swayidle (deprecated)."
            )
        );
        assert_ne!(backend.value_label(), backend.default_label());
    }

    #[test]
    fn every_scoped_row_uses_registry_metadata_and_tv_keys_are_excluded() {
        let groups = groups(
            "tvs_primary_ip=192.0.2.10\n\
tvs_primary_mac=not-a-mac\n\
tvs_primary_input=HDMI_1\n\
tvs_primary_platform=not-a-platform\n\
screen_backend=wayland\n",
        );
        let expected = [
            (
                "screen.idle_blank",
                "Idle blanking",
                "Enabled",
                "Enabled, Disabled",
            ),
            (
                "screen.backend",
                "Desktop integration",
                "Automatic",
                "Automatic, GNOME, Wayland, swayidle (deprecated)",
            ),
            (
                "screen.idle_timeout",
                "Idle timeout",
                "300 seconds",
                "1–86400 seconds",
            ),
            (
                "screen.restore_policy",
                "Restore policy",
                "Conservative",
                "Conservative, Aggressive",
            ),
            (
                "system.sleep_wake_policy",
                "TV sleep & wake",
                "Enabled",
                "Enabled, Disabled",
            ),
            (
                "updates.auto_check",
                "Automatic update checks",
                "Enabled",
                "Enabled, Disabled",
            ),
            (
                "updates.channel",
                "Update channel",
                "Stable",
                "Stable, Prerelease",
            ),
        ];
        let rendered = rows(&groups);
        assert_eq!(rendered.len(), expected.len());
        assert_eq!(groups[0].title(), "Screen");
        assert_eq!(groups[1].title(), "Sleep & Wake");
        assert_eq!(groups[2].title(), "Updates");

        for ((key, title, default, accepted), rendered) in expected.iter().zip(rendered.iter()) {
            let definition = SETTINGS_REGISTRY.get_by_name(key).unwrap();
            assert_eq!(rendered.title(), *title);
            assert_eq!(rendered.description(), definition.description());
            assert_eq!(rendered.default_label(), *default);
            assert_eq!(rendered.accepted_values_label(), *accepted);
            assert!(definition.default_value().is_some());
            assert!(definition.fallback_storage_keys().is_empty());
        }

        assert!(rendered.iter().all(|row| {
            !matches!(
                row.title(),
                "TV address" | "TV MAC address" | "TV input" | "TV platform"
            )
        }));
        assert_eq!(
            SETTINGS_REGISTRY
                .get_by_name("screen.backend")
                .unwrap()
                .default_value(),
            Some(SettingValue::Enum("auto"))
        );
    }

    #[test]
    fn missing_values_use_defaults_and_legacy_aliases_remain_valid_without_legacy_keys() {
        let default_groups = groups("");
        assert!(rows(&default_groups)
            .iter()
            .all(|row| row.source_label() == "Default" && row.problem().is_none()));

        let alias_groups = groups("screen_restore_policy=marker_only\n");
        let restore = row(&alias_groups, "Screen", "Restore policy");
        assert_eq!(restore.value_label(), "Conservative");
        assert_eq!(restore.source_label(), "Saved configuration");
        assert_eq!(restore.accepted_values_label(), "Conservative, Aggressive");
    }

    #[test]
    fn application_suppresses_duplicate_reads_and_retries_failed_reads() {
        let (mut app, opening) = SettingsApplication::open();
        let first = opening.read_operation().unwrap();
        assert!(matches!(
            opening.presentation().status(),
            SettingsStatus::Loading { .. }
        ));
        assert!(app.handle_intent(SettingsIntent::Retry).is_none());
        assert!(app.handle_intent(SettingsIntent::Refresh).is_none());

        let failed = app
            .complete_read(first, Err(SettingsReadError::stopped()))
            .unwrap();
        assert!(matches!(
            failed.presentation().status(),
            SettingsStatus::Failed(_)
        ));
        assert_eq!(
            failed.presentation().retry_action().unwrap().intent(),
            SettingsIntent::Retry
        );

        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let refreshed = refresh.read_operation().unwrap();
        assert!(app.handle_intent(SettingsIntent::Refresh).is_none());
        assert!(app.handle_intent(SettingsIntent::Retry).is_none());
        assert!(app.complete_read(first, Ok(groups(""))).is_none());
        let refreshed_ready = app
            .complete_read(refreshed, Ok(groups("updates_channel=prerelease\n")))
            .unwrap();
        assert_eq!(
            row(
                refreshed_ready.presentation().groups(),
                "Updates",
                "Update channel"
            )
            .value_label(),
            "Prerelease"
        );
        assert!(matches!(
            refreshed_ready.presentation().status(),
            SettingsStatus::Ready
        ));

        let retry = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let second = retry.read_operation().unwrap();
        let ready = app.complete_read(second, Ok(groups(""))).unwrap();
        assert!(matches!(
            ready.presentation().status(),
            SettingsStatus::Ready
        ));
        assert_eq!(
            row(ready.presentation().groups(), "Updates", "Update channel").value_label(),
            "Stable"
        );
    }

    #[test]
    fn failed_refresh_keeps_hidden_apply_warning_for_retry() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        app.presentation.set_row_state(
            BehaviorSetting::ScreenIdleBlank,
            SettingsEditStatus::ApplyFailed,
            Some(SettingsFeedback::new(
                SettingsFeedbackSeverity::Warning,
                "Saved, but could not apply yet. Retry apply.",
            )),
            true,
        );

        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let failed = app
            .complete_read(
                refresh.read_operation().unwrap(),
                Err(SettingsReadError::unreadable("temporary read failure")),
            )
            .unwrap();
        assert!(matches!(
            failed.presentation().status(),
            SettingsStatus::Failed(_)
        ));
        assert_eq!(failed.presentation().groups().len(), 3);
        assert_eq!(
            failed
                .presentation()
                .row(BehaviorSetting::ScreenIdleBlank)
                .unwrap()
                .edit_status(),
            SettingsEditStatus::ApplyFailed
        );
        assert!(failed
            .presentation()
            .row(BehaviorSetting::ScreenIdleBlank)
            .unwrap()
            .retry_apply_action()
            .is_some());

        let retry = app.handle_intent(SettingsIntent::Retry).unwrap();
        let ready = app
            .complete_read(retry.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let row = ready
            .presentation()
            .row(BehaviorSetting::ScreenIdleBlank)
            .unwrap();
        assert_eq!(row.edit_status(), SettingsEditStatus::ApplyFailed);
        assert!(row.retry_apply_action().is_some());
    }

    #[test]
    fn mutation_keeps_controls_available_and_worker_stop_is_recoverable() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();

        let transition = app
            .handle_intent(SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenIdleBlank,
                value: "disabled".to_string(),
            })
            .unwrap();
        let operation = transition.mutation_operation().unwrap().clone();
        assert!(app.is_mutating());
        assert!(app
            .presentation()
            .groups()
            .iter()
            .flat_map(|group| group.rows())
            .all(|row| row.editor_enabled()));
        assert_eq!(
            app.presentation()
                .row(BehaviorSetting::ScreenIdleBlank)
                .unwrap()
                .edit_status(),
            SettingsEditStatus::Saving
        );

        let stopped = app.mutation_worker_stopped(&operation).unwrap();
        assert!(stopped.read_operation().is_some());
        let reconciled = app
            .complete_read(stopped.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let row = reconciled
            .presentation()
            .row(BehaviorSetting::ScreenIdleBlank)
            .unwrap();
        assert_eq!(row.edit_status(), SettingsEditStatus::PersistenceFailed);
        assert_eq!(
            row.feedback().unwrap().severity(),
            SettingsFeedbackSeverity::Warning
        );
        assert!(row.feedback().unwrap().message().contains("unchanged"));
        assert!(row
            .retry_apply_action()
            .is_some_and(|action| action.enabled()));
        assert!(!app.is_mutating());
        assert!(app
            .handle_intent(SettingsIntent::RetryApply(BehaviorSetting::ScreenIdleBlank))
            .is_some());
    }

    #[test]
    fn rapid_edits_save_in_order_and_failures_restore_the_latest_saved_value() {
        let path = test_path("queued-edits");
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let first = app
            .handle_intent(SettingsIntent::Commit {
                setting: BehaviorSetting::UpdatesChannel,
                value: "prerelease".into(),
            })
            .unwrap();
        for (setting, value) in [
            (BehaviorSetting::SystemSleepWakePolicy, "disabled"),
            (BehaviorSetting::UpdatesChannel, "bad"),
        ] {
            let queued = app
                .handle_intent(SettingsIntent::Commit {
                    setting,
                    value: value.into(),
                })
                .unwrap();
            assert!(
                queued.mutation_operation().is_none(),
                "only one write runs at a time"
            );
            assert!(rows(queued.presentation().groups())
                .iter()
                .all(|row| row.editor_enabled()));
        }
        let mut operation = first.mutation_operation().unwrap().clone();
        // Neither of these settings invokes a system service.
        let applier = SettingsApplier::from_env();
        for (index, expected) in [
            BehaviorSetting::UpdatesChannel,
            BehaviorSetting::SystemSleepWakePolicy,
            BehaviorSetting::UpdatesChannel,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(operation.setting(), expected);
            let SettingsMutationRequest::Set(value) = operation.request() else {
                unreachable!()
            };
            let result = SettingsMutation::set(
                &SettingsStore::load(&path).unwrap(),
                operation.key_name(),
                value,
            )
            .map_err(SettingsMutationFailure::Validation)
            .and_then(|mutation| execute_settings_mutation(&path, mutation, &applier, &mut |_| {}));
            let done = app.complete_mutation(&operation, result).unwrap();
            assert!(app
                .complete_mutation(
                    &operation,
                    Err(SettingsMutationFailure::Persistence(SettingsError::Apply {
                        message: "stale".into()
                    }))
                )
                .is_none());
            let channel = done
                .presentation()
                .row(BehaviorSetting::UpdatesChannel)
                .unwrap();
            assert_eq!(channel.value_label(), "Prerelease");
            if index < 2 {
                assert_eq!(
                    channel.edit_status(),
                    SettingsEditStatus::Saving,
                    "an older result must not replace the queued draft"
                );
                operation = done.mutation_operation().unwrap().clone();
            } else {
                assert!(done.mutation_operation().is_none());
                assert_eq!(channel.edit_status(), SettingsEditStatus::ValidationFailed);
                assert!(channel.feedback().is_some());
            }
        }
        assert!(!app.is_mutating());
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("system_sleep_wake_policy=disabled"));
        assert!(saved.contains("updates_channel=prerelease"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn queued_edits_resume_after_a_stopped_worker_is_reconciled() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let first = app
            .handle_intent(SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenIdleTimeout,
                value: "600".into(),
            })
            .unwrap();
        app.handle_intent(SettingsIntent::Commit {
            setting: BehaviorSetting::UpdatesChannel,
            value: "prerelease".into(),
        })
        .unwrap();
        let stopped = app
            .mutation_worker_stopped(first.mutation_operation().unwrap())
            .unwrap();
        assert!(
            app.is_mutating(),
            "queued writes still exclude TV configuration changes"
        );
        let ready = app
            .complete_read(
                stopped.read_operation().unwrap(),
                Ok(groups("screen_idle_timeout=600\n")),
            )
            .unwrap();
        assert_eq!(
            ready.mutation_operation().unwrap().setting(),
            BehaviorSetting::UpdatesChannel
        );
        assert!(ready
            .presentation()
            .row(BehaviorSetting::ScreenIdleTimeout)
            .unwrap()
            .retry_apply_action()
            .is_some());
    }

    #[test]
    fn closing_drains_already_accepted_edits_without_accepting_new_ones() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let intent = SettingsIntent::Commit {
            setting: BehaviorSetting::UpdatesChannel,
            value: "prerelease".into(),
        };
        let first = app.handle_intent(intent.clone()).unwrap();
        app.handle_intent(intent.clone()).unwrap();
        app.shutdown();
        assert!(app.handle_intent(intent).is_none());
        let second = app
            .mutation_worker_stopped(first.mutation_operation().unwrap())
            .unwrap();
        assert!(second.read_operation().is_none());
        assert!(app
            .mutation_worker_stopped(second.mutation_operation().unwrap())
            .is_none());
        assert!(!app.is_mutating());
    }

    #[test]
    fn set_enabled_intent_uses_canonical_toggle_values() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let transition = app
            .handle_intent(SettingsIntent::SetEnabled {
                setting: BehaviorSetting::UpdatesAutoCheck,
                enabled: false,
            })
            .unwrap();
        assert_eq!(
            transition.mutation_operation().unwrap().request(),
            &SettingsMutationRequest::Set("disabled".to_string())
        );
        assert!(app
            .handle_intent(SettingsIntent::SetEnabled {
                setting: BehaviorSetting::ScreenIdleTimeout,
                enabled: true,
            })
            .is_none());
    }

    #[test]
    fn failed_apply_warning_survives_unchanged_refresh_but_not_changed_value() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        app.presentation.set_row_state(
            BehaviorSetting::ScreenIdleBlank,
            SettingsEditStatus::ApplyFailed,
            Some(SettingsFeedback::new(
                SettingsFeedbackSeverity::Warning,
                "Saved, but could not apply yet. Retry apply.",
            )),
            true,
        );

        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        assert_eq!(refresh.presentation().groups().len(), 3);
        assert_eq!(
            refresh
                .presentation()
                .row(BehaviorSetting::ScreenIdleBlank)
                .unwrap()
                .edit_status(),
            SettingsEditStatus::ApplyFailed
        );
        let operation = refresh.read_operation().unwrap();
        let ready = app.complete_read(operation, Ok(groups(""))).unwrap();
        let row = ready
            .presentation()
            .row(BehaviorSetting::ScreenIdleBlank)
            .unwrap();
        assert_eq!(row.edit_status(), SettingsEditStatus::ApplyFailed);
        assert!(row.retry_apply_action().is_some());

        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let changed = app
            .complete_read(
                refresh.read_operation().unwrap(),
                Ok(groups("screen_idle_blank=disabled\n")),
            )
            .unwrap();
        let row = changed
            .presentation()
            .row(BehaviorSetting::ScreenIdleBlank)
            .unwrap();
        assert_eq!(row.value_label(), "Disabled");
        assert_eq!(row.edit_status(), SettingsEditStatus::Unchanged);
        assert!(row.retry_apply_action().is_none());
    }

    #[test]
    fn validation_failure_restores_previous_row_without_claiming_a_save() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let transition = app
            .handle_intent(SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenIdleTimeout,
                value: "bad".to_string(),
            })
            .unwrap();
        let operation = transition.mutation_operation().unwrap().clone();
        let transition = app
            .complete_mutation(
                &operation,
                Err(SettingsMutationFailure::Validation(
                    SettingsError::InvalidValue {
                        key: "screen.idle_timeout".to_string(),
                        value: "bad".to_string(),
                        expected: "1–86400 seconds".to_string(),
                    },
                )),
            )
            .unwrap();
        let row = transition
            .presentation()
            .row(BehaviorSetting::ScreenIdleTimeout)
            .unwrap();
        assert_eq!(row.value_label(), "300 seconds");
        assert_eq!(row.edit_status(), SettingsEditStatus::ValidationFailed);
        assert!(row.feedback().unwrap().message().contains("not valid"));
    }

    #[test]
    fn failed_new_edit_keeps_retry_apply_for_prior_saved_warning() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        app.presentation.set_row_state(
            BehaviorSetting::ScreenIdleBlank,
            SettingsEditStatus::ApplyFailed,
            Some(SettingsFeedback::new(
                SettingsFeedbackSeverity::Warning,
                "Saved, but could not apply yet. Retry apply.",
            )),
            true,
        );
        let transition = app
            .handle_intent(SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenIdleBlank,
                value: "disabled".to_string(),
            })
            .unwrap();
        let operation = transition.mutation_operation().unwrap().clone();
        assert!(transition
            .presentation()
            .row_visible(BehaviorSetting::ScreenIdleTimeout));
        let transition = app
            .complete_mutation(
                &operation,
                Err(SettingsMutationFailure::Persistence(SettingsError::Apply {
                    message: "write failed".to_string(),
                })),
            )
            .unwrap();
        let row = transition
            .presentation()
            .row(BehaviorSetting::ScreenIdleBlank)
            .unwrap();
        assert_eq!(row.edit_status(), SettingsEditStatus::PersistenceFailed);
        assert!(transition
            .presentation()
            .row_visible(BehaviorSetting::ScreenIdleTimeout));
        assert!(row.retry_apply_action().is_some());
        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let refreshed = app
            .complete_read(refresh.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        assert!(
            refreshed
                .presentation()
                .row(BehaviorSetting::ScreenIdleBlank)
                .unwrap()
                .retry_apply_action()
                .is_some(),
            "navigation must retain the pending runtime retry after a failed later edit"
        );
    }

    #[test]
    fn apply_outcomes_do_not_claim_runtime_application_when_deferred() {
        for (outcome, detail) in [
            (
                SettingsApplyOutcome::NotInstalled {
                    service: "lg-buddy-screen.service",
                },
                "not installed",
            ),
            (
                SettingsApplyOutcome::InactiveDisabled {
                    service: "lg-buddy-screen.service",
                },
                "inactive and disabled",
            ),
            (
                SettingsApplyOutcome::Skipped {
                    reason: "test".into(),
                },
                "skipped",
            ),
        ] {
            let feedback = apply_feedback(&outcome).expect("incomplete apply needs feedback");
            assert_eq!(feedback.severity(), SettingsFeedbackSeverity::Warning);
            assert!(feedback.message().contains(detail));
        }
    }

    #[test]
    fn successful_apply_outcomes_are_silent() {
        for outcome in [
            SettingsApplyOutcome::NoActionRequired,
            SettingsApplyOutcome::Enabled {
                unit: "LG_Buddy_update_check.timer",
            },
            SettingsApplyOutcome::Restarted {
                service: "LG_Buddy_screen.service",
            },
            SettingsApplyOutcome::EnabledStarted {
                unit: "LG_Buddy_update_check.timer",
            },
            SettingsApplyOutcome::DisabledStopped {
                unit: "LG_Buddy_update_check.timer",
            },
        ] {
            assert!(
                apply_feedback(&outcome).is_none(),
                "unexpected feedback for {outcome:?}"
            );
        }
    }

    #[test]
    fn shutdown_rejects_late_mutation_result() {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups("")))
            .unwrap();
        let transition = app
            .handle_intent(SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenIdleBlank,
                value: "disabled".to_string(),
            })
            .unwrap();
        let operation = transition.mutation_operation().unwrap().clone();
        app.shutdown();
        assert!(app.mutation_worker_stopped(&operation).is_none());
    }

    #[test]
    fn shutdown_rejects_late_read_results() {
        let (mut app, opening) = SettingsApplication::open();
        let operation = opening.read_operation().unwrap();
        app.shutdown();
        assert!(app
            .complete_read(operation, Ok(groups("updates_channel=prerelease\n")))
            .is_none());
        assert!(matches!(
            app.presentation().status(),
            SettingsStatus::Loading { .. }
        ));
    }

    #[test]
    fn unreadable_settings_path_reaches_failed_presentation() {
        let path = test_path("unreadable");
        fs::create_dir(&path).unwrap();
        let read_error = SettingsStore::load(&path).unwrap_err();
        let diagnostic = read_error.to_string();
        let (mut app, opening) = SettingsApplication::open();
        let failed = app
            .complete_read(
                opening.read_operation().unwrap(),
                Err(SettingsReadError::unreadable(diagnostic.clone())),
            )
            .unwrap();
        assert_eq!(failed.diagnostic(), Some(diagnostic.as_str()));
        assert!(matches!(
            failed.presentation().status(),
            SettingsStatus::Failed(_)
        ));
        let error = match failed.presentation().status() {
            SettingsStatus::Failed(error) => error,
            _ => unreachable!(),
        };
        assert_eq!(error.summary(), "LG Buddy could not read its settings");
        assert!(!error.detail().contains(path.to_str().unwrap()));
        fs::remove_dir(&path).unwrap();
    }
}
