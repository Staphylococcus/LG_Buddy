//! Foreground first-TV pairing. The application validates the draft and owns
//! cancellation; its worker pairs and verifies before publishing any profile.

use std::net::Ipv4Addr;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::time::Duration;

use crate::config::{HdmiInput, MacAddress, TvPlatform};
use crate::pairing_store::PairingStore;
use crate::presentation::{brightness::UserFacingError, pairing::PairingPresentation};
use crate::settings::ConfigPathResolver;
use crate::tvs::{TvCredentialState, TvId, TvProfile};
use crate::web_os::{WebOsClient, WebOsEndpoint, WebOsPairingError, WebOsPairingEvent};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingIntent {
    SetAddress(String),
    SetMac(String),
    SetInput(HdmiInput),
    Submit,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingStage {
    Editing,
    Connecting,
    WaitingForConfirmation,
    Verifying,
    Saving,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PairingDraft {
    pub address: String,
    pub mac: String,
    pub input: HdmiInput,
}

impl Default for PairingDraft {
    fn default() -> Self {
        Self {
            address: String::new(),
            mac: String::new(),
            input: HdmiInput::Hdmi1,
        }
    }
}

/// Validated primary-profile fields. Platform is deliberately native webOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingRequest {
    address: Ipv4Addr,
    mac: MacAddress,
    input: HdmiInput,
}

impl PairingRequest {
    pub fn address(&self) -> Ipv4Addr {
        self.address
    }
    pub fn mac(&self) -> MacAddress {
        self.mac
    }
    pub fn input(&self) -> HdmiInput {
        self.input
    }
}

// Exactly one side wins the cancellation/commit boundary. The UI never waits
// for a filesystem lock: cancellation is accepted until publication starts.
const ACTIVE: u8 = 0;
const CANCELLED: u8 = 1;
const SAVING: u8 = 2;

#[derive(Debug, Clone)]
pub struct PairingOperation {
    id: u64,
    request: PairingRequest,
    gate: Arc<AtomicU8>,
}

impl PartialEq for PairingOperation {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && Arc::ptr_eq(&self.gate, &other.gate)
    }
}
impl Eq for PairingOperation {}

