//! Application-owned TV profile collection and first-TV pairing entry point.
//!
//! The current durable configuration has one `primary` profile.  The
//! collection shape is intentional: it lets the renderer exercise selection
//! and adaptive multi-TV layouts without adding a second-TV storage format or
//! a second-TV pairing workflow to production.

mod management;
pub use management::{
    TvsManagementAction, TvsManagementError, TvsManagementOperation, TvsManagementOutcome,
};

use std::error::Error;
use std::fmt;
use std::fs;
use std::net::Ipv4Addr;
use std::time::Duration;

use crate::auth::{resolve_bscpylgtv_auth_context_from_env, resolve_config_owner};
use crate::config::{HdmiInput, MacAddress, TvPlatform};
use crate::pairing::{
    PairingApplication, PairingError, PairingFailure, PairingIntent, PairingOperation,
    PairingStage, PairingUpdate,
};
use crate::platform_access_token::{PlatformAccessTokenStore, PlatformAccessTokenStoreError};
use crate::presentation::brightness::UserFacingError;
use crate::presentation::tvs::TvsPresentation;
use crate::settings::{ConfigPathResolver, SettingValue, SettingsError, SettingsStore};
use crate::tv::{build_tv_client, TvClient, TvClientBuildOptions};

const PRIMARY_PROFILE_ID: &str = "primary";
const PRIMARY_PROFILE_NAME: &str = "Primary TV";
const MODEL_READ_TIMEOUT: Duration = Duration::from_secs(3);
const TV_STORAGE_KEYS: &[&str] = &[
    "tvs_primary_ip",
    "tv_ip",
    "tvs_primary_mac",
    "tv_mac",
    "tvs_primary_input",
    "input",
    "tvs_primary_platform",
];

/// Stable identity for a profile in the application-owned collection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TvId(String);

impl TvId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn primary() -> Self {
        Self::new(PRIMARY_PROFILE_ID)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for TvId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for TvId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for TvId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A configured TV profile.  This type deliberately contains no credential
/// material; `credentials` reports only local observability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvProfile {
    id: TvId,
    name: String,
    address: Ipv4Addr,
    mac: MacAddress,
    input: HdmiInput,
    platform: TvPlatform,
    credentials: TvCredentialState,
    model_name: Option<String>,
}

impl TvProfile {
    pub fn new<I, N>(
        id: I,
        name: N,
        address: Ipv4Addr,
        mac: MacAddress,
        input: HdmiInput,
        platform: TvPlatform,
        credentials: TvCredentialState,
    ) -> Self
    where
        I: Into<TvId>,
        N: Into<String>,
    {
        Self {
            id: id.into(),
            name: name.into(),
            address,
            mac,
            input,
            platform,
            credentials,
            model_name: None,
        }
    }

    pub fn id(&self) -> &TvId {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn model_name(&self) -> Option<&str> {
        self.model_name.as_deref()
    }

    pub fn display_name(&self) -> &str {
        self.model_name().unwrap_or_else(|| self.name())
    }

    pub fn address(&self) -> Ipv4Addr {
        self.address
    }

    pub fn mac(&self) -> MacAddress {
        self.mac
    }

    pub(crate) fn set_input(&mut self, input: HdmiInput) {
        self.input = input;
    }

    pub fn input(&self) -> HdmiInput {
        self.input
    }

    pub fn platform(&self) -> TvPlatform {
        self.platform
    }

    pub fn platform_label(&self) -> &'static str {
        match self.platform {
            TvPlatform::Bscpylgtv => "bscpylgtv (compatibility)",
            TvPlatform::LgWebOs => "Native webOS",
        }
    }

    pub fn input_label(&self) -> &'static str {
        match self.input {
            HdmiInput::Hdmi1 => "HDMI 1",
            HdmiInput::Hdmi2 => "HDMI 2",
            HdmiInput::Hdmi3 => "HDMI 3",
            HdmiInput::Hdmi4 => "HDMI 4",
        }
    }

    pub fn credentials(&self) -> TvCredentialState {
        self.credentials
    }
}

/// What LG Buddy can observe about a profile's local credentials.
///
/// `Stored` and `LocalFile` describe local files only.  They do not mean that
/// a TV is paired, reachable, or that the credential is accepted by the TV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvCredentialState {
    Stored,
    Missing,
    LocalFile,
    Malformed,
    Unreadable,
    Unknown,
}

impl TvCredentialState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stored => "Stored locally",
            Self::Missing => "Not found",
            Self::LocalFile => "Local file present",
            Self::Malformed => "Malformed local file",
            Self::Unreadable => "Local file unreadable",
            Self::Unknown => "Unknown",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Stored => {
                "A native access token is stored locally; this does not establish current access to the TV."
            }
            Self::Missing => "No local credential was found.",
            Self::LocalFile => {
                "A legacy credential file is present locally; authentication is not verified."
            }
            Self::Malformed => {
                "A local native credential file is malformed; authentication is not verified."
            }
            Self::Unreadable => {
                "A local credential file could not be read; authentication is not verified."
            }
            Self::Unknown => "The local credential state could not be determined.",
        }
    }
}

/// User actions that the TVs application accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TvsIntent {
    Select(TvId),
    Retry,
    PairTv,
    SetInput(HdmiInput),
    UnpairTv,
    ConfirmUnpair,
    CancelUnpair,
    RetryInputApply,
    Pairing(PairingIntent),
}

/// Opaque identity for one asynchronous profile read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TvsReadOperation(u64);

impl TvsReadOperation {
    pub(crate) fn new(id: u64) -> Self {
        Self(id)
    }
}

/// One optional live read for a specific profile, separate from local loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvsModelReadOperation {
    id: u64,
    profile: TvProfile,
}

impl TvsModelReadOperation {
    pub fn profile(&self) -> &TvProfile {
        &self.profile
    }
}

