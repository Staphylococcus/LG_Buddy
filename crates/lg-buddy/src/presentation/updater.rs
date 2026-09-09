use crate::presentation::settings::{SettingsAction, SettingsPresentation};
use crate::presentation::update_check::AvailableUpdate;
use crate::settings_view::SettingsIntent;

/// The one update card rendered by the Settings view.
///
/// `UpdateCheckPresentation` and `UpdateInstallPresentation` remain the
/// application-owned workflow facts. This type is a derived, renderer-facing
/// snapshot that resolves which one is visible at a given moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterPresentation {
    title: String,
    description: String,
    action: Option<SettingsAction>,
    cancel_action: Option<SettingsAction>,
    release: Option<AvailableUpdate>,
    busy: bool,
    is_error: bool,
    warning: Option<String>,
    details_title: String,
    details: Option<String>,
}

impl UpdaterPresentation {
    pub(crate) fn from_settings(settings: &SettingsPresentation) -> Self {
        let check = settings.update_check();
        let install = settings.update_install();
        let details = install.failure_details().map(str::to_owned);
        let details_title =
            if check.checking() || check.error().is_some() || check.check_supersedes_install() {
                "Last update failure".to_string()
            } else {
                install.failure_details_title().to_string()
            };

        // A prepared confirmation, download, installation, or process
        // replacement owns the card even if an older check result is still in
        // memory.
        if install.busy() || install.cancel_action().is_some() {
            return Self {
                title: install.title().unwrap_or("Updating LG Buddy…").to_string(),
                description: install.description().to_string(),
                action: install.action().cloned(),
                cancel_action: install.cancel_action().cloned(),
                release: None,
                busy: install.busy(),
                is_error: false,
                warning: None,
                details_title,
                details,
            };
        }

        // A newly started or failed check masks both the old successful result
        // and an older installation failure. This keeps the card from showing
        // two unrelated workflows at once.
        if check.checking() {
            return Self {
                title: "Checking for updates…".to_string(),
                description: format!(
                    "Checking the saved update channel against installed version {}.",
                    check.installed_version_label()
                ),
                action: Some(check.check_action()),
                cancel_action: None,
                release: None,
                busy: true,
                is_error: false,
                warning: None,
                details_title,
                details,
            };
        }

        if let Some(error) = check.error() {
            return Self {
                title: error.summary().to_string(),
                description: error.detail().to_string(),
                action: Some(check.check_action()),
                cancel_action: None,
                release: None,
                busy: false,
                is_error: true,
                warning: None,
                details_title,
                details,
            };
        }

        // A completed check started after an installation failure is now the
        // current workflow. Keep the old diagnostic available below the card,
        // but let the new report own the title, release, and primary action.
        if check.check_supersedes_install() {
            if let Some(report) = check.result() {
                return Self::completed_check(check, install, report, details_title, details);
            }
        }

        // An installation failure is shown using its existing safe summary
        // and detail. The internal "Update not completed" title is deliberately
        // not surfaced as a second, duplicate failure row.
        if let Some(error) = install.error() {
            let action = install
                .action()
                .filter(|action| action.enabled())
                .cloned()
                .or_else(|| Some(check.check_action()));
            return Self {
                title: error.summary().to_string(),
                description: error.detail().to_string(),
                action,
                cancel_action: None,
                release: None,
                busy: false,
                is_error: true,
                warning: None,
                details_title,
                details,
            };
        }

        if let Some(report) = check.result() {
            return Self::completed_check(check, install, report, details_title, details);
        }

        Self {
            title: "Installed version".to_string(),
            description: check.installed_version_label().to_string(),
            action: Some(check.check_action()),
            cancel_action: None,
            release: None,
            busy: false,
            is_error: false,
            warning: None,
            details_title,
            details,
        }
    }

