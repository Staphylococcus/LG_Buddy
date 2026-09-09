use crate::presentation::settings::{SettingsAction, SettingsPresentation};
use crate::settings_view::SettingsIntent;

/// The compact update row rendered in Settings.
///
/// Installation confirmation and progress remain in
/// [`crate::presentation::update_install::UpdateInstallPresentation`]. This projection only exposes the row's
/// title, concise description, and one semantic action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterPresentation {
    title: String,
    description: String,
    action: SettingsAction,
}

impl UpdaterPresentation {
    pub(crate) fn from_settings(settings: &SettingsPresentation) -> Self {
        let check = settings.update_check();
        let install = settings.update_install();
        let active = install.busy() || install.cancel_action().is_some();
        if let Some(action) = install
            .action()
            .filter(|action| action.intent() == SettingsIntent::RelaunchUpdatedApplication)
        {
            if !check.checking() {
                return Self {
                    title: "Restart required".into(),
                    description: "The update is installed. Restart LG Buddy to finish.".into(),
                    action: SettingsAction::new(
                        "Install update…",
                        action.enabled(),
                        action.intent(),
                    ),
                };
            }
        }
        let report = check.result().filter(|_| {
            (install.offer_channel_matches() == Some(true) && check.error().is_none()) || active
        });
        let release = report.and_then(|report| report.available_release.as_ref());
        let mut row = Self {
            title: release.map_or_else(
                || "Installed version".into(),
                |release| format!("Update available: {}", release.version),
            ),
            description: report.filter(|_| release.is_some()).map_or_else(
                || check.installed_version_label().to_owned(),
                |report| format!("Installed version: {}", report.installed_version),
            ),
            action: SettingsAction::new(
                "Check for updates",
                check.check_action().enabled(),
                SettingsIntent::CheckForUpdates,
            ),
        };
        if check.checking() {
            row.action = SettingsAction::new("Checking…", false, SettingsIntent::CheckForUpdates);
        } else if active {
            row.action = SettingsAction::new(
                "Install update…",
                false,
                SettingsIntent::PrepareUpdateInstall,
            );
        } else if release.is_some() {
            if let Some(action) = install.action() {
                row.action =
                    SettingsAction::new("Install update…", action.enabled(), action.intent());
            }
        }
        row
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn action(&self) -> &SettingsAction {
        &self.action
    }
}

#[cfg(test)]
mod tests {
    use crate::presentation::settings::SettingsPresentation;
    use crate::presentation::update_check::{AvailableUpdate, UpdateCheckReport};
    use crate::settings::ConfigEnvReader;
    use crate::settings_view::{SettingsApplication, SettingsIntent};
    use crate::update_flow::{UpdateInstallFailure, UpdateInstallOutcome};
    use crate::updates::UpdateChannel;
    use crate::version::VersionInfo;

    fn groups(channel: &str) -> Vec<crate::presentation::settings::SettingsGroup> {
        let contents = format!("updates_channel={channel}\nupdates_auto_check=disabled\n");
        let store = ConfigEnvReader::parse("/tmp/updater-row.env", &contents).into_store();
        SettingsPresentation::from_store(&store).groups().to_vec()
    }

