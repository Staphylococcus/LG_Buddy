use crate::tvs::{TvId, TvProfile, TvsIntent};

/// The application-owned state of the TVs view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvsPresentation {
    title: String,
    status: TvsStatus,
    profiles: Vec<TvProfile>,
    selected_id: Option<TvId>,
    retry_action: Option<TvsAction>,
    pair_action: Option<TvsAction>,
    pairing: Option<super::pairing::PairingPresentation>,
    input_enabled: bool,
    unpair_action: Option<TvsAction>,
    unpair_confirmation: Option<UnpairConfirmation>,
    management_error: Option<super::brightness::UserFacingError>,
    retry_apply_action: Option<TvsAction>,
}

/// The state a renderer can show without interpreting application policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TvsStatus {
    Loading { message: String },
    Empty { title: String, description: String },
    Ready,
    Failed(super::brightness::UserFacingError),
}

/// A semantic action declared by the application for the TVs view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvsAction {
    label: String,
    enabled: bool,
    intent: TvsIntent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpairConfirmation {
    confirm: TvsAction,
    cancel: TvsAction,
}
impl UnpairConfirmation {
    pub fn title(&self) -> &str {
        "Unpair TV?"
    }
    pub fn body(&self) -> &str {
        "Remove this TV and its saved native credential from LG Buddy? You can pair a TV again afterwards. This does not remove LG Buddy’s authorization on the TV itself."
    }
    pub fn confirm_action(&self) -> &TvsAction {
        &self.confirm
    }
    pub fn cancel_action(&self) -> &TvsAction {
        &self.cancel
    }
}

impl TvsPresentation {
    pub(crate) fn loading() -> Self {
        Self {
            title: "TVs".to_string(),
            status: TvsStatus::Loading {
                message: "Loading configured TVs…".to_string(),
            },
            profiles: Vec::new(),
            selected_id: None,
            retry_action: None,
            pair_action: None,
            pairing: None,
            input_enabled: false,
            unpair_action: None,
            unpair_confirmation: None,
            management_error: None,
            retry_apply_action: None,
        }
    }

    pub(crate) fn empty() -> Self {
        Self {
            title: "TVs".to_string(),
            status: TvsStatus::Empty {
                title: "No TV configured".to_string(),
                description: "Pair your TV to control it with LG Buddy.".to_string(),
            },
            profiles: Vec::new(),
            selected_id: None,
            retry_action: None,
            pair_action: Some(TvsAction::new("Pair a TV", true, TvsIntent::PairTv)),
            pairing: None,
            input_enabled: false,
            unpair_action: None,
            unpair_confirmation: None,
            management_error: None,
            retry_apply_action: None,
        }
    }

    pub(crate) fn ready(profiles: Vec<TvProfile>, selected_id: TvId) -> Self {
        Self {
            title: "TVs".to_string(),
            status: TvsStatus::Ready,
            profiles,
            selected_id: Some(selected_id),
            retry_action: None,
            pair_action: None,
            pairing: None,
            input_enabled: false,
            unpair_action: None,
            unpair_confirmation: None,
            management_error: None,
            retry_apply_action: None,
        }
    }

    pub(crate) fn failed(error: super::brightness::UserFacingError) -> Self {
        Self {
            title: "TVs".to_string(),
            status: TvsStatus::Failed(error),
            profiles: Vec::new(),
            selected_id: None,
            retry_action: Some(TvsAction::new("Retry", true, TvsIntent::Retry)),
            pair_action: None,
            pairing: None,
            input_enabled: false,
            unpair_action: None,
            unpair_confirmation: None,
            management_error: None,
            retry_apply_action: None,
        }
    }

