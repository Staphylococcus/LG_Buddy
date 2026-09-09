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
}