impl PairingOperation {
    pub fn request(&self) -> PairingRequest {
        self.request
    }
    pub fn is_cancelled(&self) -> bool {
        self.gate.load(Ordering::Acquire) == CANCELLED
    }
    fn cancel(&self) -> bool {
        self.gate
            .compare_exchange(ACTIVE, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            || self.is_cancelled()
    }
    fn begin_save(&self) -> bool {
        self.gate
            .compare_exchange(ACTIVE, SAVING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingFailure {
    Cancelled,
    Rejected,
    Timeout,
    Verification,
    Connection,
    Persistence,
}

/// Contains only application-owned guidance, never a server frame or token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingError {
    failure: PairingFailure,
}

impl PairingError {
    pub fn new(failure: PairingFailure) -> Self {
        Self { failure }
    }
    pub fn failure(&self) -> PairingFailure {
        self.failure
    }
    fn presentation(&self) -> UserFacingError {
        let (title, detail) = match self.failure {
            PairingFailure::Cancelled => ("Pairing cancelled", "No TV was saved."),
            PairingFailure::Rejected => ("Connection declined on TV", "Start pairing again and use the TV remote to allow the connection request."),
            PairingFailure::Timeout => ("The TV did not respond in time", "Check that the TV is on and reachable, then try again. Accept the connection request on the TV when it appears."),
            PairingFailure::Verification => ("TV access could not be verified", "Check that this TV supports LG Buddy’s power, sound, and OLED brightness controls, then try again."),
            PairingFailure::Connection => ("Could not connect to the TV", "Check the IP address, turn the TV on, and make sure it is on the same network."),
            PairingFailure::Persistence => ("Could not save the TV", "Run LG Buddy as your normal user. Check that its configuration folder is writable and that another setup has not already configured a TV."),
        };
        UserFacingError::new(title, detail)
    }
}

pub trait PairingBackend: Send + Sync + 'static {
    fn pair(
        &self,
        operation: &PairingOperation,
        progress: &mut dyn FnMut(PairingStage),
    ) -> Result<TvProfile, PairingError>;
}

#[derive(Debug, Default)]
pub struct EnvironmentPairingBackend;

impl PairingBackend for EnvironmentPairingBackend {
    fn pair(
        &self,
        operation: &PairingOperation,
        progress: &mut dyn FnMut(PairingStage),
    ) -> Result<TvProfile, PairingError> {
        let path = ConfigPathResolver::resolve_from_env()
            .map_err(|_| PairingError::new(PairingFailure::Persistence))?;
        pair_and_save(operation, &path, progress, |progress| {
            WebOsClient::pair_in_memory(
                WebOsEndpoint::wss(operation.request.address),
                Duration::from_secs(3),
                Duration::from_secs(60),
                &|| operation.is_cancelled(),
                &mut |event| {
                    progress(match event {
                        WebOsPairingEvent::WaitingForConfirmation => {
                            PairingStage::WaitingForConfirmation
                        }
                        WebOsPairingEvent::Verifying => PairingStage::Verifying,
                    })
                },
            )
            .map_err(|error| {
                PairingError::new(match error {
                    WebOsPairingError::Cancelled => PairingFailure::Cancelled,
                    WebOsPairingError::Rejected => PairingFailure::Rejected,
                    WebOsPairingError::Timeout => PairingFailure::Timeout,
                    WebOsPairingError::VerificationFailed => PairingFailure::Verification,
                    WebOsPairingError::Failed => PairingFailure::Connection,
                })
            })
            .map(|(_client, token)| token)
        })
    }
}

fn pair_and_save(
    operation: &PairingOperation,
    path: &std::path::Path,
    progress: &mut dyn FnMut(PairingStage),
    authenticate: impl FnOnce(
        &mut dyn FnMut(PairingStage),
    )
        -> Result<crate::platform_access_token::PlatformAccessToken, PairingError>,
) -> Result<TvProfile, PairingError> {
    if operation.is_cancelled() {
        return Err(PairingError::new(PairingFailure::Cancelled));
    }
    let persistence_error = || PairingError::new(PairingFailure::Persistence);
    let store = PairingStore::prepare(path).map_err(|_| persistence_error())?;
    let token = authenticate(progress)?;
    if !operation.begin_save() {
        return Err(PairingError::new(PairingFailure::Cancelled));
    }
    progress(PairingStage::Saving);
    let request = operation.request;
    store
        .commit(request.address, request.mac, request.input, &token)
        .map_err(|_| persistence_error())?;
    Ok(TvProfile::new(
        TvId::primary(),
        "Primary TV",
        request.address,
        request.mac,
        request.input,
        TvPlatform::LgWebOs,
        TvCredentialState::Stored,
    ))
}

#[derive(Debug)]
pub(crate) struct PairingApplication {
    draft: PairingDraft,
    stage: PairingStage,
    error: Option<UserFacingError>,
    active: Option<PairingOperation>,
}

pub(crate) enum PairingUpdate {
    Changed,
    Start(PairingOperation),
    Cancelled,
}

impl PairingApplication {
    pub fn new() -> Self {
        Self {
            draft: PairingDraft::default(),
            stage: PairingStage::Editing,
            error: None,
            active: None,
        }
    }

    pub fn presentation(&self) -> PairingPresentation {
        PairingPresentation::new(self.draft.clone(), self.stage, self.error.clone())
    }

    pub fn handle_intent(
        &mut self,
        intent: PairingIntent,
        operation_id: u64,
    ) -> Option<PairingUpdate> {
        if intent == PairingIntent::Cancel {
            if self.active.as_ref().is_none_or(PairingOperation::cancel) {
                return Some(PairingUpdate::Cancelled);
            }
            self.stage = PairingStage::Saving;
            return Some(PairingUpdate::Changed);
        }
        if self.active.is_some() {
            return None;
        }
        match intent {
            PairingIntent::SetAddress(value) => self.draft.address = value,
            PairingIntent::SetMac(value) => self.draft.mac = value,
            PairingIntent::SetInput(value) => self.draft.input = value,
            PairingIntent::Submit => {
                let request = match validate(&self.draft) {
                    Ok(request) => request,
                    Err(error) => {
                        self.error = Some(error);
                        self.stage = PairingStage::Failed;
                        return Some(PairingUpdate::Changed);
                    }
                };
                let operation = PairingOperation {
                    id: operation_id,
                    request,
                    gate: Arc::new(AtomicU8::new(ACTIVE)),
                };
                self.active = Some(operation.clone());
                self.error = None;
                self.stage = PairingStage::Connecting;
                return Some(PairingUpdate::Start(operation));
            }
            PairingIntent::Cancel => unreachable!(),
        }
        self.error = None;
        self.stage = PairingStage::Editing;
        Some(PairingUpdate::Changed)
    }

    pub fn progress(&mut self, operation: &PairingOperation, stage: PairingStage) -> bool {
        if self.active.as_ref() != Some(operation) {
            return false;
        }
        let valid = matches!(
            (self.stage, stage),
            (
                PairingStage::Connecting,
                PairingStage::WaitingForConfirmation | PairingStage::Verifying
            ) | (
                PairingStage::WaitingForConfirmation,
                PairingStage::Verifying
            ) | (PairingStage::Verifying, PairingStage::Saving)
        );
        if valid {
            self.stage = stage;
        }
        valid
    }

    pub fn complete(
        &mut self,
        operation: &PairingOperation,
        result: &Result<TvProfile, PairingError>,
    ) -> bool {
        if self.active.as_ref() != Some(operation) {
            return false;
        }
        self.active = None;
        if let Err(error) = result {
            self.stage = PairingStage::Failed;
            self.error = Some(error.presentation());
        }
        true
    }

    pub fn shutdown(&self) {
        if let Some(operation) = &self.active {
            operation.cancel();
        }
    }
}

fn validate(draft: &PairingDraft) -> Result<PairingRequest, UserFacingError> {
    let address = draft
        .address
        .trim()
        .parse::<Ipv4Addr>()
        .ok()
        .filter(|address| {
            !address.is_unspecified() && !address.is_multicast() && !address.is_broadcast()
        })
        .ok_or_else(|| {
            UserFacingError::new(
                "Enter the TV’s IP address",
                "Use an IPv4 address such as 192.168.1.50.",
            )
        })?;
    let mac = draft.mac.trim().parse::<MacAddress>().ok()
        .filter(|mac| mac.octets() != [0; 6] && mac.octets()[0] & 1 == 0)
        .ok_or_else(|| UserFacingError::new("Enter the TV’s MAC address", "Use six pairs of hexadecimal digits, such as 02:11:22:33:44:55. Use the address of the TV’s network connection."))?;
    Ok(PairingRequest {
        address,
        mac,
        input: draft.input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::tvs::TvsStatus;
    use crate::tvs::{TvsApplication, TvsIntent, TvsTransition};

    #[test]
    fn workflow_publishes_only_verified_uncancelled_profiles() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use crate::platform_access_token::{PlatformAccessToken, PlatformAccessTokenStore};
        use crate::settings::SettingsStore;
        use std::fs;
        let root =
            std::env::temp_dir().join(format!("lg-buddy-pairing-workflow-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for (index, outcome) in [
            Some(PairingFailure::Rejected),
            Some(PairingFailure::Timeout),
            Some(PairingFailure::Verification),
            Some(PairingFailure::Cancelled),
            None,
        ]
        .into_iter()
        .enumerate()
        {
            let path = root.join(index.to_string()).join("config.env");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let original = b"# Preserve unrelated settings\nscreen_idle_timeout=42\n";
            fs::write(&path, original).unwrap();
            let (mut app, _) = blank();
            let operation = submit(&mut app);
            let mut stages = Vec::new();
            let result = pair_and_save(
                &operation,
                &path,
                &mut |stage| stages.push(stage),
                |progress| {
                    assert_eq!(fs::read(&path).unwrap(), original);
                    assert!(!path
                        .parent()
                        .unwrap()
                        .join("tvs/primary/access-token.json")
                        .exists());
                    progress(PairingStage::WaitingForConfirmation);
                    progress(PairingStage::Verifying);
                    match outcome {
                        Some(PairingFailure::Cancelled) => {
                            assert!(operation.cancel());
                        }
                        Some(failure) => return Err(PairingError::new(failure)),
                        None => {}
                    }
                    Ok(PlatformAccessToken::new("test-client-key").unwrap())
                },
            );
            match outcome {
                Some(failure) => {
                    assert_eq!(result.unwrap_err().failure(), failure);
                    assert_eq!(fs::read(&path).unwrap(), original);
                    assert!(!path
                        .parent()
                        .unwrap()
                        .join("tvs/primary/access-token.json")
                        .exists());
                    assert!(!stages.contains(&PairingStage::Saving));
                }
                None => {
                    let result = result.unwrap();
                    let settings = SettingsStore::load(&path).unwrap();
                    assert_eq!(
                        settings.raw_storage_value("tvs_primary_ip"),
                        Some("192.0.2.10")
                    );
                    assert_eq!(
                        settings.raw_storage_value("tvs_primary_platform"),
                        Some("lg_webos")
                    );
                    let token_store = PlatformAccessTokenStore::for_primary_profile(
                        &path,
                        crate::auth::resolve_config_owner(&path).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(
                        token_store.load().unwrap(),
                        Some(PlatformAccessToken::new("test-client-key").unwrap())
                    );
                    let complete = app.complete_pairing(&operation, Ok(result)).unwrap();
                    assert!(complete.profile_created());
                    assert_eq!(stages.last(), Some(&PairingStage::Saving));
                    let retry = pair_and_save(&operation, &path, &mut |_| {}, |_| {
                        panic!("existing profile must refuse before connecting")
                    });
                    assert_eq!(retry.unwrap_err().failure(), PairingFailure::Persistence);
                }
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn blank() -> (TvsApplication, TvsTransition) {
        let (mut app, opening) = TvsApplication::open();
        assert!(opening.presentation().pair_action().is_none());
        let transition = app
            .complete_read(opening.read_operation().unwrap(), Ok(vec![]))
            .unwrap();
        (app, transition)
    }

    fn submit(app: &mut TvsApplication) -> PairingOperation {
        app.handle_intent(TvsIntent::PairTv).unwrap();
        app.handle_intent(TvsIntent::Pairing(PairingIntent::SetAddress(
            "192.0.2.10".into(),
        )))
        .unwrap();
        app.handle_intent(TvsIntent::Pairing(PairingIntent::SetMac(
            "02:11:22:33:44:55".into(),
        )))
        .unwrap();
        app.handle_intent(TvsIntent::Pairing(PairingIntent::SetInput(
            HdmiInput::Hdmi3,
        )))
        .unwrap();
        let transition = app
            .handle_intent(TvsIntent::Pairing(PairingIntent::Submit))
            .unwrap();
        assert!(transition.presentation().pair_action().is_none());
        assert_eq!(
            transition.presentation().pairing().unwrap().stage(),
            PairingStage::Connecting
        );
        transition.pairing_operation().unwrap().clone()
    }

    fn profile(operation: &PairingOperation) -> TvProfile {
        let request = operation.request();
        TvProfile::new(
            TvId::primary(),
            "Primary TV",
            request.address(),
            request.mac(),
            request.input(),
            TvPlatform::LgWebOs,
            TvCredentialState::Stored,
        )
    }

    #[test]
    fn first_tv_moves_from_blank_state_through_confirmation_to_details() {
        let (mut app, empty) = blank();
        assert_eq!(
            empty.presentation().pair_action().unwrap().intent(),
            TvsIntent::PairTv
        );
        let operation = submit(&mut app);
        for (stage, fraction) in [
            (PairingStage::WaitingForConfirmation, 0.25),
            (PairingStage::Verifying, 0.5),
        ] {
            let update = app.pairing_progress(&operation, stage).unwrap();
            assert_eq!(update.presentation().pairing().unwrap().stage(), stage);
            assert_eq!(
                update.presentation().pairing().unwrap().progress_fraction(),
                Some(fraction)
            );
            assert!(update.toast_message().is_none());
        }
        assert!(operation.begin_save());
        let saving = app
            .pairing_progress(&operation, PairingStage::Saving)
            .unwrap();
        assert!(!saving.presentation().pairing().unwrap().can_cancel());
        let saved = app
            .complete_pairing(&operation, Ok(profile(&operation)))
            .unwrap();
        assert!(saved.profile_created());
        assert_eq!(saved.toast_message(), Some("TV paired successfully"));
        assert_eq!(saved.presentation().profiles().len(), 1);
        assert_eq!(
            saved.presentation().selected_profile().unwrap().input(),
            HdmiInput::Hdmi3
        );
        assert!(saved.presentation().pairing().is_none());
        assert!(saved.presentation().pair_action().is_none());
        assert!(app.handle_intent(TvsIntent::PairTv).is_none());
    }

    #[test]
    fn cancel_at_each_network_stage_returns_to_blank_and_rejects_late_results() {
        for stage in [
            PairingStage::Connecting,
            PairingStage::WaitingForConfirmation,
            PairingStage::Verifying,
        ] {
            let (mut app, _) = blank();
            let operation = submit(&mut app);
            if stage != PairingStage::Connecting {
                app.pairing_progress(&operation, stage);
            }
            let cancelled = app
                .handle_intent(TvsIntent::Pairing(PairingIntent::Cancel))
                .unwrap();
            assert!(operation.is_cancelled());
            assert!(!operation.begin_save());
            assert!(matches!(
                cancelled.presentation().status(),
                TvsStatus::Empty { .. }
            ));
            assert!(cancelled.presentation().pair_action().is_some());
            assert!(cancelled.toast_message().is_none());
            assert!(app
                .pairing_progress(&operation, PairingStage::Verifying)
                .is_none());
            assert!(app
                .complete_pairing(&operation, Ok(profile(&operation)))
                .is_none());
            let second = submit(&mut app);
            assert_ne!(second, operation);
            assert!(app
                .complete_pairing(&operation, Ok(profile(&operation)))
                .is_none());
        }
    }

    #[test]
    fn failures_retain_editable_details_but_no_profile() {
        for failure in [
            PairingFailure::Rejected,
            PairingFailure::Timeout,
            PairingFailure::Verification,
            PairingFailure::Connection,
            PairingFailure::Persistence,
        ] {
            let (mut app, _) = blank();
            let operation = submit(&mut app);
            let failed = app
                .complete_pairing(&operation, Err(PairingError::new(failure)))
                .unwrap();
            let presentation = failed.presentation().pairing().unwrap();
            assert_eq!(presentation.stage(), PairingStage::Failed);
            assert_eq!(presentation.address(), "192.0.2.10");
            assert!(presentation.error().is_some());
            assert!(presentation.can_submit());
            assert!(failed.presentation().profiles().is_empty());
            assert!(!failed.profile_created());
            assert_eq!(
                failed.toast_message(),
                Some(presentation.error().unwrap().summary())
            );
            assert!(presentation.progress_fraction().is_none());
            let retry = app
                .handle_intent(TvsIntent::Pairing(PairingIntent::Submit))
                .unwrap();
            assert!(retry.pairing_operation().is_some());
            assert!(retry.toast_message().is_none());
        }
    }

    #[test]
    fn malformed_form_never_starts_io_and_cancellation_discards_draft() {
        let (mut app, _) = blank();
        app.handle_intent(TvsIntent::PairTv).unwrap();
        let invalid = app
            .handle_intent(TvsIntent::Pairing(PairingIntent::Submit))
            .unwrap();
        assert!(invalid.pairing_operation().is_none());
        assert!(invalid.toast_message().is_none());
        assert!(invalid
            .presentation()
            .pairing()
            .unwrap()
            .error()
            .unwrap()
            .summary()
            .contains("IP"));
        app.handle_intent(TvsIntent::Pairing(PairingIntent::SetAddress(
            "192.0.2.10".into(),
        )))
        .unwrap();
        let invalid = app
            .handle_intent(TvsIntent::Pairing(PairingIntent::Submit))
            .unwrap();
        assert!(invalid.pairing_operation().is_none());
        assert!(invalid
            .presentation()
            .pairing()
            .unwrap()
            .error()
            .unwrap()
            .summary()
            .contains("MAC"));
        app.handle_intent(TvsIntent::Pairing(PairingIntent::Cancel))
            .unwrap();
        let fresh = app.handle_intent(TvsIntent::PairTv).unwrap();
        assert!(fresh.presentation().pairing().unwrap().address().is_empty());
    }

    #[test]
    fn saving_wins_late_cancel_and_shutdown_cancels_only_uncommitted_work() {
        let (mut app, _) = blank();
        let operation = submit(&mut app);
        assert!(operation.begin_save());
        let cancel = app
            .handle_intent(TvsIntent::Pairing(PairingIntent::Cancel))
            .unwrap();
        assert_eq!(
            cancel.presentation().pairing().unwrap().stage(),
            PairingStage::Saving
        );
        assert!(!operation.is_cancelled());
        assert!(app
            .complete_pairing(&operation, Ok(profile(&operation)))
            .unwrap()
            .profile_created());

        let (mut app, _) = blank();
        let operation = submit(&mut app);
        assert!(app
            .handle_intent(TvsIntent::Pairing(PairingIntent::Submit))
            .is_none());
        app.shutdown();
        assert!(operation.is_cancelled());
        assert!(app
            .complete_pairing(&operation, Ok(profile(&operation)))
            .is_none());
        assert!(app.handle_intent(TvsIntent::PairTv).is_none());
    }

    #[test]
    fn validate_unicast_addresses_and_normalize_whitespace() {
        let mut draft = PairingDraft {
            address: " 192.0.2.10 ".into(),
            mac: " 02:11:22:33:44:FF ".into(),
            input: HdmiInput::Hdmi1,
        };
        assert_eq!(
            validate(&draft).unwrap().mac.to_string(),
            "02:11:22:33:44:ff"
        );
        for address in ["0.0.0.0", "255.255.255.255", "224.0.0.1", "not-an-ip"] {
            draft.address = address.into();
            assert!(validate(&draft).is_err());
        }
        draft.address = "192.0.2.10".into();
        for mac in [
            "00:00:00:00:00:00",
            "ff:ff:ff:ff:ff:ff",
            "01:11:22:33:44:55",
            "bad",
        ] {
            draft.mac = mac.into();
            assert!(validate(&draft).is_err());
        }
    }
}
