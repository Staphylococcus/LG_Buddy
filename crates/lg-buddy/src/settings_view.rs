//! Toolkit-independent coordination for the read-only Settings view.

use std::error::Error;
use std::fmt;

use crate::presentation::brightness::UserFacingError;
use crate::presentation::settings::{
    SettingsEditStatus, SettingsEditor, SettingsFeedback, SettingsFeedbackSeverity, SettingsGroup,
    SettingsPresentation, SettingsRow,
};
use crate::settings::{
    execute_settings_mutation, retry_settings_apply, ConfigPathResolver, SettingsApplier,
    SettingsApplyOutcome, SettingsError, SettingsMutation, SettingsMutationFailure,
    SettingsMutationOutcome, SettingsMutationStage, SettingsStore,
};

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

    fn write_setting(
        &self,
        operation: SettingsMutationOperation,
        progress: &mut dyn FnMut(SettingsMutationStage),
    ) -> Result<SettingsMutationOutcome, SettingsMutationFailure>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentSettingsBackend;

impl SettingsBackend for EnvironmentSettingsBackend {
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
    Mutating(SettingsMutationOperation),
    Closed,
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
    pending_mutation: Option<PendingMutation>,
    reconcile_mutation: Option<PendingMutation>,
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
                pending_mutation: None,
                reconcile_mutation: None,
            },
            SettingsTransition {
                presentation,
                read_operation: Some(operation),
                mutation_operation: None,
                diagnostic: None,
            },
        )
    }

    pub fn handle_intent(&mut self, intent: SettingsIntent) -> Option<SettingsTransition> {
        match intent {
            SettingsIntent::Retry if matches!(self.state, SettingsApplicationState::Failed) => {
                Some(self.begin_read())
            }
            SettingsIntent::Refresh
                if matches!(
                    self.state,
                    SettingsApplicationState::Ready | SettingsApplicationState::Failed
                ) =>
            {
                Some(self.begin_read())
            }
            SettingsIntent::SetEnabled { setting, enabled }
                if self.controls_available
                    && matches!(self.state, SettingsApplicationState::Ready) =>
            {
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
            SettingsIntent::Commit { setting, value }
                if self.controls_available
                    && matches!(self.state, SettingsApplicationState::Ready) =>
            {
                self.begin_mutation(setting, SettingsMutationRequest::Set(value))
            }
            SettingsIntent::Reset(setting)
                if self.controls_available
                    && matches!(self.state, SettingsApplicationState::Ready) =>
            {
                self.begin_mutation(setting, SettingsMutationRequest::Reset)
            }
            SettingsIntent::RetryApply(setting)
                if self.controls_available
                    && matches!(self.state, SettingsApplicationState::Ready) =>
            {
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
        self.presentation = presentation.clone();
        Some(SettingsTransition {
            presentation,
            read_operation: None,
            mutation_operation: None,
            diagnostic,
        })
    }

    pub fn mutation_progress(
        &mut self,
        operation: &SettingsMutationOperation,
        stage: SettingsMutationStage,
    ) -> Option<SettingsTransition> {
        if !matches!(
            &self.state,
            SettingsApplicationState::Mutating(active) if active == operation
        ) {
            return None;
        }

        let status = match stage {
            SettingsMutationStage::Validating => SettingsEditStatus::Validating,
            SettingsMutationStage::Persisting => SettingsEditStatus::Persisting,
            SettingsMutationStage::Persisted => SettingsEditStatus::Persisted,
            SettingsMutationStage::Applying => SettingsEditStatus::Applying,
        };
        self.presentation
            .set_row_state(operation.setting(), status, None, false);
        Some(self.transition(None, None, None))
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
        if !matches!(
            &self.state,
            SettingsApplicationState::Mutating(active) if active == operation
        ) {
            return None;
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
                Some(self.transition(None, None, diagnostic))
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
                Some(self.transition(None, None, Some(failure.error().to_string())))
            }
        }
    }

    pub fn mutation_worker_stopped(
        &mut self,
        operation: &SettingsMutationOperation,
    ) -> Option<SettingsTransition> {
        let pending = match &self.pending_mutation {
            Some(pending) if pending.operation == *operation => pending.clone(),
            _ => return None,
        };
        if !matches!(
            &self.state,
            SettingsApplicationState::Mutating(active) if active == operation
        ) {
            return None;
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
        Some(self.begin_read())
    }

    pub fn is_mutating(&self) -> bool {
        self.pending_mutation.is_some()
    }

    pub fn set_controls_available(&mut self, available: bool) -> Option<SettingsTransition> {
        if self.controls_available == available {
            return None;
        }
        self.controls_available = available;
        self.presentation
            .set_controls_available(available && !self.is_mutating());
        Some(self.transition(None, None, None))
    }

    pub fn shutdown(&mut self) {
        self.state = SettingsApplicationState::Closed;
    }

    pub fn presentation(&self) -> &SettingsPresentation {
        &self.presentation
    }

    fn begin_read(&mut self) -> SettingsTransition {
        let operation = SettingsReadOperation::new(self.next_operation_id);
        self.next_operation_id += 1;
        self.state = SettingsApplicationState::Loading(operation);
        self.presentation.mark_loading();
        SettingsTransition {
            presentation: self.presentation.clone(),
            read_operation: Some(operation),
            mutation_operation: None,
            diagnostic: None,
        }
    }

    fn begin_mutation(
        &mut self,
        setting: BehaviorSetting,
        request: SettingsMutationRequest,
    ) -> Option<SettingsTransition> {
        let previous_row = self.presentation.row(setting)?.clone();
        let operation = SettingsMutationOperation::new(self.next_operation_id, setting, request);
        self.next_operation_id += 1;
        self.pending_mutation = Some(PendingMutation {
            operation: operation.clone(),
            previous_row,
        });
        self.state = SettingsApplicationState::Mutating(operation.clone());
        self.presentation
            .set_row_state(setting, SettingsEditStatus::Validating, None, false);
        self.presentation.set_controls_available(false);
        Some(self.transition(None, Some(operation), None))
    }

    fn transition(
        &self,
        read_operation: Option<SettingsReadOperation>,
        mutation_operation: Option<SettingsMutationOperation>,
        diagnostic: Option<String>,
    ) -> SettingsTransition {
        SettingsTransition {
            presentation: self.presentation.clone(),
            read_operation,
            mutation_operation,
            diagnostic,
        }
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
                "screen.backend",
                "Desktop integration",
                "Automatic",
                "Automatic, GNOME, Wayland, swayidle (deprecated)",
            ),
            (
                "screen.idle_blank",
                "Idle blanking",
                "Enabled",
                "Enabled, Disabled",
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
    fn mutation_suppresses_duplicates_and_worker_stop_is_recoverable() {
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
            .handle_intent(SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenIdleBlank,
                value: "enabled".to_string(),
            })
            .is_none());

        app.mutation_progress(&operation, SettingsMutationStage::Persisting)
            .unwrap();
        assert_eq!(
            app.presentation()
                .row(BehaviorSetting::ScreenIdleBlank)
                .unwrap()
                .edit_status(),
            SettingsEditStatus::Persisting
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
