use crate::presentation::brightness::UserFacingError;
use crate::presentation::settings::SettingsAction;
use crate::settings_view::SettingsIntent;
use crate::updates::UpdateChannel;
use crate::version::VersionInfo;

/// A completed check, including the channel actually used by discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckReport {
    pub installed_version: String,
    pub channel: UpdateChannel,
    pub available_release: Option<AvailableUpdate>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub url: String,
}

impl UpdateCheckReport {
    pub fn title(&self) -> String {
        match &self.available_release {
            Some(release) => format!("Update available: {}", release.version),
            None => "No newer release available".to_string(),
        }
    }

    pub fn description(&self) -> String {
        format!(
            "Last successful check: {} channel, compared with installed version {}.",
            self.channel.as_str(),
            self.installed_version
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckPresentation {
    installed_version_label: String,
    checking: bool,
    install_active: bool,
    /// Whether the latest check was started after the current install state.
    /// This lets the derived updater card distinguish a new successful check
    /// from the older result that preceded an installation failure.
    check_supersedes_install: bool,
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
            check_supersedes_install: false,
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
        if active {
            self.check_supersedes_install = false;
        }
        self.install_active = active;
    }

    pub(crate) fn check_supersedes_install(&self) -> bool {
        self.check_supersedes_install
    }

    pub(crate) fn start(&mut self) {
        self.checking = true;
        self.check_supersedes_install = true;
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
