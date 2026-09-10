//! Shared persistence and runtime application outcomes for CLI and GUI clients.

use std::path::Path;

use super::{
    activation::activate_before_persist, persist_settings_mutation, ServiceController,
    SettingsApplier, SettingsApplyOutcome, SettingsChange, SettingsError, SettingsMutation,
    SettingsStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsMutationStage {
    Validating,
    Persisting,
    Persisted,
    Applying,
}

/// Failures before publication. A runtime failure belongs to the successful
/// persistence outcome so clients cannot mistake a saved value for an unsaved one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsMutationFailure {
    Validation(SettingsError),
    Persistence(SettingsError),
    Activation(SettingsError),
}

impl SettingsMutationFailure {
    pub fn error(&self) -> &SettingsError {
        match self {
            Self::Validation(error) | Self::Persistence(error) | Self::Activation(error) => error,
        }
    }

    pub fn into_error(self) -> SettingsError {
        match self {
            Self::Validation(error) | Self::Persistence(error) | Self::Activation(error) => error,
        }
    }
}

/// Execute the GUI mutation path. Settings that turn on a service are
/// activated first so a successful publication cannot advertise a behavior
/// whose runtime service is unavailable. The CLI continues to use
/// [`execute_settings_mutation`] and retains its persist-then-apply contract.
pub(crate) fn execute_gui_settings_mutation<C: ServiceController>(
    path: &Path,
    mutation: SettingsMutation,
    applier: &SettingsApplier<C>,
    progress: &mut dyn FnMut(SettingsMutationStage),
) -> Result<SettingsMutationOutcome, SettingsMutationFailure> {
    progress(SettingsMutationStage::Validating);
    activate_before_persist(path, mutation, applier.service_controller())
        .map_err(SettingsMutationFailure::Activation)?;
    execute_settings_mutation(path, mutation, applier, progress)
}

#[derive(Debug, Clone)]
pub struct SettingsMutationOutcome {
    change: SettingsChange,
    apply: Result<SettingsApplyOutcome, SettingsError>,
}

impl SettingsMutationOutcome {
    pub fn change(&self) -> &SettingsChange {
        &self.change
    }

    pub fn apply(&self) -> &Result<SettingsApplyOutcome, SettingsError> {
        &self.apply
    }
}

/// Execute an already validated mutation using the same writer and service
/// operations for all frontends. Explicit unchanged commands still reapply,
/// preserving the CLI's existing recovery behavior.
pub fn execute_settings_mutation<C: ServiceController>(
    path: &Path,
    mutation: SettingsMutation,
    applier: &SettingsApplier<C>,
    progress: &mut dyn FnMut(SettingsMutationStage),
) -> Result<SettingsMutationOutcome, SettingsMutationFailure> {
    progress(SettingsMutationStage::Persisting);
    let change =
        persist_settings_mutation(path, mutation).map_err(SettingsMutationFailure::Persistence)?;
    progress(SettingsMutationStage::Persisted);
    Ok(apply_change(change, applier, progress))
}

/// Retry the current configuration without rewriting it or replaying an old
/// draft. This also preserves a default or legacy source when no write occurred.
pub fn retry_settings_apply<C: ServiceController>(
    store: &SettingsStore,
    key: &str,
    applier: &SettingsApplier<C>,
    progress: &mut dyn FnMut(SettingsMutationStage),
) -> Result<SettingsMutationOutcome, SettingsMutationFailure> {
    progress(SettingsMutationStage::Validating);
    let change =
        SettingsChange::for_apply(store, key).map_err(SettingsMutationFailure::Validation)?;
    Ok(apply_change(change, applier, progress))
}

