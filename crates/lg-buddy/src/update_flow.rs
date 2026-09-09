//! Application-owned update installation and process handoff.

use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::presentation::brightness::UserFacingError;
use crate::presentation::settings::SettingsAction;
use crate::presentation::update_check::UpdateCheckReport;
use crate::presentation::update_install::UpdateInstallPresentation;
use crate::settings_view::SettingsIntent;
use crate::update_install::{
    install_gui_update, prepare_gui_update, InstalledUpdate, PreparedUpdateInstall,
    UpdateInstallCancellation, UpdateInstallError, UpdateInstallStage,
};
use crate::updates::UpdateChannel;

#[derive(Debug, Clone)]
pub enum UpdateInstallTask {
    Prepare {
        version: String,
        url: String,
        channel: UpdateChannel,
    },
    Install {
        prepared: PreparedUpdateInstall,
        cancellation: UpdateInstallCancellation,
    },
    Relaunch(InstalledUpdate),
}

#[derive(Debug, Clone)]
pub struct UpdateInstallOperation {
    id: u64,
    task: UpdateInstallTask,
}

impl PartialEq for UpdateInstallOperation {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for UpdateInstallOperation {}

impl UpdateInstallOperation {
    pub fn task(&self) -> &UpdateInstallTask {
        &self.task
    }
}

#[derive(Debug)]
pub enum UpdateInstallOutcome {
    Prepared(PreparedUpdateInstall),
    Installed(InstalledUpdate),
    Relaunched,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInstallFailure {
    pub presentation: UserFacingError,
    pub diagnostic: String,
    pub cancelled: bool,
}

impl UpdateInstallFailure {
    pub fn worker_stopped(operation: &UpdateInstallOperation) -> Self {
        if matches!(operation.task(), UpdateInstallTask::Install { cancellation, .. }
            if !cancellation.can_cancel() && !cancellation.is_cancelled())
        {
            return Self::stopped();
        }
        Self {
            presentation: UserFacingError::new(
                "Update stopped",
                "The update stopped before installation began. Try again.",
            ),
            diagnostic: "update preparation worker stopped without a result".into(),
            cancelled: false,
        }
    }

    pub fn stopped() -> Self {
        Self {
            presentation: UserFacingError::new("Update stopped", "The update did not finish. If installation had begun, the installation may be incomplete. Review the error details before trying again."),
            diagnostic: "update installation worker stopped without a result".into(),
            cancelled: false,
        }
    }
}

impl From<UpdateInstallError> for UpdateInstallFailure {
    fn from(error: UpdateInstallError) -> Self {
        let cancelled = matches!(error, UpdateInstallError::Cancelled);
        let detail = error.user_message();
        let summary = match &error {
            UpdateInstallError::InstallerFailedWithOutput {
                mutation_started: true,
                ..
            } => "Update incomplete",
            _ => "Could not install update",
        };
        Self {
            presentation: UserFacingError::new(summary, &detail),
            diagnostic: error.to_string(),
            cancelled,
        }
    }
}

pub trait UpdateInstallBackend: Send + Sync + 'static {
    fn run(
        &self,
        operation: &UpdateInstallOperation,
        progress: &mut dyn FnMut(UpdateInstallStage),
    ) -> Result<UpdateInstallOutcome, UpdateInstallFailure>;

    /// Runs on the UI thread after the worker has verified both installed binaries.
    /// A successful production handoff replaces this process, releasing its bus name.
    fn relaunch(&self, installed: &InstalledUpdate) -> Result<(), UpdateInstallFailure>;
}

#[derive(Debug, Default)]
pub struct EnvironmentUpdateInstallBackend;

impl UpdateInstallBackend for EnvironmentUpdateInstallBackend {
    fn run(
        &self,
        operation: &UpdateInstallOperation,
        progress: &mut dyn FnMut(UpdateInstallStage),
    ) -> Result<UpdateInstallOutcome, UpdateInstallFailure> {
        match operation.task() {
            UpdateInstallTask::Prepare {
                version,
                url,
                channel,
            } => prepare_gui_update(version, url, *channel)
                .map(UpdateInstallOutcome::Prepared)
                .map_err(Into::into),
            UpdateInstallTask::Install {
                prepared,
                cancellation,
            } => install_gui_update(prepared, cancellation, progress)
                .map(UpdateInstallOutcome::Installed)
                .map_err(Into::into),
            UpdateInstallTask::Relaunch(_) => Err(UpdateInstallFailure::stopped()),
        }
    }