/// A state update for the TVs renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvsTransition {
    presentation: TvsPresentation,
    read_operation: Option<TvsReadOperation>,
    model_read_operation: Option<TvsModelReadOperation>,
    pairing_operation: Option<PairingOperation>,
    profile_changed: bool,
    management_operation: Option<TvsManagementOperation>,
    toast_message: Option<String>,
    diagnostic: Option<String>,
}

impl TvsTransition {
    pub(crate) fn update_presentation_from(&mut self, other: Self) {
        self.presentation = other.presentation;
    }

    pub(crate) fn clear_toast(&mut self) {
        self.toast_message = None;
    }
    pub fn presentation(&self) -> &TvsPresentation {
        &self.presentation
    }

    pub fn read_operation(&self) -> Option<TvsReadOperation> {
        self.read_operation
    }

    pub fn model_read_operation(&self) -> Option<&TvsModelReadOperation> {
        self.model_read_operation.as_ref()
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }

    pub fn pairing_operation(&self) -> Option<&PairingOperation> {
        self.pairing_operation.as_ref()
    }

    /// The durable profile changed, so other application views can reload it.
    pub(crate) fn profile_changed(&self) -> bool {
        self.profile_changed
    }

    pub fn management_operation(&self) -> Option<&TvsManagementOperation> {
        self.management_operation.as_ref()
    }

    /// One-time feedback for this transition, separate from persistent view state.
    pub fn toast_message(&self) -> Option<&str> {
        self.toast_message.as_deref()
    }
}

/// A read failure keeps configuration errors distinct from an empty profile
/// collection so the renderer can explain what needs fixing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvsReadFailure {
    NotConfigured,
    InvalidConfiguration,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvsReadError {
    failure: TvsReadFailure,
    diagnostic: String,
}

impl TvsReadError {
    pub fn new(failure: TvsReadFailure, diagnostic: impl Into<String>) -> Self {
        Self {
            failure,
            diagnostic: diagnostic.into(),
        }
    }

    pub fn failure(&self) -> TvsReadFailure {
        self.failure
    }

    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }

    pub fn internal(diagnostic: impl Into<String>) -> Self {
        Self::new(TvsReadFailure::Internal, diagnostic)
    }
}

impl fmt::Display for TvsReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic)
    }
}

impl Error for TvsReadError {}

/// The application boundary used by the GTK worker.
pub trait TvsBackend: Send + Sync + 'static {
    fn read_profiles(&self) -> Result<Vec<TvProfile>, TvsReadError>;
    fn read_model_name(&self, profile: &TvProfile) -> Result<String, TvsReadError>;
    fn manage(
        &self,
        _operation: &TvsManagementOperation,
    ) -> Result<TvsManagementOutcome, TvsManagementError> {
        Err(TvsManagementError::stopped())
    }
}

/// Production reader for the existing primary profile.
///
/// Profile loading reads `SettingsStore` and local credential metadata only.
/// A separate bounded model read uses the configured TV client's authentication
/// policy (stored tokens only for native webOS). Its failure does not hide the
/// saved profile.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentTvsBackend;

impl TvsBackend for EnvironmentTvsBackend {
    fn manage(
        &self,
        operation: &TvsManagementOperation,
    ) -> Result<TvsManagementOutcome, TvsManagementError> {
        management::manage(operation)
    }

    fn read_profiles(&self) -> Result<Vec<TvProfile>, TvsReadError> {
        let config_path = ConfigPathResolver::resolve_from_env()
            .map_err(|error| TvsReadError::new(TvsReadFailure::NotConfigured, error.to_string()))?;
        let store = SettingsStore::load(&config_path).map_err(settings_read_error)?;

        read_profiles_from_store(&config_path, &store)
    }

    fn read_model_name(&self, profile: &TvProfile) -> Result<String, TvsReadError> {
        let path = ConfigPathResolver::resolve_from_env()
            .map_err(|error| TvsReadError::internal(error.to_string()))?;
        let client = build_tv_client(
            &path,
            profile.address(),
            profile.platform(),
            TvClientBuildOptions::production()
                .stored_token_only()
                .with_command_timeout(MODEL_READ_TIMEOUT),
        )
        .map_err(|error| TvsReadError::internal(error.to_string()))?;
        client
            .model_name()
            .map_err(|error| TvsReadError::internal(error.to_string()))
    }
}