fn apply_change<C: ServiceController>(
    change: SettingsChange,
    applier: &SettingsApplier<C>,
    progress: &mut dyn FnMut(SettingsMutationStage),
) -> SettingsMutationOutcome {
    progress(SettingsMutationStage::Applying);
    let apply = applier
        .apply(&change)
        .map_err(|error| SettingsError::ApplyAfterPersist {
            key: change.mutation().key_name().to_string(),
            path: change.path().to_path_buf(),
            message: error.to_string(),
        });
    SettingsMutationOutcome { change, apply }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::tests::{unique_test_path, FakeServiceController};
    use crate::settings::SettingsCommand;
    use crate::settings::{SettingSource, SettingsCommandRunner};

    #[test]
    fn idle_control_visibility_follows_persistence_even_when_runtime_apply_fails() {
        use crate::presentation::settings::SettingsPresentation;
        use crate::settings_view::{BehaviorSetting, SettingsApplication, SettingsIntent};

        let path = unique_test_path("idle-visibility");
        std::fs::write(&path, "screen_backend=wayland\nscreen_idle_timeout=600\n").unwrap();
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(
            opening.read_operation().unwrap(),
            Ok(
                SettingsPresentation::from_store(&SettingsStore::load(&path).unwrap())
                    .groups()
                    .to_vec(),
            ),
        )
        .unwrap();

        for enabled in [false, true] {
            let started = app
                .handle_intent(SettingsIntent::SetEnabled {
                    setting: BehaviorSetting::ScreenIdleBlank,
                    enabled,
                })
                .unwrap();
            let operation = started.mutation_operation().unwrap();
            let mutation = SettingsMutation::set(
                &SettingsStore::load(&path).unwrap(),
                "screen.idle_blank",
                if enabled { "enabled" } else { "disabled" },
            )
            .unwrap();
            let controller = if enabled {
                FakeServiceController::active_or_enabled()
            } else {
                FakeServiceController::failing_restart()
            };
            let result = execute_settings_mutation(
                &path,
                mutation,
                &SettingsApplier::new(controller),
                &mut |_| {},
            );
            let done = app.complete_mutation(operation, result).unwrap();
            let presentation = done.presentation();
            assert_eq!(
                presentation.row_visible(BehaviorSetting::ScreenBackend),
                enabled
            );
            assert_eq!(
                presentation.row_visible(BehaviorSetting::ScreenIdleTimeout),
                enabled
            );
            assert_eq!(
                presentation.row_visible(BehaviorSetting::ScreenHonorIdleInhibitors),
                enabled
            );
            assert!(presentation.row_visible(BehaviorSetting::ScreenRestorePolicy));
            let toggle = presentation.groups()[0]
                .rows()
                .iter()
                .find(|row| row.setting() == BehaviorSetting::ScreenIdleBlank)
                .unwrap();
            assert_eq!(toggle.retry_apply_action().is_some(), !enabled);
            let saved = std::fs::read_to_string(&path).unwrap();
            assert!(saved.contains("screen_backend=wayland\n"));
            assert!(saved.contains("screen_idle_timeout=600\n"));
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn shared_executor_and_cli_write_the_same_values_and_runtime_actions() {
        for (key, value, restarts, enables, disables) in [
            ("screen.backend", "gnome", 1, 0, 0),
            ("screen.idle_blank", "disabled", 1, 0, 0),
            ("screen.honor_idle_inhibitors", "enabled", 1, 0, 0),
            ("screen.idle_timeout", "600", 1, 0, 0),
            ("screen.restore_policy", "aggressive", 1, 0, 0),
            ("system.sleep_wake_policy", "disabled", 0, 0, 0),
            ("updates.auto_check", "disabled", 0, 0, 1),
            ("updates.channel", "prerelease", 0, 0, 0),
        ] {
            let path = unique_test_path("mutation-shared");
            let cli_path = unique_test_path("mutation-cli");
            let controller = FakeServiceController::active_or_enabled();
            let cli_controller = FakeServiceController::active_or_enabled();
            let store = SettingsStore::load(&path).unwrap();
            let mutation = SettingsMutation::set(&store, key, value).unwrap();
            let mut stages = Vec::new();
            let outcome = execute_settings_mutation(
                &path,
                mutation,
                &SettingsApplier::new(controller.clone()),
                &mut |stage| stages.push(stage),
            )
            .unwrap();
            assert!(outcome.apply().is_ok(), "{key}");
            assert_eq!(
                stages,
                [
                    SettingsMutationStage::Persisting,
                    SettingsMutationStage::Persisted,
                    SettingsMutationStage::Applying
                ]
            );
            SettingsCommandRunner::with_applier(
                SettingsStore::load(&cli_path).unwrap(),
                SettingsApplier::new(cli_controller.clone()),
            )
            .run(
                SettingsCommand::Set {
                    key: key.into(),
                    value: value.into(),
                },
                &mut Vec::new(),
            )
            .unwrap();
            assert_eq!(
                std::fs::read(&path).unwrap(),
                std::fs::read(&cli_path).unwrap()
            );
            for service in [&controller, &cli_controller] {
                assert_eq!(service.restarts.get(), restarts, "{key}");
                assert_eq!(service.enables.get(), enables, "{key}");
                assert_eq!(service.disables.get(), disables, "{key}");
            }
            std::fs::remove_file(path).unwrap();
            std::fs::remove_file(cli_path).unwrap();
        }
    }

    #[test]
    fn runtime_failure_keeps_saved_value_and_retry_does_not_rewrite_config() {
        let path = unique_test_path("mutation-retry");
        std::fs::write(&path, "# preserve me\nscreen_idle_timeout=broken\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let mutation = SettingsMutation::set(&store, "screen.idle_timeout", "600").unwrap();
        let outcome = execute_settings_mutation(
            &path,
            mutation,
            &SettingsApplier::new(FakeServiceController::failing_restart()),
            &mut |_| {},
        )
        .unwrap();
        assert!(matches!(
            outcome.apply(),
            Err(SettingsError::ApplyAfterPersist { .. })
        ));
        assert_eq!(
            outcome
                .change()
                .effective_setting()
                .value()
                .unwrap()
                .to_string(),
            "600"
        );
        let contents = std::fs::read(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let mut stages = Vec::new();
        let retry = retry_settings_apply(
            &store,
            "screen.idle_timeout",
            &SettingsApplier::new(FakeServiceController::active_or_enabled()),
            &mut |stage| stages.push(stage),
        )
        .unwrap();
        assert!(matches!(
            retry.apply(),
            Ok(SettingsApplyOutcome::Restarted { .. })
        ));
        assert!(!retry.change().file_changed());
        assert_eq!(
            stages,
            [
                SettingsMutationStage::Validating,
                SettingsMutationStage::Applying
            ]
        );
        assert_eq!(std::fs::read(&path).unwrap(), contents);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn persistence_failure_does_not_run_apply() {
        let path = unique_test_path("mutation-no-write");
        let store = SettingsStore::load(&path).unwrap();
        let mutation = SettingsMutation::set(&store, "screen.idle_timeout", "600").unwrap();
        std::fs::create_dir(&path).unwrap();
        let controller = FakeServiceController::active_or_enabled();
        let mut stages = Vec::new();
        assert!(matches!(
            execute_settings_mutation(
                &path,
                mutation,
                &SettingsApplier::new(controller.clone()),
                &mut |stage| stages.push(stage)
            ),
            Err(SettingsMutationFailure::Persistence(_))
        ));
        assert_eq!(controller.restarts.get(), 0);
        assert_eq!(stages, [SettingsMutationStage::Persisting]);
        std::fs::remove_dir(path).unwrap();
    }

    #[test]
    fn reset_removes_invalid_override_and_retry_preserves_default_source() {
        let path = unique_test_path("mutation-reset");
        std::fs::write(&path, "screen_idle_timeout=broken\n").unwrap();
        let store = SettingsStore::load(&path).unwrap();
        let mutation = SettingsMutation::unset(&store, "screen.idle_timeout").unwrap();
        let applier = SettingsApplier::new(FakeServiceController::missing());
        let reset = execute_settings_mutation(&path, mutation, &applier, &mut |_| {}).unwrap();
        assert_eq!(
            reset.change().effective_setting().source(),
            SettingSource::Default
        );
        assert!(matches!(
            reset.apply(),
            Ok(SettingsApplyOutcome::NotInstalled { .. })
        ));
        let retry = retry_settings_apply(
            &SettingsStore::load(&path).unwrap(),
            "screen.idle_timeout",
            &applier,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            retry.change().effective_setting().source(),
            SettingSource::Default
        );
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("screen_idle_timeout"));
        std::fs::remove_file(path).unwrap();
    }
}
