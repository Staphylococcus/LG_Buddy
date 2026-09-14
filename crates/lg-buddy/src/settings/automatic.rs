//! Explicit GUI transition from a saved legacy override to portable discovery.
//! Ordinary CLI mutations retain their persist-then-apply compatibility contract.

use std::{fs, path::Path};

use crate::backend::{resolve_backend_with_probe, BackendProbe, SystemBackendProbe};
use crate::config::ScreenBackend;

use super::{
    execute_settings_mutation, ServiceController, SettingsApplier, SettingsError, SettingsMutation,
    SettingsMutationFailure, SettingsMutationOutcome, SettingsMutationStage, SettingsStore,
};

pub(super) fn transition_to_automatic<C: ServiceController>(
    path: &Path,
    mutation: SettingsMutation,
    applier: &SettingsApplier<C>,
    progress: &mut dyn FnMut(SettingsMutationStage),
) -> Result<SettingsMutationOutcome, SettingsMutationFailure> {
    if std::env::var("LG_BUDDY_SCREEN_BACKEND").is_ok_and(|value| value != "auto") {
        return Err(failure("Remove the LG_BUDDY_SCREEN_BACKEND environment override before switching to automatic integration."));
    }
    transition_with_probe(
        path,
        mutation,
        applier,
        progress,
        &SystemBackendProbe::default(),
    )
}

fn transition_with_probe<C: ServiceController>(
    path: &Path,
    mutation: SettingsMutation,
    applier: &SettingsApplier<C>,
    progress: &mut dyn FnMut(SettingsMutationStage),
    probe: &impl BackendProbe,
) -> Result<SettingsMutationOutcome, SettingsMutationFailure> {
    let store = SettingsStore::load(path).map_err(SettingsMutationFailure::Validation)?;
    let backend = store
        .effective_by_name("screen.backend")
        .map_err(SettingsMutationFailure::Validation)?;
    if backend.value().and_then(super::SettingValue::as_enum) == Some("auto") {
        return execute_settings_mutation(path, mutation, applier, progress);
    }
    let idle_blank = store
        .effective_by_name("screen.idle_blank")
        .map_err(SettingsMutationFailure::Validation)?;
    if idle_blank.value().and_then(super::SettingValue::as_enum) != Some("disabled") {
        resolve_backend_with_probe(probe, ScreenBackend::Auto)
            .map_err(|error| failure(format!("Automatic integration was not enabled: {error}. Your previous settings are unchanged.")))?;
    }
    let original = fs::read(path).map_err(|error| {
        failure(format!(
            "Could not read the existing configuration: {error}"
        ))
    })?;
    let outcome = execute_settings_mutation(path, mutation, applier, progress)?;
    if let Err(error) = outcome.apply() {
        super::store::atomic_write_config(path, &original)
            .map_err(|rollback| failure(format!("Automatic integration could not be applied ({error}) and the previous configuration could not be restored ({rollback}). Check the saved desktop integration before retrying.")))?;
        // A failed restart may have stopped the old process. Restore its config
        // before requesting recovery; a second failure does not undo rollback.
        let recovery = applier.apply(outcome.change());
        let detail = recovery.err().map_or(String::new(), |error| {
            format!(" The service also needs attention: {error}.")
        });
        return Err(failure(format!("Automatic integration could not be applied: {error}. Your previous settings were restored.{detail}")));
    }
    Ok(outcome)
}

fn failure(message: impl Into<String>) -> SettingsMutationFailure {
    SettingsMutationFailure::Activation(SettingsError::Activation {
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::tests::{unique_test_path, FakeServiceController};

    struct NativeProbe(bool);
    impl BackendProbe for NativeProbe {
        fn has_command(&self, _: &str) -> bool {
            panic!("automatic transition must not probe swayidle")
        }
        fn gnome_shell_available(&self) -> bool {
            self.0
        }
        fn gnome_screen_saver_available(&self) -> bool {
            self.0
        }
        fn gnome_idle_monitor_available(&self) -> bool {
            self.0
        }
    }

    #[test]
    fn each_legacy_override_transitions_once_without_changing_behavior() {
        for backend in ["gnome", "wayland", "swayidle"] {
            let path = unique_test_path("automatic-transition");
            let original = format!("# retain comments\nscreen_backend={backend}\nscreen_idle_timeout=731\nscreen_honor_idle_inhibitors=enabled\nscreen_restore_policy=aggressive\n");
            fs::write(&path, &original).unwrap();
            let mutation = SettingsMutation::set(
                &SettingsStore::load(&path).unwrap(),
                "screen.backend",
                "auto",
            )
            .unwrap();
            let applier = SettingsApplier::new(FakeServiceController::active_or_enabled());
            transition_with_probe(&path, mutation, &applier, &mut |_| {}, &NativeProbe(true))
                .unwrap();
            let expected =
                original.replace(&format!("screen_backend={backend}"), "screen_backend=auto");
            assert_eq!(fs::read_to_string(&path).unwrap(), expected);
            // The portable configuration needs no migration on a later login,
            // including a login where no native activity source is present.
            transition_with_probe(&path, mutation, &applier, &mut |_| {}, &NativeProbe(false))
                .unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), expected);
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn failed_native_validation_and_runtime_apply_preserve_every_legacy_configuration() {
        for backend in ["gnome", "wayland", "swayidle"] {
            for native_available in [false, true] {
                let path = unique_test_path("automatic-transition-failure");
                let original = format!("# exact bytes, including no final newline\nscreen_backend={backend}\nscreen_idle_timeout=731");
                fs::write(&path, &original).unwrap();
                let mutation = SettingsMutation::set(
                    &SettingsStore::load(&path).unwrap(),
                    "screen.backend",
                    "auto",
                )
                .unwrap();
                let result = transition_with_probe(
                    &path,
                    mutation,
                    &SettingsApplier::new(FakeServiceController::failing_restart()),
                    &mut |_| {},
                    &NativeProbe(native_available),
                );
                assert!(result.is_err());
                assert_eq!(fs::read_to_string(&path).unwrap(), original);
                fs::remove_file(path).unwrap();
            }
        }
    }

    #[test]
    fn disabled_idle_blanking_allows_transition_without_native_activity() {
        let path = unique_test_path("automatic-transition-disabled");
        fs::write(
            &path,
            "screen_backend=swayidle\nscreen_idle_blank=disabled\n",
        )
        .unwrap();
        let mutation = SettingsMutation::set(
            &SettingsStore::load(&path).unwrap(),
            "screen.backend",
            "auto",
        )
        .unwrap();
        transition_with_probe(
            &path,
            mutation,
            &SettingsApplier::new(FakeServiceController::active_or_enabled()),
            &mut |_| {},
            &NativeProbe(false),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "screen_backend=auto\nscreen_idle_blank=disabled\n"
        );
        fs::remove_file(path).unwrap();
    }
}