fn read_profiles_from_store(
    config_path: &std::path::Path,
    store: &SettingsStore,
) -> Result<Vec<TvProfile>, TvsReadError> {
    if !TV_STORAGE_KEYS
        .iter()
        .any(|key| store.raw_storage_value(key).is_some())
    {
        return Ok(Vec::new());
    }

    let address = required_ipv4(store, "tv.ip")?;
    let mac = required_mac(store, "tv.mac")?;
    let input = required_input(store, "tv.input")?;
    let platform = required_platform(store)?;
    let credentials = local_credentials(config_path, platform);

    Ok(vec![TvProfile::new(
        TvId::primary(),
        PRIMARY_PROFILE_NAME,
        address,
        mac,
        input,
        platform,
        credentials,
    )])
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TvsState {
    Loading(TvsReadOperation),
    Empty,
    Ready {
        profiles: Vec<TvProfile>,
        selected_id: TvId,
    },
    Failed(UserFacingError),
    Closed,
}

/// Headless TVs application state machine.
#[derive(Debug)]
pub struct TvsApplication {
    state: TvsState,
    next_operation_id: u64,
    pending_model: Option<TvsModelReadOperation>,
    pending_input: Option<HdmiInput>,
    pairing: Option<PairingApplication>,
    management: Option<TvsManagementOperation>,
    confirming_unpair: bool,
    controls_available: bool,
    management_error: Option<UserFacingError>,
    input_apply_failed: bool,
    closed: bool,
}

impl TvsApplication {
    pub fn open() -> (Self, TvsTransition) {
        let operation = TvsReadOperation::new(0);
        let application = Self {
            state: TvsState::Loading(operation),
            next_operation_id: 1,
            pending_model: None,
            pending_input: None,
            pairing: None,
            management: None,
            confirming_unpair: false,
            controls_available: true,
            management_error: None,
            input_apply_failed: false,
            closed: false,
        };
        let transition = application.transition(Some(operation), None);
        (application, transition)
    }

    pub fn handle_intent(&mut self, intent: TvsIntent) -> Option<TvsTransition> {
        if self.closed {
            return None;
        }
        match intent {
            TvsIntent::SetInput(input) => {
                if !self.can_set_input() || self.confirming_unpair {
                    return None;
                }
                let profile = self.selected_profile()?;
                let current_input = self
                    .pending_input
                    .or_else(|| {
                        self.management
                            .as_ref()
                            .and_then(|operation| match operation.action {
                                TvsManagementAction::SetInput(input) => Some(input),
                                _ => None,
                            })
                    })
                    .unwrap_or(profile.input());
                if current_input == input {
                    return None;
                }
                if self.management.is_some() {
                    self.pending_input = Some(input);
                    self.management_error = None;
                    return Some(self.transition(None, None));
                }
                self.start_management(TvsManagementAction::SetInput(input))
            }
            TvsIntent::RetryInputApply => {
                if !self.can_manage() || !self.input_apply_failed || self.confirming_unpair {
                    return None;
                }
                self.start_management(TvsManagementAction::RetryInputApply)
            }
            TvsIntent::UnpairTv => {
                if !self.can_manage() || self.confirming_unpair {
                    return None;
                }
                self.confirming_unpair = true;
                self.management_error = None;
                Some(self.transition(None, None))
            }
            TvsIntent::ConfirmUnpair => {
                if !self.can_manage() || !self.confirming_unpair {
                    return None;
                }
                self.confirming_unpair = false;
                self.start_management(TvsManagementAction::Unpair)
            }
            TvsIntent::CancelUnpair => {
                if !self.confirming_unpair {
                    return None;
                }
                self.confirming_unpair = false;
                Some(self.transition(None, None))
            }
            TvsIntent::PairTv => {
                if !self.controls_available
                    || !matches!(self.state, TvsState::Empty)
                    || self.pairing.is_some()
                {
                    return None;
                }
                self.pairing = Some(PairingApplication::new());
                Some(self.transition(None, None))
            }
            TvsIntent::Pairing(intent) => {
                let update = self
                    .pairing
                    .as_mut()?
                    .handle_intent(intent, self.next_operation_id)?;
                match update {
                    PairingUpdate::Changed => Some(self.transition(None, None)),
                    PairingUpdate::Cancelled => {
                        self.pairing = None;
                        Some(self.transition(None, None))
                    }
                    PairingUpdate::Start(operation) => {
                        self.next_operation_id += 1;
                        let mut transition = self.transition(None, None);
                        transition.pairing_operation = Some(operation);
                        Some(transition)
                    }
                }
            }
            TvsIntent::Retry => {
                if !matches!(self.state, TvsState::Failed(_)) {
                    return None;
                }
                let operation = self.new_read();
                Some(self.transition(Some(operation), None))
            }
            TvsIntent::Select(id) => {
                if self.is_managing() || self.confirming_unpair {
                    return None;
                }
                let TvsState::Ready {
                    profiles,
                    selected_id,
                } = &mut self.state
                else {
                    return None;
                };

                if *selected_id == id || !profiles.iter().any(|profile| profile.id() == &id) {
                    return None;
                }
                *selected_id = id;
                Some(self.transition_with_model_read())
            }
        }
    }

    pub fn complete_read(
        &mut self,
        operation: TvsReadOperation,
        result: Result<Vec<TvProfile>, TvsReadError>,
    ) -> Option<TvsTransition> {
        if self.closed {
            return None;
        }
        if !matches!(self.state, TvsState::Loading(active) if active == operation) {
            return None;
        }

        match result {
            Ok(profiles) if profiles.is_empty() => self.state = TvsState::Empty,
            Ok(profiles) => {
                let selected_id = profiles[0].id().clone();
                self.state = TvsState::Ready {
                    profiles,
                    selected_id,
                };
            }
            Err(error) => {
                let user_facing = tvs_error(error.failure());
                self.state = TvsState::Failed(user_facing);
                return Some(self.transition(
                    None,
                    Some(format!("could not load configured TVs: {error}")),
                ));
            }
        }

        Some(self.transition_with_model_read())
    }

    pub fn complete_model_read(
        &mut self,
        operation: TvsModelReadOperation,
        result: Result<String, TvsReadError>,
    ) -> Option<TvsTransition> {
        if self.closed {
            return None;
        }
        if self.pending_model.as_ref() != Some(&operation) {
            return None;
        }
        self.pending_model = None;
        let TvsState::Ready { profiles, .. } = &mut self.state else {
            return None;
        };
        let profile = profiles
            .iter_mut()
            .find(|profile| profile.id() == operation.profile.id())?;
        let diagnostic = match result {
            Ok(model) if !model.trim().is_empty() => {
                profile.model_name = Some(model.trim().to_owned());
                None
            }
            Ok(_) => Some("the TV returned an empty model name".to_string()),
            Err(error) => Some(format!("could not read the TV model: {error}")),
        };
        Some(self.transition(None, diagnostic))
    }

    pub fn shutdown(&mut self) {
        if let Some(pairing) = &self.pairing {
            pairing.shutdown();
        }
        self.pairing = None;
        self.confirming_unpair = false;
        self.pending_model = None;
        self.closed = true;
        if self.management.is_none() {
            self.state = TvsState::Closed;
        }
    }

    pub fn pairing_progress(
        &mut self,
        operation: &PairingOperation,
        stage: PairingStage,
    ) -> Option<TvsTransition> {
        if self.closed {
            return None;
        }
        self.pairing
            .as_mut()?
            .progress(operation, stage)
            .then(|| self.transition(None, None))
    }

    pub fn complete_pairing(
        &mut self,
        operation: &PairingOperation,
        result: Result<TvProfile, PairingError>,
    ) -> Option<TvsTransition> {
        if self.closed {
            return None;
        }
        if !self.pairing.as_mut()?.complete(operation, &result) {
            return None;
        }
        match result {
            Ok(profile) => {
                self.pairing = None;
                self.state = TvsState::Ready {
                    selected_id: profile.id().clone(),
                    profiles: vec![profile],
                };
                let mut transition = self.transition_with_model_read();
                transition.profile_changed = true;
                transition.toast_message = Some("TV paired successfully".into());
                Some(transition)
            }
            Err(error) => {
                if error.failure() == PairingFailure::Cancelled {
                    self.pairing = None;
                }
                let mut transition = self.transition(None, None);
                transition.toast_message = transition
                    .presentation()
                    .pairing()
                    .and_then(|pairing| pairing.error())
                    .map(|error| error.summary().to_owned());
                Some(transition)
            }
        }
    }

    pub fn is_pairing(&self) -> bool {
        self.pairing.is_some()
    }

    fn selected_profile(&self) -> Option<&TvProfile> {
        match &self.state {
            TvsState::Ready {
                profiles,
                selected_id,
            } => profiles.iter().find(|p| p.id() == selected_id),
            _ => None,
        }
    }

    fn can_manage(&self) -> bool {
        !self.closed
            && self.controls_available
            && self.management.is_none()
            && self.pairing.is_none()
            && self.selected_profile().is_some()
    }

    fn can_set_input(&self) -> bool {
        !self.closed
            && self.controls_available
            && self.pairing.is_none()
            && self.selected_profile().is_some()
            && self.management.as_ref().is_none_or(|operation| {
                matches!(operation.action, TvsManagementAction::SetInput(_))
            })
    }

    pub fn is_managing(&self) -> bool {
        self.management.is_some()
    }

    pub(crate) fn set_controls_available(&mut self, available: bool) -> Option<TvsTransition> {
        if self.controls_available == available {
            return None;
        }
        self.controls_available = available;
        Some(self.transition(None, None))
    }

    fn start_management(&mut self, action: TvsManagementAction) -> Option<TvsTransition> {
        let operation = TvsManagementOperation {
            id: self.next_operation_id,
            profile: self.selected_profile()?.clone(),
            action,
        };
        self.next_operation_id += 1;
        self.pending_model = None;
        self.management_error = None;
        self.management = Some(operation.clone());
        let mut transition = self.transition(None, None);
        transition.management_operation = Some(operation);
        Some(transition)
    }

    pub fn complete_management(
        &mut self,
        operation: &TvsManagementOperation,
        result: Result<TvsManagementOutcome, TvsManagementError>,
    ) -> Option<TvsTransition> {
        if self.management.as_ref() != Some(operation) {
            return None;
        }
        self.management = None;
        let pending_input = self.pending_input.take();
        let toast = match result {
            Ok(TvsManagementOutcome::Unpaired) => {
                self.state = TvsState::Empty;
                self.input_apply_failed = false;
                Some("TV unpaired".to_string())
            }
            Ok(TvsManagementOutcome::InputChanged(profile)) => {
                self.state = TvsState::Ready {
                    selected_id: profile.id().clone(),
                    profiles: vec![profile],
                };
                self.input_apply_failed = false;
                Some("HDMI input updated".to_string())
            }
            Ok(TvsManagementOutcome::InputApplyFailed(profile)) => {
                self.state = TvsState::Ready {
                    selected_id: profile.id().clone(),
                    profiles: vec![profile],
                };
                self.input_apply_failed = true;
                self.management_error = Some(UserFacingError::new(
                    "HDMI input saved",
                    "The selection was saved, but could not be applied. Retry applying it.",
                ));
                None
            }
            Err(error) => {
                self.management_error = Some(error.presentation().clone());
                None
            }
        };
        if let Some(input) = pending_input {
            let mut transition = self.start_management(TvsManagementAction::SetInput(input))?;
            transition.profile_changed = !self.closed;
            return Some(transition);
        }
        let mut transition = self.transition(None, None);
        transition.profile_changed = !self.closed;
        transition.toast_message = toast;
        Some(transition)
    }

    fn transition_with_model_read(&mut self) -> TvsTransition {
        let mut transition = self.transition(None, None);
        self.pending_model = transition
            .presentation()
            .selected_profile()
            .filter(|profile| profile.model_name().is_none())
            .map(|profile| {
                let operation = TvsModelReadOperation {
                    id: self.next_operation_id,
                    profile: profile.clone(),
                };
                self.next_operation_id += 1;
                operation
            });
        transition.model_read_operation = self.pending_model.clone();
        transition
    }

    fn new_read(&mut self) -> TvsReadOperation {
        let operation = TvsReadOperation::new(self.next_operation_id);
        self.next_operation_id += 1;
        self.state = TvsState::Loading(operation);
        operation
    }

    fn transition(
        &self,
        read_operation: Option<TvsReadOperation>,
        diagnostic: Option<String>,
    ) -> TvsTransition {
        TvsTransition {
            presentation: self.presentation(),
            read_operation,
            model_read_operation: None,
            pairing_operation: None,
            profile_changed: false,
            management_operation: None,
            toast_message: None,
            diagnostic,
        }
    }

    pub(crate) fn presentation(&self) -> TvsPresentation {
        let mut presentation = match &self.state {
            TvsState::Loading(_) => TvsPresentation::loading(),
            TvsState::Empty => TvsPresentation::empty(),
            TvsState::Ready {
                profiles,
                selected_id,
            } => TvsPresentation::ready(profiles.clone(), selected_id.clone()),
            TvsState::Failed(error) => TvsPresentation::failed(error.clone()),
            TvsState::Closed => TvsPresentation::loading(),
        };
        let pending_input = self.pending_input.or_else(|| {
            self.management
                .as_ref()
                .and_then(|operation| match operation.action {
                    TvsManagementAction::SetInput(input) => Some(input),
                    _ => None,
                })
        });
        if let Some(input) = pending_input {
            presentation.set_input(input);
        }
        let input_enabled = self.can_set_input() && !self.confirming_unpair;
        let actions_enabled = self.can_manage() && !self.confirming_unpair;
        presentation.set_management(
            input_enabled,
            actions_enabled,
            self.confirming_unpair,
            self.controls_available,
            self.management_error.clone(),
            self.input_apply_failed,
        );
        if let Some(pairing) = &self.pairing {
            presentation.set_pairing(pairing.presentation());
        }
        presentation
    }
}

fn settings_read_error(error: SettingsError) -> TvsReadError {
    let failure = match error {
        SettingsError::ConfigPath(_) => TvsReadFailure::NotConfigured,
        SettingsError::ReadConfig { .. } => TvsReadFailure::Internal,
        _ => TvsReadFailure::InvalidConfiguration,
    };
    TvsReadError::new(failure, error.to_string())
}

fn required_ipv4(store: &SettingsStore, key: &str) -> Result<Ipv4Addr, TvsReadError> {
    match store
        .effective_by_name(key)
        .and_then(|setting| setting.required_value())
    {
        Ok(SettingValue::Ipv4(value)) => Ok(value),
        Ok(_) => Err(invalid_setting(key, "an IPv4 address")),
        Err(error) => Err(invalid_setting_error(error)),
    }
}

fn required_mac(store: &SettingsStore, key: &str) -> Result<MacAddress, TvsReadError> {
    match store
        .effective_by_name(key)
        .and_then(|setting| setting.required_value())
    {
        Ok(SettingValue::MacAddress(value)) => Ok(value),
        Ok(_) => Err(invalid_setting(key, "a MAC address")),
        Err(error) => Err(invalid_setting_error(error)),
    }
}

fn required_input(store: &SettingsStore, key: &str) -> Result<HdmiInput, TvsReadError> {
    match store
        .effective_by_name(key)
        .and_then(|setting| setting.required_value())
    {
        Ok(SettingValue::Enum(value)) => value
            .parse()
            .map_err(|_| invalid_setting(key, "one of HDMI_1, HDMI_2, HDMI_3, HDMI_4")),
        Ok(_) => Err(invalid_setting(
            key,
            "one of HDMI_1, HDMI_2, HDMI_3, HDMI_4",
        )),
        Err(error) => Err(invalid_setting_error(error)),
    }
}

fn required_platform(store: &SettingsStore) -> Result<TvPlatform, TvsReadError> {
    match store
        .effective_by_name("tv.platform")
        .and_then(|setting| setting.required_value())
    {
        Ok(SettingValue::Enum(value)) => value
            .parse()
            .map_err(|_| invalid_setting("tv.platform", "bscpylgtv or lg_webos")),
        Ok(_) => Err(invalid_setting("tv.platform", "bscpylgtv or lg_webos")),
        Err(error) => Err(invalid_setting_error(error)),
    }
}

fn invalid_setting(key: &str, expected: &str) -> TvsReadError {
    TvsReadError::new(
        TvsReadFailure::InvalidConfiguration,
        format!("invalid value for setting `{key}`; expected {expected}"),
    )
}

fn invalid_setting_error(error: SettingsError) -> TvsReadError {
    TvsReadError::new(TvsReadFailure::InvalidConfiguration, error.to_string())
}

fn local_credentials(config_path: &std::path::Path, platform: TvPlatform) -> TvCredentialState {
    match platform {
        TvPlatform::LgWebOs => {
            let Ok(owner) = resolve_config_owner(config_path) else {
                return TvCredentialState::Unknown;
            };
            let Ok(store) = PlatformAccessTokenStore::for_primary_profile(config_path, owner)
            else {
                return TvCredentialState::Unknown;
            };
            match store.load() {
                Ok(Some(_)) => TvCredentialState::Stored,
                Ok(None) => TvCredentialState::Missing,
                Err(PlatformAccessTokenStoreError::InvalidJson { .. })
                | Err(PlatformAccessTokenStoreError::InvalidToken { .. }) => {
                    TvCredentialState::Malformed
                }
                Err(PlatformAccessTokenStoreError::Io { .. }) => TvCredentialState::Unreadable,
                Err(_) => TvCredentialState::Unknown,
            }
        }
        TvPlatform::Bscpylgtv => {
            let Ok(auth) = resolve_bscpylgtv_auth_context_from_env(config_path) else {
                return TvCredentialState::Unknown;
            };
            let Some(path) = auth.key_file_path() else {
                return TvCredentialState::Unknown;
            };
            match fs::metadata(path) {
                Ok(metadata) if metadata.is_file() => TvCredentialState::LocalFile,
                Ok(_) => TvCredentialState::Unknown,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    TvCredentialState::Unknown
                }
                Err(_) => TvCredentialState::Unreadable,
            }
        }
    }
}