    fn completed_check(
        check: &crate::presentation::update_check::UpdateCheckPresentation,
        install: &crate::presentation::update_install::UpdateInstallPresentation,
        report: &crate::presentation::update_check::UpdateCheckReport,
        details_title: String,
        details: Option<String>,
    ) -> Self {
        let can_install_offer = report.available_release.is_some()
            && install.action().is_some_and(|action| {
                action.enabled() && action.intent() == SettingsIntent::PrepareUpdateInstall
            });
        let action = if can_install_offer {
            install.action().cloned()
        } else {
            // A changed channel, or a temporarily unavailable install action,
            // must leave the user with a way to perform a fresh check instead
            // of a disabled install control.
            Some(check.check_action())
        };

        Self {
            title: report.title(),
            description: report.description(),
            action,
            cancel_action: None,
            release: report.available_release.clone(),
            busy: false,
            is_error: false,
            warning: report.warning.clone(),
            details_title,
            details,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
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

    pub fn release(&self) -> Option<&AvailableUpdate> {
        self.release.as_ref()
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn is_error(&self) -> bool {
        self.is_error
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn details_title(&self) -> &str {
        &self.details_title
    }

    pub fn details(&self) -> Option<&str> {
        self.details.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::settings::SettingsPresentation;
    use crate::presentation::update_check::UpdateCheckReport;
    use crate::settings::ConfigEnvReader;
    use crate::settings_view::{SettingsApplication, SettingsIntent};
    use crate::update_flow::{UpdateInstallFailure, UpdateInstallOperation, UpdateInstallOutcome};
    use crate::update_install::{InstalledUpdate, UpdateInstallStage};
    use crate::updates::UpdateChannel;
    use crate::version::VersionInfo;

    fn groups(channel: &str) -> Vec<crate::presentation::settings::SettingsGroup> {
        let contents = format!("updates_channel={channel}\nupdates_auto_check=disabled\n");
        let store = ConfigEnvReader::parse("/tmp/updater-card.env", &contents).into_store();
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

    fn check(
        app: &mut SettingsApplication,
        report: Result<UpdateCheckReport, crate::settings_view::UpdateCheckError>,
    ) {
        let operation = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        app.complete_update_check(operation, report).unwrap();
    }

    fn prepare(app: &mut SettingsApplication) -> UpdateInstallOperation {
        app.handle_intent(SettingsIntent::PrepareUpdateInstall)
            .unwrap()
            .update_install_operation()
            .unwrap()
            .clone()
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
    fn application_transitions_render_one_card_through_install_failure_and_retry() {
        let mut app = application("stable");
        check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let offer = app.presentation().updater();
        assert_eq!(offer.title(), "Update available: 1.7.0");
        assert_eq!(
            offer.action().map(SettingsAction::label),
            Some("Install update…")
        );
        assert_eq!(
            offer.release().map(|release| release.version.as_str()),
            Some("1.7.0")
        );

        let preparation = prepare(&mut app);
        let preparing = app.presentation().updater();
        assert!(preparing.busy());
        assert_eq!(
            preparing.cancel_action().map(SettingsAction::label),
            Some("Cancel")
        );
        app.complete_update_install(&preparation, Ok(UpdateInstallOutcome::Prepared(prepared())))
            .unwrap();
        let confirmation = app.presentation().updater();
        assert_eq!(confirmation.title(), "Install LG Buddy 1.7.0?");
        assert!(!confirmation.busy());
        assert_eq!(
            confirmation.action().map(SettingsAction::label),
            Some("Install and restart")
        );
        assert_eq!(
            confirmation.cancel_action().map(SettingsAction::label),
            Some("Cancel")
        );

        let install = app
            .handle_intent(SettingsIntent::ConfirmUpdateInstall)
            .unwrap()
            .update_install_operation()
            .unwrap()
            .clone();
        app.update_install_progress(&install, UpdateInstallStage::Acquiring)
            .unwrap();
        let progress = app.presentation().updater();
        assert!(progress.busy());
        assert_eq!(progress.action(), None);
        assert_eq!(
            progress.cancel_action().map(SettingsAction::label),
            Some("Cancel")
        );

        app.update_install_progress(&install, UpdateInstallStage::Installing)
            .unwrap();
        assert_eq!(app.presentation().updater().cancel_action(), None);
        let failed = app
            .complete_update_install(
                &install,
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
        let failure = failed.presentation().updater();
        assert!(failure.is_error());
        assert_eq!(failure.title(), "Could not install update");
        assert_eq!(failure.description(), "The installer failed.");
        assert_eq!(
            failure.action().map(SettingsAction::label),
            Some("Retry update")
        );
        assert_eq!(failure.release(), None);
        assert_eq!(failure.warning(), None);
        assert_eq!(failure.details(), Some("installer failed"));

        let details = failure.details().unwrap().to_owned();
        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let groups = app.presentation().groups().to_vec();
        app.complete_read(refresh.read_operation().unwrap(), Ok(groups))
            .unwrap();
        assert_eq!(
            app.presentation().updater().details(),
            Some(details.as_str())
        );
        let retry_operation = prepare(&mut app);
        assert_eq!(
            app.presentation().updater().details(),
            Some(details.as_str())
        );
        app.handle_intent(SettingsIntent::CancelUpdateInstall)
            .unwrap();
        assert_eq!(
            app.presentation().updater().details_title(),
            "Last update failure"
        );
        assert_eq!(
            app.presentation().updater().details(),
            Some(details.as_str())
        );
        assert!(app
            .complete_update_install(
                &retry_operation,
                Ok(UpdateInstallOutcome::Prepared(prepared()))
            )
            .is_none());

        let retry = app.presentation().updater();
        assert_eq!(
            retry.action().map(SettingsAction::intent),
            Some(SettingsIntent::PrepareUpdateInstall)
        );
    }

    #[test]
    fn checking_and_check_failure_mask_stale_result_and_install_failure() {
        let mut app = application("stable");
        check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let preparation = prepare(&mut app);
        app.complete_update_install(
            &preparation,
            Err(UpdateInstallFailure {
                presentation: crate::presentation::brightness::UserFacingError::new(
                    "Install failed",
                    "Try again.",
                ),
                diagnostic: "details".into(),
                cancelled: false,
            }),
        )
        .unwrap();
        let check_operation = app.handle_intent(SettingsIntent::CheckForUpdates).unwrap();
        let checking = check_operation.presentation().updater();
        assert_eq!(checking.title(), "Checking for updates…");
        assert!(!checking.is_error());
        assert_eq!(checking.release(), None);
        assert_eq!(checking.warning(), None);
        assert_eq!(checking.details_title(), "Last update failure");

        let operation = check_operation.update_check_operation().unwrap();
        let failed = app
            .complete_update_check(
                operation,
                Err(crate::settings_view::UpdateCheckError::stopped()),
            )
            .unwrap();
        let check_failed = failed.presentation().updater();
        assert!(check_failed.is_error());
        assert_eq!(check_failed.title(), "Update check stopped");
        assert_eq!(
            check_failed.action().map(SettingsAction::label),
            Some("Retry check")
        );
        assert_eq!(check_failed.details_title(), "Last update failure");
    }

    #[test]
    fn channel_mismatch_uses_check_and_matching_channel_uses_install() {
        let mut app = application("prerelease");
        check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let mismatch = app.presentation().updater();
        assert_eq!(
            mismatch.action().map(SettingsAction::label),
            Some("Check for updates")
        );
        assert_eq!(
            mismatch.release().map(|release| release.version.as_str()),
            Some("1.7.0")
        );

        let mut app = application("stable");
        check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        assert_eq!(
            app.presentation()
                .updater()
                .action()
                .map(SettingsAction::label),
            Some("Install update…")
        );
    }

    #[test]
    fn completed_check_without_release_clears_the_old_offer() {
        let mut app = application("stable");
        check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        assert_eq!(
            app.presentation()
                .updater()
                .action()
                .map(SettingsAction::label),
            Some("Install update…")
        );

        let operation = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        let mut current = report(UpdateChannel::Stable, false);
        current.warning = Some("Cache warning".into());
        app.complete_update_check(operation, Ok(current)).unwrap();
        let no_update = app.presentation().updater();
        assert_eq!(no_update.title(), "No newer release available");
        assert_eq!(no_update.release(), None);
        assert_eq!(
            no_update.action().map(SettingsAction::label),
            Some("Check for updates")
        );
        assert_eq!(no_update.warning(), Some("Cache warning"));
    }

    #[test]
    fn a_new_successful_check_owns_the_card_but_keeps_the_old_diagnostic() {
        let mut app = application("stable");
        check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let preparation = prepare(&mut app);
        app.complete_update_install(
            &preparation,
            Err(UpdateInstallFailure {
                presentation: crate::presentation::brightness::UserFacingError::new(
                    "Install failed",
                    "Try again.",
                ),
                diagnostic: "installer diagnostic".into(),
                cancelled: false,
            }),
        )
        .unwrap();

        let operation = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        let mut current = report(UpdateChannel::Stable, false);
        current.warning = Some("Cache warning".into());
        app.complete_update_check(operation, Ok(current)).unwrap();

        let card = app.presentation().updater();
        assert_eq!(card.title(), "No newer release available");
        assert!(!card.is_error());
        assert_eq!(
            card.action().map(SettingsAction::label),
            Some("Check for updates")
        );
        assert_eq!(card.warning(), Some("Cache warning"));
        assert_eq!(card.details_title(), "Last update failure");
        assert_eq!(card.details(), Some("installer diagnostic"));
    }

    #[test]
    fn failed_install_with_changed_channel_offers_a_fresh_check() {
        let mut app = application("stable");
        check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let preparation = prepare(&mut app);
        app.complete_update_install(
            &preparation,
            Err(UpdateInstallFailure {
                presentation: crate::presentation::brightness::UserFacingError::new(
                    "Install failed",
                    "Try again.",
                ),
                diagnostic: "installer diagnostic".into(),
                cancelled: false,
            }),
        )
        .unwrap();

        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        app.complete_read(refresh.read_operation().unwrap(), Ok(groups("prerelease")))
            .unwrap();
        let card = app.presentation().updater();
        assert!(card.is_error());
        assert_eq!(
            card.action().map(SettingsAction::label),
            Some("Check for updates")
        );
        assert_eq!(
            card.action().map(SettingsAction::intent),
            Some(SettingsIntent::CheckForUpdates)
        );
    }

    #[test]
    fn warning_is_only_visible_with_completed_check_and_restart_failure_keeps_retry() {
        let mut app = application("stable");
        let operation = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        let mut checked = report(UpdateChannel::Stable, false);
        checked.warning = Some("Cache warning".into());
        app.complete_update_check(operation, Ok(checked)).unwrap();
        assert_eq!(
            app.presentation().updater().warning(),
            Some("Cache warning")
        );

        let operation = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        assert_eq!(app.presentation().updater().warning(), None);
        app.complete_update_check(
            operation,
            Err(crate::settings_view::UpdateCheckError::stopped()),
        )
        .unwrap();
        assert_eq!(app.presentation().updater().warning(), None);

        let mut app = application("stable");
        check(&mut app, Ok(report(UpdateChannel::Stable, true)));
        let preparation = prepare(&mut app);
        app.complete_update_install(&preparation, Ok(UpdateInstallOutcome::Prepared(prepared())))
            .unwrap();
        let install = app
            .handle_intent(SettingsIntent::ConfirmUpdateInstall)
            .unwrap()
            .update_install_operation()
            .unwrap()
            .clone();
        let installed = InstalledUpdate::from_parts(
            "1.7.0".parse().unwrap(),
            UpdateChannel::Stable,
            "v1.7.0",
            "x86_64-unknown-linux-musl",
            "a".repeat(40),
            "/unused/lg-buddy",
            "/unused/lg-buddy-gui",
        );
        let restarting = app
            .complete_update_install(&install, Ok(UpdateInstallOutcome::Installed(installed)))
            .unwrap();
        let relaunch = restarting.update_install_operation().unwrap().clone();
        app.complete_update_install(&relaunch, Err(UpdateInstallFailure::stopped()))
            .unwrap();
        let restart_failure = app.presentation().updater();
        assert!(restart_failure.is_error());
        assert_eq!(restart_failure.title(), "Update stopped");
        assert_eq!(
            restart_failure.action().map(SettingsAction::label),
            Some("Retry restart")
        );
        assert_eq!(
            restart_failure.action().map(SettingsAction::intent),
            Some(SettingsIntent::RelaunchUpdatedApplication)
        );
    }
}
