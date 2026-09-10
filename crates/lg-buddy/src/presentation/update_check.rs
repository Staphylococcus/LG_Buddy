use crate::presentation::brightness::UserFacingError;
use crate::presentation::settings::SettingsAction;
use crate::settings_view::SettingsIntent;
use crate::updates::UpdateChannel;
use crate::version::VersionInfo;

/// Update availability on the checked channel, without an installation target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckReport {
    pub installed_version: String,
    pub channel: UpdateChannel,
    pub update_available: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckPresentation {
    installed_version_label: String,
    checking: bool,
    install_active: bool,
    result: Option<UpdateCheckReport>,
    error: Option<UserFacingError>,
}

impl Default for UpdateCheckPresentation {
    fn default() -> Self {
        let version = VersionInfo::current();
        Self {
            installed_version_label: format!(
                "{} ({})",
                version.version(),
                version.channel().as_str()
            ),
            checking: false,
            install_active: false,
            result: None,
            error: None,
        }
    }
}

impl UpdateCheckPresentation {
    pub fn installed_version_label(&self) -> &str {
        &self.installed_version_label
    }

    pub fn checking(&self) -> bool {
        self.checking
    }

    pub fn check_action(&self) -> SettingsAction {
        SettingsAction::new(
            if self.checking {
                "Checking…"
            } else if self.error.is_some() {
                "Retry check"
            } else {
                "Check for updates"
            },
            !self.checking && !self.install_active,
            SettingsIntent::CheckForUpdates,
        )
    }

    pub fn result(&self) -> Option<&UpdateCheckReport> {
        self.result.as_ref()
    }

    pub fn error(&self) -> Option<&UserFacingError> {
        self.error.as_ref()
    }

    pub(crate) fn set_install_active(&mut self, active: bool) {
        self.install_active = active;
    }

    pub(crate) fn start(&mut self) {
        self.checking = true;
        self.error = None;
    }

    pub(crate) fn clear_result(&mut self) {
        self.result = None;
        self.error = None;
    }

    pub(crate) fn complete(&mut self, result: Result<UpdateCheckReport, UserFacingError>) {
        self.checking = false;
        match result {
            Ok(report) => {
                self.result = Some(report);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}
