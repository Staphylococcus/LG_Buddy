use crate::presentation::brightness::UserFacingError;
use crate::presentation::settings::SettingsAction;

/// The current, explicitly requested installation workflow in Settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateInstallPresentation {
    pub(crate) title: Option<String>,
    pub(crate) description: String,
    pub(crate) action: Option<SettingsAction>,
    pub(crate) cancel_action: Option<SettingsAction>,
    pub(crate) busy: bool,
    pub(crate) error: Option<UserFacingError>,
    pub(crate) failure_details: Option<String>,
    pub(crate) check_channel_matches: Option<bool>,
    pub(crate) release_url: Option<String>,
}

impl UpdateInstallPresentation {
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn action(&self) -> Option<&SettingsAction> {
        self.action.as_ref()
    }

    pub fn cancel_action(&self) -> Option<&SettingsAction> {
        self.cancel_action.as_ref()
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn error(&self) -> Option<&UserFacingError> {
        self.error.as_ref()
    }

    /// Most recent failure in this application session, retained across retries.
    /// Render on demand; the normal workflow uses the concise error above.
    pub fn failure_details(&self) -> Option<&str> {
        self.failure_details.as_deref()
    }

    pub fn failure_details_title(&self) -> &'static str {
        if self.error.is_some() {
            "Failure details"
        } else {
            "Last update failure"
        }
    }

    pub(crate) fn check_channel_matches(&self) -> Option<bool> {
        self.check_channel_matches
    }

    pub fn release_url(&self) -> Option<&str> {
        self.release_url.as_deref()
    }
}