    fn relaunch(&self, installed: &InstalledUpdate) -> Result<(), UpdateInstallFailure> {
        let error = Command::new(installed.gui_path())
            .arg("--gapplication-replace")
            .exec();
        Err(UpdateInstallFailure {
            presentation: UserFacingError::new("Update installed; restart failed", "The installed application could not be started. Retry restarting, or close LG Buddy and open it from the application menu."),
            diagnostic: format!("could not replace the running GUI with the installed GUI: {error}"),
            cancelled: false,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct UpdateInstallApplication {
    next_id: u64,
    pending: Option<UpdateInstallOperation>,
    prepared: Option<PreparedUpdateInstall>,
    installed: Option<InstalledUpdate>,
    presentation: UpdateInstallPresentation,
    cancelling: bool,
}

impl UpdateInstallApplication {
    pub(crate) fn presentation(&self) -> &UpdateInstallPresentation {
        &self.presentation
    }

    pub(crate) fn active(&self) -> bool {
        self.pending.is_some() || self.prepared.is_some()
    }

    pub(crate) fn can_close(&self) -> bool {
        match self.pending.as_ref().map(|operation| &operation.task) {
            Some(UpdateInstallTask::Install { cancellation, .. }) => {
                cancellation.is_cancelled() || cancellation.cancel()
            }
            Some(UpdateInstallTask::Relaunch(_)) => false,
            _ => true,
        }
    }

    pub(crate) fn refresh_offer(
        &mut self,
        report: Option<&UpdateCheckReport>,
        channel: Option<UpdateChannel>,
        available: bool,
    ) {
        if self.active() || self.installed.is_some() {
            return;
        }
        let offered = report.is_some_and(|report| report.available_release.is_some());
        self.presentation.offer_channel_matches =
            offered.then(|| report.is_some_and(|report| Some(report.channel) == channel));
        self.presentation.action = offered.then(|| {
            SettingsAction::new(
                if self.presentation.error.is_some() {
                    "Retry update"
                } else {
                    "Install update…"
                },
                available && report.is_some_and(|report| Some(report.channel) == channel),
                SettingsIntent::PrepareUpdateInstall,
            )
        });
        if offered && self.presentation.title.is_none() {
            self.presentation.title = Some("Install update".into());
        }
        if !offered && self.presentation.error.is_none() {
            self.presentation.title = None;
        }
        if self.presentation.error.is_none() {
            self.presentation.description =
                if offered && report.is_some_and(|report| Some(report.channel) != channel) {
                    "Check for updates on the current channel before installing.".into()
                } else {
                    String::new()
                };
        }
    }

    fn start(&mut self, task: UpdateInstallTask) -> UpdateInstallOperation {
        self.next_id += 1;
        let operation = UpdateInstallOperation {
            id: self.next_id,
            task,
        };
        self.pending = Some(operation.clone());
        self.presentation.busy = true;
        self.presentation.action = None;
        self.presentation.error = None;
        self.cancelling = false;
        operation
    }

    fn clear_presentation(&mut self) {
        let failure_details = self.presentation.failure_details.take();
        self.presentation = UpdateInstallPresentation {
            failure_details,
            ..Default::default()
        };
    }

    pub(crate) fn handle(
        &mut self,
        intent: SettingsIntent,
        report: Option<&UpdateCheckReport>,
    ) -> Option<Option<UpdateInstallOperation>> {
        match intent {
            SettingsIntent::PrepareUpdateInstall
                if !self.active()
                    && self
                        .presentation
                        .action
                        .as_ref()
                        .is_some_and(SettingsAction::enabled) =>
            {
                let report = report?;
                let release = report.available_release.as_ref()?;
                let operation = self.start(UpdateInstallTask::Prepare {
                    version: release.version.clone(),
                    url: release.url.clone(),
                    channel: report.channel,
                });
                self.presentation.title = Some("Preparing update…".into());
                self.presentation.description =
                    "Checking this release and installation before confirmation.".into();
                self.presentation.cancel_action = Some(cancel_action());
                Some(Some(operation))
            }
            SettingsIntent::ConfirmUpdateInstall if self.pending.is_none() => {
                let prepared = self.prepared.take()?;
                let operation = self.start(UpdateInstallTask::Install {
                    prepared,
                    cancellation: UpdateInstallCancellation::default(),
                });
                self.presentation.title = Some("Downloading and verifying update…".into());
                self.presentation.description = "You can cancel before installation begins.".into();
                self.presentation.cancel_action = Some(cancel_action());
                Some(Some(operation))
            }
            SettingsIntent::CancelUpdateInstall if self.active() && !self.cancelling => {
                if let Some(UpdateInstallOperation {
                    task: UpdateInstallTask::Install { cancellation, .. },
                    ..
                }) = &self.pending
                {
                    if !cancellation.cancel() {
                        return None;
                    }
                    self.cancelling = true;
                    self.presentation.title = Some("Cancelling update…".into());
                    self.presentation.description = "Waiting for the current preparation step to finish. No installation will start.".into();
                    self.presentation.cancel_action = None;
                } else {
                    self.pending = None;
                    self.prepared = None;
                    self.clear_presentation();
                }
                Some(None)
            }
            SettingsIntent::RelaunchUpdatedApplication if self.pending.is_none() => {
                let installed = self.installed.clone()?;
                self.presentation.title = Some("Restarting LG Buddy…".into());
                self.presentation.cancel_action = None;
                Some(Some(self.start(UpdateInstallTask::Relaunch(installed))))
            }
            _ => None,
        }
    }

    pub(crate) fn progress(
        &mut self,
        operation: &UpdateInstallOperation,
        stage: UpdateInstallStage,
    ) -> bool {
        if self.pending.as_ref() != Some(operation) || self.cancelling {
            return false;
        }
        let (title, detail, cancellable) = match stage {
            UpdateInstallStage::InitialPreflight | UpdateInstallStage::Discovering | UpdateInstallStage::Resolving | UpdateInstallStage::Offered => ("Preparing update…", "Checking this release and installation before confirmation.", true),
            UpdateInstallStage::Acquiring => ("Downloading and verifying update…", "You can cancel before installation begins.", true),
            UpdateInstallStage::CandidatePreflight => ("Checking installation compatibility…", "You can cancel before installation begins.", true),
            UpdateInstallStage::Installing => ("Installing update…", "Authorize the update when prompted. Keep LG Buddy open until installation finishes.", false),
            UpdateInstallStage::VerifyingInstalled => ("Verifying installed update…", "LG Buddy will restart after verification.", false),
        };
        self.presentation.title = Some(title.into());
        self.presentation.description = detail.into();
        self.presentation.cancel_action = cancellable.then(cancel_action);
        true
    }

    pub(crate) fn complete(
        &mut self,
        operation: &UpdateInstallOperation,
        result: Result<UpdateInstallOutcome, UpdateInstallFailure>,
    ) -> Option<(Option<UpdateInstallOperation>, Option<String>)> {
        if self.pending.as_ref() != Some(operation) {
            return None;
        }
        self.pending = None;
        self.presentation.busy = false;
        self.presentation.cancel_action = None;
        match result {
            Ok(UpdateInstallOutcome::Prepared(prepared))
                if matches!(operation.task, UpdateInstallTask::Prepare { .. }) =>
            {
                self.presentation.title = Some(format!(
                    "Install LG Buddy {}?",
                    prepared.identity().version()
                ));
                self.presentation.description = format!("{} channel. LG Buddy will request authorization, install this release, and restart. Your TV pairing and settings will be kept.", prepared.channel().as_str());
                self.prepared = Some(prepared);
                self.presentation.action = Some(SettingsAction::new(
                    "Install and restart",
                    true,
                    SettingsIntent::ConfirmUpdateInstall,
                ));
                self.presentation.cancel_action = Some(cancel_action());
            }
            Ok(UpdateInstallOutcome::Installed(installed))
                if matches!(operation.task, UpdateInstallTask::Install { .. }) =>
            {
                self.installed = Some(installed.clone());
                self.presentation.title = Some("Restarting LG Buddy…".into());
                self.presentation.description = "The installed update was verified.".into();
                return Some((
                    Some(self.start(UpdateInstallTask::Relaunch(installed))),
                    None,
                ));
            }
            Ok(UpdateInstallOutcome::Relaunched)
                if matches!(operation.task, UpdateInstallTask::Relaunch(_)) => {}
            Ok(_) => return self.failed(UpdateInstallFailure::worker_stopped(operation)),
            Err(error) if error.cancelled => {
                self.clear_presentation();
            }
            Err(error) => return self.failed(error),
        }
        Some((None, None))
    }

    fn failed(
        &mut self,
        error: UpdateInstallFailure,
    ) -> Option<(Option<UpdateInstallOperation>, Option<String>)> {
        self.presentation.title = Some(
            if self.installed.is_some() {
                "Restart required"
            } else {
                "Update not completed"
            }
            .into(),
        );
        self.presentation.description.clear();
        self.presentation.failure_details = Some(retained_failure_details(&error.diagnostic));
        self.presentation.error = Some(error.presentation);
        if self.installed.is_some() {
            self.presentation.action = Some(SettingsAction::new(
                "Retry restart",
                true,
                SettingsIntent::RelaunchUpdatedApplication,
            ));
        }
        Some((None, Some(error.diagnostic)))
    }
}

/// Retain only bounded plain text from updater diagnostics. The installer does
/// not read TV credentials; also omit credential-bearing lines and URLs from
/// subprocess/network errors so an on-demand view need not expose them.
pub(crate) fn retained_failure_details(diagnostic: &str) -> String {
    const LIMIT: usize = 64 * 1024;
    let mut result = String::new();
    for line in diagnostic.lines() {
        let line: String = line
            .chars()
            .filter(|character| {
                (!character.is_control() || *character == '\t')
                    && !matches!(*character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
            .collect();
        let lower = line.to_ascii_lowercase();
        let secret = [
            "password",
            "passwd",
            "token",
            "client-key",
            "client_key",
            "secret",
            "authorization:",
            "bearer ",
        ]
        .iter()
        .any(|key| lower.contains(key));
        let line = if secret {
            "[Credential-bearing output omitted]".to_string()
        } else {
            line.split_inclusive(char::is_whitespace)
                .map(|part| {
                    if part.contains("://") {
                        format!("[URL omitted]{}", &part[part.trim_end().len()..])
                    } else {
                        part.to_string()
                    }
                })
                .collect()
        };
        let remaining = LIMIT.saturating_sub(result.len());
        if line.len() + 1 > remaining {
            let mut end = remaining;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            result.push_str(&line[..end]);
            result.push_str("\n[Details truncated]");
            break;
        }
        result.push_str(&line);
        result.push('\n');
    }
    result.trim().to_string()
}

fn cancel_action() -> SettingsAction {
    SettingsAction::new("Cancel", true, SettingsIntent::CancelUpdateInstall)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::update_check::AvailableUpdate;
    use crate::settings::ConfigEnvReader;
    use crate::settings_view::{BehaviorSetting, SettingsApplication};
    use crate::version::VersionInfo;

    fn prepared() -> PreparedUpdateInstall {
        PreparedUpdateInstall::from_parts(
            VersionInfo::current(),
            "1.7.0".parse().unwrap(),
            UpdateChannel::Stable,
            "https://github.com/Staphylococcus/LG_Buddy/releases/tag/v1.7.0",
            "v1.7.0",
            "x86_64-unknown-linux-musl",
            "a".repeat(40),
        )
    }

    fn offered() -> SettingsApplication {
        let (mut app, opening) = SettingsApplication::open();
        let store = ConfigEnvReader::parse(
            "/tmp/unused-update-fixture.env",
            "updates_auto_check=disabled\nupdates_channel=stable\n",
        )
        .into_store();
        let groups = crate::presentation::settings::SettingsPresentation::from_store(&store)
            .groups()
            .to_vec();
        app.complete_read(opening.read_operation().unwrap(), Ok(groups))
            .unwrap();
        let operation = app
            .handle_intent(SettingsIntent::CheckForUpdates)
            .unwrap()
            .update_check_operation()
            .unwrap();
        app.complete_update_check(
            operation,
            Ok(UpdateCheckReport {
                installed_version: "1.6.0".into(),
                channel: UpdateChannel::Stable,
                available_release: Some(AvailableUpdate {
                    version: "1.7.0".into(),
                    url: prepared().release().url().into(),
                }),
                warning: None,
            }),
        )
        .unwrap();
        app
    }

    fn prepare(app: &mut SettingsApplication) -> UpdateInstallOperation {
        app.handle_intent(SettingsIntent::PrepareUpdateInstall)
            .unwrap()
            .update_install_operation()
            .unwrap()
            .clone()
    }

    #[test]
    fn confirmation_is_required_and_cancelled_or_closed_preparations_cannot_start_an_install() {
        let mut app = offered();
        assert!(app
            .presentation()
            .update_install()
            .action()
            .unwrap()
            .enabled());
        let old = prepare(&mut app);
        assert!(app
            .handle_intent(SettingsIntent::PrepareUpdateInstall)
            .is_none());
        assert!(app
            .handle_intent(SettingsIntent::ConfirmUpdateInstall)
            .is_none());
        assert!(app.handle_intent(SettingsIntent::CheckForUpdates).is_none());
        assert!(app
            .handle_intent(SettingsIntent::SetEnabled {
                setting: BehaviorSetting::UpdatesAutoCheck,
                enabled: true
            })
            .is_none());
        app.handle_intent(SettingsIntent::CancelUpdateInstall)
            .unwrap();
        let current = prepare(&mut app);
        assert!(app
            .complete_update_install(&old, Ok(UpdateInstallOutcome::Prepared(prepared())))
            .is_none());
        let confirmation = app
            .complete_update_install(&current, Ok(UpdateInstallOutcome::Prepared(prepared())))
            .unwrap();
        assert!(confirmation.update_install_operation().is_none());
        assert_eq!(
            confirmation
                .presentation()
                .update_install()
                .action()
                .unwrap()
                .label(),
            "Install and restart"
        );
        assert!(!confirmation
            .presentation()
            .update_check()
            .check_action()
            .enabled());
        assert!(app
            .handle_intent(SettingsIntent::PrepareUpdateInstall)
            .is_none());
        app.handle_intent(SettingsIntent::CancelUpdateInstall)
            .unwrap();
        assert!(app
            .handle_intent(SettingsIntent::ConfirmUpdateInstall)
            .is_none());
        let closing = prepare(&mut app);
        app.shutdown();
        assert!(app
            .complete_update_install(&closing, Ok(UpdateInstallOutcome::Prepared(prepared())))
            .is_none());
    }

    #[test]
    fn cancellation_waits_for_cleanup_and_no_install_or_handoff_is_reported() {
        let mut app = offered();
        let preparation = prepare(&mut app);
        app.complete_update_install(&preparation, Ok(UpdateInstallOutcome::Prepared(prepared())))
            .unwrap();
        let install = app
            .handle_intent(SettingsIntent::ConfirmUpdateInstall)
            .unwrap()
            .update_install_operation()
            .unwrap()
            .clone();
        app.handle_intent(SettingsIntent::CancelUpdateInstall)
            .unwrap();
        let UpdateInstallTask::Install { cancellation, .. } = install.task() else {
            panic!("install task");
        };
        assert!(cancellation.is_cancelled());
        assert!(
            app.is_mutating(),
            "download cleanup must finish before a new operation"
        );
        assert!(
            app.can_close(),
            "an already cancelled download must not trap the window open"
        );
        assert!(app
            .handle_intent(SettingsIntent::PrepareUpdateInstall)
            .is_none());
        let cancelled = app
            .complete_update_install(&install, Err(UpdateInstallError::Cancelled.into()))
            .unwrap();
        assert!(cancelled.update_install_operation().is_none());
        assert!(!app.is_mutating());
        assert!(cancelled.presentation().update_install().error().is_none());
    }

    #[test]
    fn failures_allow_retry_and_verified_success_requests_only_an_installed_gui_handoff() {
        let mut app = offered();
        let preparation = prepare(&mut app);
        app.complete_update_install(&preparation, Err(UpdateInstallFailure::stopped()))
            .unwrap();
        assert_eq!(
            app.presentation()
                .update_install()
                .action()
                .unwrap()
                .label(),
            "Retry update"
        );
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
            "/usr/bin/lg-buddy",
            "/usr/bin/lg-buddy-gui",
        );
        let completion = app
            .complete_update_install(
                &install,
                Ok(UpdateInstallOutcome::Installed(installed.clone())),
            )
            .unwrap();
        let handoff = completion.update_install_operation().unwrap();
        assert!(
            matches!(handoff.task(), UpdateInstallTask::Relaunch(result) if result == &installed)
        );
        let failed = app
            .complete_update_install(handoff, Err(UpdateInstallFailure::stopped()))
            .unwrap();
        assert_eq!(
            failed
                .presentation()
                .update_install()
                .action()
                .unwrap()
                .intent(),
            SettingsIntent::RelaunchUpdatedApplication
        );
        assert!(app
            .handle_intent(SettingsIntent::ConfirmUpdateInstall)
            .is_none());
    }

    #[test]
    fn failure_details_survive_refresh_retry_and_cancellation_without_terminal_output() {
        let mut app = offered();
        let operation = prepare(&mut app);
        let failed = app
            .complete_update_install(
                &operation,
                Err(UpdateInstallError::InstallerFailedWithOutput {
                    code: Some(1),
                    output: "install: cannot create regular file: No space left on device\naccess_token=private-value".into(),
                    mutation_started: true,
                }.into()),
            )
            .unwrap();
        let details = failed
            .presentation()
            .update_install()
            .failure_details()
            .unwrap()
            .to_string();
        assert!(details.contains("No space left on device"));
        assert!(details.contains("exit status 1"));
        assert!(!details.contains("private-value"));
        assert!(!failed
            .presentation()
            .update_install()
            .error()
            .unwrap()
            .detail()
            .contains("No space left"));

        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        let groups = app.presentation().groups().to_vec();
        app.complete_read(refresh.read_operation().unwrap(), Ok(groups))
            .unwrap();
        assert_eq!(
            app.presentation().update_install().failure_details(),
            Some(details.as_str())
        );
        prepare(&mut app);
        assert!(app.presentation().update_install().error().is_none());
        assert_eq!(
            app.presentation().update_install().failure_details_title(),
            "Last update failure"
        );
        app.handle_intent(SettingsIntent::CancelUpdateInstall)
            .unwrap();
        assert_eq!(
            app.presentation().update_install().failure_details(),
            Some(details.as_str())
        );
    }

    #[test]
    fn retained_failure_details_remove_credentials_urls_controls_and_bound_unicode_text() {
        let details = super::retained_failure_details(
            "install: No space left on device\npassword=hunter2\nAuthorization: Bearer abc\nclient-key=key123\nfetch failed https://user:pass@example.test/asset?signed=value\nordinary\0text\u{202e}",
        );
        for secret in ["hunter2", "abc", "key123", "user:pass", "signed=value"] {
            assert!(!details.contains(secret), "leaked {secret}");
        }
        assert!(details.contains("No space left on device"));
        assert!(details.contains("fetch failed [URL omitted]"));
        assert!(details.contains("ordinarytext"));
        assert!(!details.contains('\0'));
        assert!(!details.contains('\u{202e}'));
        let large = super::retained_failure_details(&"☃".repeat(30_000));
        assert!(large.len() <= 64 * 1024 + "\n[Details truncated]".len());
        assert!(large.ends_with("[Details truncated]"));
    }

    #[test]
    fn an_old_channel_result_is_retained_but_cannot_be_installed() {
        let mut app = offered();
        let refresh = app
            .handle_intent(SettingsIntent::Refresh)
            .unwrap()
            .read_operation()
            .unwrap();
        let store = ConfigEnvReader::parse(
            "/tmp/unused-update-fixture.env",
            "updates_channel=prerelease\n",
        )
        .into_store();
        app.complete_read(
            refresh,
            Ok(
                crate::presentation::settings::SettingsPresentation::from_store(&store)
                    .groups()
                    .to_vec(),
            ),
        )
        .unwrap();
        assert_eq!(
            app.presentation().update_check().result().unwrap().channel,
            UpdateChannel::Stable
        );
        assert!(!app
            .presentation()
            .update_install()
            .action()
            .unwrap()
            .enabled());
        assert!(app
            .handle_intent(SettingsIntent::PrepareUpdateInstall)
            .is_none());
    }
}
