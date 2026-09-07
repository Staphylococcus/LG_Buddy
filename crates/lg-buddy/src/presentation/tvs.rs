use crate::tvs::{TvId, TvProfile, TvsIntent};

/// The application-owned state of the TVs view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvsPresentation {
    title: String,
    status: TvsStatus,
    profiles: Vec<TvProfile>,
    selected_id: Option<TvId>,
    retry_action: Option<TvsAction>,
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
        }
    }

    pub(crate) fn empty() -> Self {
        Self {
            title: "TVs".to_string(),
            status: TvsStatus::Empty {
                title: "No TV configured".to_string(),
                description: "Configure a TV to see its details here.".to_string(),
            },
            profiles: Vec::new(),
            selected_id: None,
            retry_action: None,
        }
    }

    pub(crate) fn ready(profiles: Vec<TvProfile>, selected_id: TvId) -> Self {
        Self {
            title: "TVs".to_string(),
            status: TvsStatus::Ready,
            profiles,
            selected_id: Some(selected_id),
            retry_action: None,
        }
    }

    pub(crate) fn failed(error: super::brightness::UserFacingError) -> Self {
        Self {
            title: "TVs".to_string(),
            status: TvsStatus::Failed(error),
            profiles: Vec::new(),
            selected_id: None,
            retry_action: Some(TvsAction::new("Retry", true, TvsIntent::Retry)),
        }
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
            .contains("not verified"));
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