    pub fn input_enabled(&self) -> bool {
        self.input_enabled
    }
    pub fn unpair_action(&self) -> Option<&TvsAction> {
        self.unpair_action.as_ref()
    }
    pub fn unpair_confirmation(&self) -> Option<&UnpairConfirmation> {
        self.unpair_confirmation.as_ref()
    }
    pub fn management_error(&self) -> Option<&super::brightness::UserFacingError> {
        self.management_error.as_ref()
    }
    pub fn retry_apply_action(&self) -> Option<&TvsAction> {
        self.retry_apply_action.as_ref()
    }

    pub(crate) fn set_input(&mut self, input: crate::config::HdmiInput) {
        if let Some(profile) = self
            .profiles
            .iter_mut()
            .find(|p| Some(p.id()) == self.selected_id.as_ref())
        {
            profile.set_input(input);
        }
    }

    pub(crate) fn set_management(
        &mut self,
        enabled: bool,
        confirming: bool,
        confirmation_enabled: bool,
        error: Option<super::brightness::UserFacingError>,
        retry_apply: bool,
    ) {
        self.input_enabled = enabled;
        if let Some(action) = &mut self.pair_action {
            action.enabled = confirmation_enabled;
        }
        self.unpair_action = self
            .selected_profile()
            .map(|_| TvsAction::new("Unpair TV…", enabled, TvsIntent::UnpairTv));
        self.unpair_confirmation = confirming.then(|| UnpairConfirmation {
            confirm: TvsAction::new("Unpair", confirmation_enabled, TvsIntent::ConfirmUnpair),
            cancel: TvsAction::new("Cancel", true, TvsIntent::CancelUnpair),
        });
        self.management_error = error;
        self.retry_apply_action =
            retry_apply.then(|| TvsAction::new("Retry", enabled, TvsIntent::RetryInputApply));
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn status(&self) -> &TvsStatus {
        &self.status
    }

    pub fn profiles(&self) -> &[TvProfile] {
        &self.profiles
    }

    pub fn selected_profile(&self) -> Option<&TvProfile> {
        self.selected_id
            .as_ref()
            .and_then(|id| self.profiles.iter().find(|profile| profile.id() == id))
    }

    pub fn selected_id(&self) -> Option<&TvId> {
        self.selected_id.as_ref()
    }

    pub fn retry_action(&self) -> Option<&TvsAction> {
        self.retry_action.as_ref()
    }

    pub fn pair_action(&self) -> Option<&TvsAction> {
        self.pair_action.as_ref()
    }

    pub fn pairing(&self) -> Option<&super::pairing::PairingPresentation> {
        self.pairing.as_ref()
    }

    pub(crate) fn set_pairing(&mut self, pairing: super::pairing::PairingPresentation) {
        self.pair_action = None;
        self.pairing = Some(pairing);
    }
}

impl TvsAction {
    pub(crate) fn new(label: &str, enabled: bool, intent: TvsIntent) -> Self {
        Self {
            label: label.to_string(),
            enabled,
            intent,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn intent(&self) -> TvsIntent {
        self.intent.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{TvsPresentation, TvsStatus};
    use crate::tvs::{TvCredentialState, TvsIntent};

    #[test]
    fn loading_presentation_is_renderer_safe() {
        let presentation = TvsPresentation::loading();

        assert_eq!(presentation.title(), "TVs");
        assert!(matches!(presentation.status(), TvsStatus::Loading { .. }));
        assert!(presentation.profiles().is_empty());
        assert!(presentation.selected_profile().is_none());
        assert!(presentation.retry_action().is_none());
    }

    #[test]
    fn credential_labels_describe_local_observation_only() {
        assert_eq!(TvCredentialState::Stored.label(), "Stored locally");
        assert!(TvCredentialState::Stored
            .description()
            .contains("does not establish current access"));
        assert_eq!(
            TvCredentialState::LocalFile.description(),
            "A legacy credential file is present locally; authentication is not verified."
        );
        assert_eq!(
            TvsPresentation::failed(crate::presentation::brightness::UserFacingError::new(
                "summary", "detail"
            ))
            .retry_action()
            .expect("retry action")
            .intent(),
            TvsIntent::Retry
        );
    }
}