fn tvs_error(failure: TvsReadFailure) -> UserFacingError {
    match failure {
        TvsReadFailure::NotConfigured => {
            UserFacingError::new("LG Buddy is not configured.", "Configure a TV, then retry.")
        }
        TvsReadFailure::InvalidConfiguration => UserFacingError::new(
            "LG Buddy could not load its TV configuration.",
            "Check the saved TV address, MAC address, input, and platform, then retry.",
        ),
        TvsReadFailure::Internal => UserFacingError::new(
            "LG Buddy could not load its TVs.",
            "Retry. If this continues, check the LG Buddy logs.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HdmiInput, TvPlatform};
    use crate::presentation::tvs::TvsStatus;
    use std::path::{Path, PathBuf};

    fn profile(id: &str, address: &str) -> TvProfile {
        TvProfile::new(
            id,
            id,
            address.parse().expect("address"),
            "aa:bb:cc:dd:ee:ff".parse().expect("MAC"),
            HdmiInput::Hdmi2,
            TvPlatform::Bscpylgtv,
            TvCredentialState::Unknown,
        )
    }

    struct TempConfig {
        root: PathBuf,
        path: PathBuf,
    }

    impl TempConfig {
        fn new(contents: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "lg-buddy-tvs-{}-{}",
                std::process::id(),
                unique_suffix()
            ));
            fs::create_dir(&root).expect("config directory");
            let path = root.join("config.env");
            fs::write(&path, contents).expect("config");
            Self { root, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn config_path(contents: &str) -> TempConfig {
        TempConfig::new(contents)
    }

    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    }

    #[test]
    fn opening_starts_one_read_and_loading_presentation() {
        let (app, transition) = TvsApplication::open();
        assert_eq!(transition.read_operation(), Some(TvsReadOperation::new(0)));
        assert!(matches!(
            transition.presentation().status(),
            TvsStatus::Loading { .. }
        ));
        assert!(matches!(app.state, TvsState::Loading(_)));
    }

    #[test]
    fn empty_and_ready_results_are_distinct() {
        let (mut app, opening) = TvsApplication::open();
        app.complete_read(opening.read_operation().expect("read"), Ok(Vec::new()));
        let empty = app.transition(None, None);
        assert!(matches!(
            empty.presentation().status(),
            TvsStatus::Empty { .. }
        ));

        let (mut app, opening) = TvsApplication::open();
        let first = profile("a", "192.0.2.1");
        let second = profile("b", "192.0.2.2");
        let transition = app
            .complete_read(
                opening.read_operation().expect("read"),
                Ok(vec![first, second]),
            )
            .expect("completion");
        assert!(matches!(
            transition.presentation().status(),
            TvsStatus::Ready
        ));
        assert_eq!(
            transition
                .presentation()
                .selected_profile()
                .unwrap()
                .id()
                .as_str(),
            "a"
        );
    }

    #[test]
    fn select_is_core_owned_and_stale_completion_is_ignored() {
        let (mut app, opening) = TvsApplication::open();
        assert!(app
            .complete_read(TvsReadOperation::new(99), Ok(Vec::new()))
            .is_none());
        app.complete_read(
            opening.read_operation().expect("read"),
            Ok(vec![profile("a", "192.0.2.1"), profile("b", "192.0.2.2")]),
        );
        let selected = app
            .handle_intent(TvsIntent::Select(TvId::from("b")))
            .expect("selection");
        assert_eq!(
            selected
                .presentation()
                .selected_profile()
                .unwrap()
                .id()
                .as_str(),
            "b"
        );
        assert!(app
            .handle_intent(TvsIntent::Select(TvId::from("missing")))
            .is_none());
    }

    #[test]
    fn model_read_enriches_the_profile_without_hiding_local_details() {
        let (mut app, opening) = TvsApplication::open();
        let ready = app
            .complete_read(
                opening.read_operation().unwrap(),
                Ok(vec![profile("a", "192.0.2.1")]),
            )
            .unwrap();
        assert_eq!(
            ready
                .presentation()
                .selected_profile()
                .unwrap()
                .display_name(),
            "a"
        );
        let operation = ready.model_read_operation().unwrap().clone();
        let updated = app
            .complete_model_read(operation.clone(), Ok(" OLED42C2 ".into()))
            .unwrap();
        let selected = updated.presentation().selected_profile().unwrap();
        assert_eq!(selected.display_name(), "OLED42C2");
        assert_eq!(selected.name(), "a");
        assert_eq!(selected.id().as_str(), "a");
        assert_eq!(selected.address(), "192.0.2.1".parse::<Ipv4Addr>().unwrap());
        assert!(updated.model_read_operation().is_none());
        assert!(app
            .complete_model_read(operation, Ok("duplicate".into()))
            .is_none());
    }

    #[test]
    fn unavailable_or_empty_model_keeps_the_saved_profile() {
        for result in [
            Err(TvsReadError::internal("TV offline")),
            Ok("  ".to_string()),
        ] {
            let (mut app, opening) = TvsApplication::open();
            let ready = app
                .complete_read(
                    opening.read_operation().unwrap(),
                    Ok(vec![profile("a", "192.0.2.1")]),
                )
                .unwrap();
            let updated = app
                .complete_model_read(ready.model_read_operation().unwrap().clone(), result)
                .unwrap();
            assert_eq!(updated.presentation(), ready.presentation());
            assert!(updated.diagnostic().is_some());
            assert!(
                updated.model_read_operation().is_none(),
                "failure must not schedule retries"
            );
        }
    }

    #[test]
    fn model_results_cannot_replace_a_different_selection_or_a_closed_view() {
        let (mut app, opening) = TvsApplication::open();
        let ready = app
            .complete_read(
                opening.read_operation().unwrap(),
                Ok(vec![profile("a", "192.0.2.1"), profile("b", "192.0.2.2")]),
            )
            .unwrap();
        let second = app.handle_intent(TvsIntent::Select("b".into())).unwrap();
        assert!(app
            .complete_model_read(
                ready.model_read_operation().unwrap().clone(),
                Ok("old model".into())
            )
            .is_none());
        app.shutdown();
        assert!(app
            .complete_model_read(
                second.model_read_operation().unwrap().clone(),
                Ok("late model".into())
            )
            .is_none());
    }

    #[test]
    fn invalid_partial_config_is_not_empty() {
        let config = config_path("tvs_primary_ip=192.0.2.42\n");
        let store = SettingsStore::load(config.path()).expect("settings");
        let error = read_profiles_from_store(config.path(), &store).expect_err("partial config");
        assert_eq!(error.failure(), TvsReadFailure::InvalidConfiguration);
    }

    #[test]
    fn invalid_value_config_is_not_empty() {
        let config = config_path(
            "tvs_primary_ip=not-an-ip\n\
             tvs_primary_mac=aa:bb:cc:dd:ee:ff\n\
             tvs_primary_input=HDMI_3\n",
        );
        let store = SettingsStore::load(config.path()).expect("settings");
        let error = read_profiles_from_store(config.path(), &store).expect_err("invalid config");
        assert_eq!(error.failure(), TvsReadFailure::InvalidConfiguration);
    }

    #[test]
    fn an_empty_settings_store_is_the_no_tv_state() {
        let config = config_path("");
        let store = SettingsStore::load(config.path()).expect("settings");
        assert!(read_profiles_from_store(config.path(), &store)
            .expect("empty settings")
            .is_empty());
    }

    #[test]
    fn legacy_keys_are_read_through_settings_store() {
        let config = config_path(
            "tv_ip=192.0.2.42\n\
             tv_mac=aa:bb:cc:dd:ee:ff\n\
             input=HDMI_3\n",
        );
        let store = SettingsStore::load(config.path()).expect("settings");
        let profiles = read_profiles_from_store(config.path(), &store).expect("legacy profile");
        assert_eq!(profiles.len(), 1);
        let profile = &profiles[0];
        assert_eq!(profile.address(), "192.0.2.42".parse::<Ipv4Addr>().unwrap());
        assert_eq!(profile.input(), HdmiInput::Hdmi3);
        assert_eq!(profile.platform(), TvPlatform::Bscpylgtv);
    }

    #[test]
    fn native_stored_token_is_reported_without_claiming_pairing() {
        let config = config_path(
            "tvs_primary_ip=192.0.2.42\n\
             tvs_primary_mac=aa:bb:cc:dd:ee:ff\n\
             tvs_primary_input=HDMI_3\n\
             tvs_primary_platform=lg_webos\n",
        );
        let owner = resolve_config_owner(config.path()).expect("owner");
        let store =
            PlatformAccessTokenStore::for_primary_profile(config.path(), owner).expect("store");
        let parent = store.token_path().parent().expect("profile dir");
        assert_eq!(
            local_credentials(config.path(), TvPlatform::LgWebOs),
            TvCredentialState::Missing
        );
        fs::create_dir_all(store.token_path()).expect("unreadable token directory");
        assert_eq!(
            local_credentials(config.path(), TvPlatform::LgWebOs),
            TvCredentialState::Unreadable
        );
        fs::remove_dir(store.token_path()).expect("remove token directory");
        fs::create_dir_all(parent).expect("profile dir");
        fs::write(store.token_path(), "not-json\n").expect("malformed token");
        assert_eq!(
            local_credentials(config.path(), TvPlatform::LgWebOs),
            TvCredentialState::Malformed
        );
        fs::write(store.token_path(), "{\"access_token\":\"token\"}\n").expect("token");
        assert_eq!(
            local_credentials(config.path(), TvPlatform::LgWebOs),
            TvCredentialState::Stored
        );
        assert!(TvCredentialState::Stored
            .description()
            .contains("does not establish current access"));
    }

    #[test]
    fn retry_starts_a_fresh_read_and_shutdown_rejects_late_results() {
        let (mut app, opening) = TvsApplication::open();
        let failed = app
            .complete_read(
                opening.read_operation().expect("opening read"),
                Err(TvsReadError::internal("read failed")),
            )
            .expect("failed read transition");
        assert!(matches!(
            failed.presentation().status(),
            TvsStatus::Failed(_)
        ));

        let retry = app
            .handle_intent(TvsIntent::Retry)
            .expect("retry transition");
        let retry_operation = retry.read_operation().expect("retry read");
        assert_ne!(
            retry_operation,
            opening.read_operation().expect("opening read")
        );
        assert!(app
            .complete_read(
                opening.read_operation().expect("opening read"),
                Ok(Vec::new())
            )
            .is_none());

        app.shutdown();
        assert!(app
            .complete_read(retry_operation, Ok(vec![profile("late", "192.0.2.8")]))
            .is_none());
    }
}

#[cfg(test)]
mod management_tests {
    use super::*;

    fn configured() -> (TvsApplication, TvsTransition) {
        let (mut app, opening) = TvsApplication::open();
        let profile = TvProfile::new(
            TvId::primary(),
            "TV",
            "192.0.2.10".parse().unwrap(),
            "02:11:22:33:44:55".parse().unwrap(),
            HdmiInput::Hdmi1,
            TvPlatform::LgWebOs,
            TvCredentialState::Stored,
        );
        let ready = app
            .complete_read(opening.read_operation().unwrap(), Ok(vec![profile]))
            .unwrap();
        (app, ready)
    }

    #[test]
    fn unpair_requires_confirmation_and_cancellation_keeps_profile() {
        let (mut app, ready) = configured();
        assert!(app.handle_intent(TvsIntent::ConfirmUnpair).is_none());
        let confirmation = app.handle_intent(TvsIntent::UnpairTv).unwrap();
        assert!(confirmation.presentation().unpair_confirmation().is_some());
        assert!(confirmation.management_operation().is_none());
        assert!(!confirmation.presentation().input_enabled());
        let cancelled = app.handle_intent(TvsIntent::CancelUnpair).unwrap();
        assert_eq!(
            cancelled.presentation().profiles(),
            ready.presentation().profiles()
        );
        assert!(cancelled.presentation().unpair_confirmation().is_none());
        assert!(cancelled.management_operation().is_none());
    }

    #[test]
    fn failed_unpair_preserves_profile_and_success_reuses_empty_pairing() {
        let (mut app, ready) = configured();
        app.handle_intent(TvsIntent::UnpairTv).unwrap();
        let started = app.handle_intent(TvsIntent::ConfirmUnpair).unwrap();
        let operation = started.management_operation().unwrap();
        assert!(app.handle_intent(TvsIntent::ConfirmUnpair).is_none());
        let failed = app
            .complete_management(operation, Err(TvsManagementError::stopped()))
            .unwrap();
        assert_eq!(
            failed.presentation().profiles(),
            ready.presentation().profiles()
        );
        assert!(failed.presentation().management_error().is_some());
        app.handle_intent(TvsIntent::UnpairTv).unwrap();
        let started = app.handle_intent(TvsIntent::ConfirmUnpair).unwrap();
        let operation = started.management_operation().unwrap();
        let success = app
            .complete_management(operation, Ok(TvsManagementOutcome::Unpaired))
            .unwrap();
        assert!(success.presentation().profiles().is_empty());
        assert_eq!(
            success.presentation().pair_action().unwrap().intent(),
            TvsIntent::PairTv
        );
        assert_eq!(success.toast_message(), Some("TV unpaired"));
        assert!(app
            .complete_model_read(
                ready.model_read_operation().unwrap().clone(),
                Ok("stale TV".into())
            )
            .is_none());
        assert!(app
            .complete_management(operation, Ok(TvsManagementOutcome::Unpaired))
            .is_none());
        assert!(app
            .handle_intent(TvsIntent::PairTv)
            .unwrap()
            .presentation()
            .pairing()
            .is_some());
    }

    #[test]
    fn input_failure_restores_selection_and_apply_failure_retains_saved_selection() {
        let (mut app, ready) = configured();
        let started = app
            .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
            .unwrap();
        assert_eq!(
            started.presentation().selected_profile().unwrap().input(),
            HdmiInput::Hdmi3
        );
        assert!(started.presentation().input_enabled());
        let queued = app
            .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi4))
            .expect("a later input choice is queued");
        assert_eq!(
            queued.presentation().selected_profile().unwrap().input(),
            HdmiInput::Hdmi4
        );
        assert!(queued.presentation().input_enabled());
        let queued = app
            .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi2))
            .expect("the latest input choice replaces the queued one");
        assert_eq!(
            queued.presentation().selected_profile().unwrap().input(),
            HdmiInput::Hdmi2
        );
        assert!(app.handle_intent(TvsIntent::UnpairTv).is_none());
        let failed = app
            .complete_management(
                started.management_operation().unwrap(),
                Err(TvsManagementError::stopped()),
            )
            .unwrap();
        assert_eq!(
            failed.presentation().selected_profile().unwrap().input(),
            HdmiInput::Hdmi2
        );
        assert!(failed.presentation().input_enabled());
        let queued_operation = failed
            .management_operation()
            .expect("the queued input must start after the first result");
        assert_eq!(
            queued_operation.action(),
            TvsManagementAction::SetInput(HdmiInput::Hdmi2)
        );
        assert_eq!(queued_operation.profile().input(), HdmiInput::Hdmi1);
        let failed = app
            .complete_management(queued_operation, Err(TvsManagementError::stopped()))
            .unwrap();
        assert_eq!(
            failed.presentation().selected_profile().unwrap().input(),
            HdmiInput::Hdmi1
        );
        assert!(failed.presentation().input_enabled());
        let started = app
            .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
            .unwrap();
        let mut saved = ready.presentation().selected_profile().unwrap().clone();
        saved.set_input(HdmiInput::Hdmi3);
        let applied = app
            .complete_management(
                started.management_operation().unwrap(),
                Ok(TvsManagementOutcome::InputApplyFailed(saved)),
            )
            .unwrap();
        assert_eq!(
            applied.presentation().selected_profile().unwrap().input(),
            HdmiInput::Hdmi3
        );
        assert!(applied.presentation().retry_apply_action().is_some());
        let retry = app.handle_intent(TvsIntent::RetryInputApply).unwrap();
        assert_eq!(
            retry.management_operation().unwrap().action(),
            TvsManagementAction::RetryInputApply
        );
        assert_eq!(
            retry.management_operation().unwrap().profile().input(),
            HdmiInput::Hdmi3
        );
        app.shutdown();
        assert!(app
            .complete_management(
                retry.management_operation().unwrap(),
                Err(TvsManagementError::stopped())
            )
            .is_some());
        assert!(app
            .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi4))
            .is_none());
    }

    #[test]
    fn shutdown_drains_a_queued_input_change() {
        let (mut app, ready) = configured();
        let started = app
            .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
            .unwrap();
        app.handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi4))
            .expect("queued input");
        app.handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi2))
            .expect("latest queued input");
        app.shutdown();

        let queued = app
            .complete_management(
                started.management_operation().unwrap(),
                Err(TvsManagementError::stopped()),
            )
            .expect("the active write completes after close");
        let queued_operation = queued
            .management_operation()
            .expect("the queued write starts after close");
        assert!(!queued.profile_changed());
        assert_eq!(
            queued_operation.action(),
            TvsManagementAction::SetInput(HdmiInput::Hdmi2)
        );
        let completed = app
            .complete_management(queued_operation, Err(TvsManagementError::stopped()))
            .expect("the queued write completes after close");
        assert_eq!(
            completed.presentation().profiles(),
            ready.presentation().profiles()
        );
        assert!(!completed.profile_changed());
        assert!(app
            .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi2))
            .is_none());
    }
}
