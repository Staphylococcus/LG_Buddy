//! Local changes to the configured TV, using the existing settings and pairing stores.

use super::{read_profiles_from_store, TvProfile};
use crate::config::HdmiInput;
use crate::presentation::brightness::UserFacingError;
use crate::settings::{
    persist_settings_mutation, ConfigPathResolver, SettingsApplier, SettingsMutation, SettingsStore,
};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvsManagementAction {
    SetInput(HdmiInput),
    RetryInputApply,
    Unpair,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvsManagementOperation {
    pub(super) id: u64,
    pub(super) profile: TvProfile,
    pub(super) action: TvsManagementAction,
}

impl TvsManagementOperation {
    pub fn profile(&self) -> &TvProfile {
        &self.profile
    }

    pub fn action(&self) -> TvsManagementAction {
        self.action
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TvsManagementOutcome {
    InputChanged(TvProfile),
    InputApplyFailed(TvProfile),
    Unpaired,
}

/// Sanitized failure; storage paths, protocol responses and tokens never enter presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvsManagementError(UserFacingError);

impl TvsManagementError {
    pub fn new(summary: &str, detail: &str) -> Self {
        Self(UserFacingError::new(summary, detail))
    }

    pub fn stopped() -> Self {
        Self::new(
            "TV change stopped",
            "Check the saved TV details before trying again.",
        )
    }

    pub fn presentation(&self) -> &UserFacingError {
        &self.0
    }
}

pub(super) fn manage(
    operation: &TvsManagementOperation,
) -> Result<TvsManagementOutcome, TvsManagementError> {
    let path = ConfigPathResolver::resolve_from_env().map_err(|_| configuration_changed())?;
    manage_at(&path, operation)
}

fn configuration_changed() -> TvsManagementError {
    TvsManagementError::new(
        "TV configuration changed",
        "Reopen LG Buddy to load the current TV before trying again.",
    )
}

fn manage_at(
    path: &Path,
    operation: &TvsManagementOperation,
) -> Result<TvsManagementOutcome, TvsManagementError> {
    let store = SettingsStore::load(path).map_err(|_| configuration_changed())?;
    let profiles = read_profiles_from_store(path, &store).map_err(|_| configuration_changed())?;
    let mut profile = profiles
        .into_iter()
        .next()
        .ok_or_else(configuration_changed)?;
    if !same_configuration(&profile, operation.profile()) {
        return Err(configuration_changed());
    }
    match operation.action {
        TvsManagementAction::Unpair => {
            crate::pairing_store::unpair_primary(path, &operation.profile).map_err(|_| {
                TvsManagementError::new("Could not unpair TV", "LG Buddy could not remove its local connection. Check access to the configuration and credential files, then try again.")
            })?;
            Ok(TvsManagementOutcome::Unpaired)
        }
        TvsManagementAction::SetInput(_) | TvsManagementAction::RetryInputApply => {
            // Retrying applies the already-persisted selection, never a stale draft value.
            let input = match operation.action {
                TvsManagementAction::SetInput(input) => input,
                _ => profile.input(),
            };
            let mutation =
                SettingsMutation::set(&store, "tv.input", input.as_str()).map_err(|_| {
                    TvsManagementError::new(
                        "Invalid HDMI input",
                        "Choose an input from HDMI 1 to HDMI 4.",
                    )
                })?;
            let change = persist_settings_mutation(path, mutation).map_err(|_| {
                TvsManagementError::new("Could not save HDMI input", "The previous input is still selected. Check access to the configuration file and try again.")
            })?;
            profile.input = input;
            profile.model_name = operation.profile.model_name.clone();
            match SettingsApplier::from_env().apply(&change) {
                Ok(_) => Ok(TvsManagementOutcome::InputChanged(profile)),
                Err(_) => Ok(TvsManagementOutcome::InputApplyFailed(profile)),
            }
        }
    }
}

pub(super) fn same_configuration(left: &TvProfile, right: &TvProfile) -> bool {
    left.id() == right.id()
        && left.address() == right.address()
        && left.mac() == right.mac()
        && left.input() == right.input()
        && left.platform() == right.platform()
}