    fn application(channel: &str) -> SettingsApplication {
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups(channel)))
            .unwrap();
        app
    }

    fn report(channel: UpdateChannel, available: bool) -> UpdateCheckReport {
        UpdateCheckReport {
            installed_version: "1.6.0".into(),
            channel,
            available_release: available.then(|| AvailableUpdate {
                version: "1.7.0".into(),
                url: "https://example.test/v1.7.0".into(),
            }),
            warning: None,
        }
    }

    fn complete_check(
        app: &mut SettingsApplication,
        result: Result<UpdateCheckReport, crate::settings_view::UpdateCheckError>,
    ) -> crate::settings_view::SettingsTransition {
        let operation = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        app.complete_update_check(operation, result).unwrap()
    }

    fn prepared() -> crate::update_install::PreparedUpdateInstall {
        crate::update_install::PreparedUpdateInstall::from_parts(
            VersionInfo::current(),
            "1.7.0".parse().unwrap(),
            UpdateChannel::Stable,
            "https://example.test/v1.7.0",
            "v1.7.0",
            "x86_64-unknown-linux-musl",
            "a".repeat(40),
        )
    }

    #[test]
    fn row_has_only_check_and_install_states() {
        let mut app = application("stable");
        let idle = app.presentation().updater();
        assert_eq!(idle.title(), "Installed version");
        assert_eq!(
            idle.description(),
            app.presentation().update_check().installed_version_label()
        );
        assert_eq!(idle.action().label(), "Check for updates");
        assert!(idle.action().enabled());

        let checking = app.handle_intent(SettingsIntent::CheckForUpdates).unwrap();
        let checking_row = checking.presentation().updater();
        assert_eq!(checking_row.title(), "Installed version");
        assert_eq!(checking_row.action().label(), "Checking…");
        assert!(!checking_row.action().enabled());

        app.complete_update_check(
            checking.update_check_operation().unwrap(),
            Ok(report(UpdateChannel::Stable, true)),
        )
        .unwrap();
        let offer = app.presentation().updater();
        assert_eq!(offer.title(), "Update available: 1.7.0");
        assert_eq!(offer.description(), "Installed version: 1.6.0");
        assert_eq!(offer.action().label(), "Install update…");
        assert!(offer.action().enabled());
    }

    #[test]
    fn channel_mismatch_keeps_installed_row_and_requires_check() {
        let mut app = application("prerelease");
        complete_check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let row = app.presentation().updater();
        assert_eq!(row.title(), "Installed version");
        assert_eq!(row.action().label(), "Check for updates");
        assert_eq!(row.action().intent(), SettingsIntent::CheckForUpdates);
        assert!(row.action().enabled());
    }

    #[test]
    fn busy_or_confirmation_disables_install_row_while_modal_owns_flow() {
        let mut app = application("stable");
        complete_check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let preparing = app
            .handle_intent(SettingsIntent::PrepareUpdateInstall)
            .unwrap();
        let row = preparing.presentation().updater();
        assert_eq!(row.title(), "Update available: 1.7.0");
        assert_eq!(row.action().label(), "Install update…");
        assert_eq!(row.action().intent(), SettingsIntent::PrepareUpdateInstall);
        assert!(!row.action().enabled());

        let operation = preparing.update_install_operation().unwrap().clone();
        app.complete_update_install(&operation, Ok(UpdateInstallOutcome::Prepared(prepared())))
            .unwrap();
        let confirmation = app.presentation().updater();
        assert_eq!(confirmation.title(), "Update available: 1.7.0");
        assert_eq!(confirmation.action().label(), "Install update…");
        assert_eq!(
            confirmation.action().intent(),
            SettingsIntent::PrepareUpdateInstall
        );
        assert!(!confirmation.action().enabled());
    }

    #[test]
    fn matching_offer_remains_visible_when_controls_are_temporarily_disabled() {
        let mut app = application("stable");
        complete_check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let transition = app.set_controls_available(false).unwrap();
        let row = transition.presentation().updater();
        assert_eq!(row.title(), "Update available: 1.7.0");
        assert_eq!(row.action().label(), "Install update…");
        assert!(!row.action().enabled());
    }

    #[test]
    fn install_failure_row_returns_install_retry_without_modal_fields() {
        let mut app = application("stable");
        complete_check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let preparation = app
            .handle_intent(SettingsIntent::PrepareUpdateInstall)
            .unwrap()
            .update_install_operation()
            .unwrap()
            .clone();
        app.complete_update_install(
            &preparation,
            Err(UpdateInstallFailure {
                presentation: crate::presentation::brightness::UserFacingError::new(
                    "Could not install update",
                    "The installer failed.",
                ),
                diagnostic: "installer failed".into(),
                cancelled: false,
            }),
        )
        .unwrap();
        let row = app.presentation().updater();
        assert_eq!(row.title(), "Update available: 1.7.0");
        assert_eq!(row.action().label(), "Install update…");
        assert_eq!(row.action().intent(), SettingsIntent::PrepareUpdateInstall);
        assert!(app.presentation().update_install().error().is_some());
    }

    #[test]
    fn completed_check_notices_are_one_shot_and_repeated_failures_are_sanitized() {
        let mut app = application("stable");
        let up_to_date = complete_check(&mut app, Ok(report(UpdateChannel::Stable, false)));
        assert_eq!(
            up_to_date.update_notice().unwrap().title(),
            "Already up to date"
        );
        assert!(up_to_date.update_notice().unwrap().details().is_none());

        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let refresh = app
            .complete_read(refresh.read_operation().unwrap(), Ok(groups("stable")))
            .unwrap();
        assert!(refresh.update_notice().is_none());

        let error = || {
            crate::settings_view::UpdateCheckError::from(crate::updates::UpdatesError::Http {
                url: "https://user:pass@example.test/release".into(),
                message: "token=secret".into(),
            })
        };
        let first = complete_check(&mut app, Err(error()));
        let first_notice = first.update_notice().unwrap();
        assert_eq!(first_notice.title(), "Could not check for updates");
        let first_details = first_notice.details().unwrap();
        assert!(first_details.contains("Check your internet connection"));
        assert!(!first_details.contains("secret"));
        assert!(!first_details.contains("user:pass"));

        let second = complete_check(&mut app, Err(error()));
        assert_eq!(second.update_notice(), first.update_notice());
    }
}
