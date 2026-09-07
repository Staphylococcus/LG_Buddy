//! Toolkit-independent coordination for the read-only Settings view.

use std::error::Error;
use std::fmt;

use crate::presentation::brightness::UserFacingError;
use crate::presentation::settings::{SettingsGroup, SettingsPresentation};
use crate::settings::SettingsStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsIntent {
    Retry,
    Refresh,
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
    diagnostic: Option<String>,
}

impl SettingsTransition {
    pub fn presentation(&self) -> &SettingsPresentation {
        &self.presentation
    }

    pub fn read_operation(&self) -> Option<SettingsReadOperation> {
        self.read_operation
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
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentSettingsBackend;

impl SettingsBackend for EnvironmentSettingsBackend {
    fn read_settings(&self) -> Result<Vec<SettingsGroup>, SettingsReadError> {
        let store = SettingsStore::load_from_env()
            .map_err(|error| SettingsReadError::unreadable(error.to_string()))?;
        Ok(SettingsPresentation::from_store(&store).groups().to_vec())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SettingsApplicationState {
    Loading(SettingsReadOperation),
    Ready,
    Failed,
    Closed,
}

#[derive(Debug)]
pub struct SettingsApplication {
    state: SettingsApplicationState,
    next_operation_id: u64,
    presentation: SettingsPresentation,
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
            },
            SettingsTransition {
                presentation,
                read_operation: Some(operation),
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
                self.state = SettingsApplicationState::Ready;
                (SettingsPresentation::ready(groups), None)
            }
            Err(error) => {
                self.state = SettingsApplicationState::Failed;
                let diagnostic = error.diagnostic().to_string();
                (
                    SettingsPresentation::failed(user_facing_read_error(error.failure())),
                    Some(diagnostic),
                )
            }
        };
        self.presentation = presentation.clone();
        Some(SettingsTransition {
            presentation,
            read_operation: None,
            diagnostic,
        })
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
        self.presentation = SettingsPresentation::loading();
        SettingsTransition {
            presentation: self.presentation.clone(),
            read_operation: Some(operation),
            diagnostic: None,
        }
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
    use crate::presentation::settings::{SettingsRow, SettingsStatus};
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
