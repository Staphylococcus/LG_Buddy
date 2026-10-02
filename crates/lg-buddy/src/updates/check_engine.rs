// Update check engine: `run_updates_command` / `check_for_updates` / `saved_update_channel` /
// `discover_install_candidate_for_channel` and the internal engine
// (`prepare_update_check`, `run_update_check`, `check_updates_with_cache`), the `UpdateCheckResult` /
// `UpdateCheckOutcome` value types, and the `UpdatesRunContext` / `PreparedUpdateCheck` helpers.
// Moved verbatim from updates.rs; the entry points the parent re-exports for external callers
// stay `pub` / `pub(crate)`, and the items the parent's colocated tests touch are reachable
// through the parent's private `use check_engine::{...}` import.
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;

use crate::session_notifications::{
    SessionBusUpdateNotificationHandoff, UpdateNotificationError, UpdateNotificationHandoff,
    UpdateNotificationRequest,
};
use crate::version::{ReleaseChannel, VersionInfo};

use super::cache::{DefaultUpdateCacheStore, UpdateCacheStore};
use super::command::UpdatesCommand;
use super::github::{fetch_latest_release, GitHubReleasesClient, UreqGitHubReleasesClient};
use super::notification::{
    evaluate_update_notification_policy, render_update_notification_failure,
    render_update_notification_sent, render_update_notification_skip, UpdateNotificationDecision,
    UpdateNotificationPolicyInput,
};
#[cfg(test)]
use super::StaticUpdateSettings;
use super::{
    EnvUpdateSettings, ReleaseInfo, UpdateChannel, UpdateCheckCache, UpdateSettings,
    UpdatesDeferredFailure, UpdatesError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckResult {
    pub(super) check_channel: UpdateChannel,
    pub(super) current_version: Version,
    pub(super) current_channel: ReleaseChannel,
    pub(super) latest: ReleaseInfo,
}

impl UpdateCheckResult {
    pub fn check_channel(&self) -> UpdateChannel {
        self.check_channel
    }

    pub fn current_version(&self) -> &Version {
        &self.current_version
    }

    pub fn current_channel(&self) -> ReleaseChannel {
        self.current_channel
    }

    pub fn latest(&self) -> &ReleaseInfo {
        &self.latest
    }

    pub fn update_available(&self) -> bool {
        self.latest.version > self.current_version
    }

    pub fn render(&self) -> String {
        let status = if self.update_available() {
            "update available"
        } else {
            "up to date"
        };

        let mut output = format!(
            "status: {status}\ncurrent: {} ({})\nlatest: {} ({})\nurl: {}\n",
            self.current_version,
            self.current_channel.as_str(),
            self.latest.version(),
            self.latest.channel().as_str(),
            self.latest.url()
        );
        if self.update_available() {
            output.push_str("install: lg-buddy updates install\n");
        }
        output
    }

    fn notification_request(&self) -> Result<UpdateNotificationRequest, UpdateNotificationError> {
        UpdateNotificationRequest::new(
            self.check_channel,
            self.current_version.clone(),
            self.current_channel,
            self.latest.version().clone(),
            self.latest.channel(),
            self.latest.url().to_string(),
        )
    }
}

#[derive(Debug)]
pub struct UpdateCheckOutcome {
    pub(super) result: UpdateCheckResult,
    pub(super) warnings: Vec<UpdatesDeferredFailure>,
}

impl UpdateCheckOutcome {
    pub fn result(&self) -> &UpdateCheckResult {
        &self.result
    }

    pub fn warnings(&self) -> &[UpdatesDeferredFailure] {
        &self.warnings
    }
}

pub fn run_updates_command<W: io::Write>(
    command: UpdatesCommand,
    writer: &mut W,
) -> Result<(), UpdatesError> {
    if matches!(command, UpdatesCommand::Install) {
        return Err(UpdatesError::CommandInvariant(
            "updates install must use the install orchestrator".to_string(),
        ));
    }
    let client = UreqGitHubReleasesClient::default();
    let version = VersionInfo::current();
    let notification_handoff = SessionBusUpdateNotificationHandoff;
    let cache_store = DefaultUpdateCacheStore::from_env();
    let update_settings = EnvUpdateSettings::from_env()?;
    let context = UpdatesRunContext {
        version,
        client: &client,
        notifier: &notification_handoff,
        cache_store: &cache_store,
        update_settings: &update_settings,
        now_unix_seconds: current_unix_seconds(),
    };

    run_updates_command_with_update_settings(command, writer, context)
}

pub fn check_for_updates() -> Result<UpdateCheckOutcome, UpdatesError> {
    let client = UreqGitHubReleasesClient::default();
    let cache_store = DefaultUpdateCacheStore::from_env();
    let update_settings = EnvUpdateSettings::from_env()?;

    run_update_check(
        VersionInfo::current(),
        &client,
        &cache_store,
        &update_settings,
        current_unix_seconds(),
    )
}

pub(crate) fn saved_update_channel() -> Result<UpdateChannel, UpdatesError> {
    EnvUpdateSettings::from_env()?.channel()
}

pub(crate) fn discover_install_candidate_for_channel(
    current: VersionInfo,
    channel: UpdateChannel,
) -> Result<ReleaseInfo, UpdatesError> {
    let client = UreqGitHubReleasesClient::default();
    check_updates(channel, current, &client).map(|result| result.latest)
}

#[cfg(test)]
pub(super) fn discover_install_candidate_with<C: GitHubReleasesClient, U: UpdateSettings>(
    current: VersionInfo,
    client: &C,
    settings: &U,
) -> Result<ReleaseInfo, UpdatesError> {
    let channel = settings.channel()?;
    check_updates(channel, current, client).map(|result| result.latest)
}

#[cfg(test)]
pub(super) fn run_updates_command_with<
    W: io::Write,
    C: GitHubReleasesClient,
    N: UpdateNotificationHandoff,
    S: UpdateCacheStore,
>(
    command: UpdatesCommand,
    writer: &mut W,
    version: VersionInfo,
    client: &C,
    notifier: &N,
    cache_store: &S,
    now_unix_seconds: u64,
) -> Result<(), UpdatesError> {
    let update_settings = StaticUpdateSettings::enabled(UpdateChannel::Stable);
    let context = UpdatesRunContext {
        version,
        client,
        notifier,
        cache_store,
        update_settings: &update_settings,
        now_unix_seconds,
    };
    run_updates_command_with_update_settings(command, writer, context)
}

pub(super) struct UpdatesRunContext<'a, C, N, S, U> {
    pub(super) version: VersionInfo,
    pub(super) client: &'a C,
    pub(super) notifier: &'a N,
    pub(super) cache_store: &'a S,
    pub(super) update_settings: &'a U,
    pub(super) now_unix_seconds: u64,
}

struct PreparedUpdateCheck {
    result: UpdateCheckResult,
    cache: UpdateCheckCache,
    deferred_failures: Vec<UpdatesDeferredFailure>,
}

fn prepare_update_check<C: GitHubReleasesClient, S: UpdateCacheStore, U: UpdateSettings>(
    version: VersionInfo,
    client: &C,
    cache_store: &S,
    update_settings: &U,
    now_unix_seconds: u64,
) -> Result<PreparedUpdateCheck, UpdatesError> {
    let channel = update_settings.channel()?;
    let mut deferred_failures = Vec::new();
    let mut cache = match cache_store.load() {
        Ok(cache) => cache,
        Err(err) => {
            deferred_failures.push(UpdatesDeferredFailure::Cache(Box::new(err)));
            UpdateCheckCache::default()
        }
    };
    let result = check_updates_with_cache(channel, version, client, &mut cache, now_unix_seconds)?;

    Ok(PreparedUpdateCheck {
        result,
        cache,
        deferred_failures,
    })
}

pub(super) fn run_update_check<C: GitHubReleasesClient, S: UpdateCacheStore, U: UpdateSettings>(
    version: VersionInfo,
    client: &C,
    cache_store: &S,
    update_settings: &U,
    now_unix_seconds: u64,
) -> Result<UpdateCheckOutcome, UpdatesError> {
    let mut prepared = prepare_update_check(
        version,
        client,
        cache_store,
        update_settings,
        now_unix_seconds,
    )?;
    if let Err(err) = cache_store.save(&prepared.cache) {
        prepared
            .deferred_failures
            .push(UpdatesDeferredFailure::Cache(Box::new(err)));
    }

    Ok(UpdateCheckOutcome {
        result: prepared.result,
        warnings: prepared.deferred_failures,
    })
}

pub(super) fn run_updates_command_with_update_settings<
    W: io::Write,
    C: GitHubReleasesClient,
    N: UpdateNotificationHandoff,
    S: UpdateCacheStore,
    U: UpdateSettings,
>(
    command: UpdatesCommand,
    writer: &mut W,
    context: UpdatesRunContext<'_, C, N, S, U>,
) -> Result<(), UpdatesError> {
    if matches!(command, UpdatesCommand::BackgroundCheck)
        && !context.update_settings.automatic_checks_enabled()?
    {
        writer.write_all(b"background: skipped (automatic update checks disabled)\n")?;
        return Ok(());
    }
    let notify = command.notify();
    let mut prepared = prepare_update_check(
        context.version,
        context.client,
        context.cache_store,
        context.update_settings,
        context.now_unix_seconds,
    )?;
    let result = &prepared.result;

    writer.write_all(result.render().as_bytes())?;
    let notification_decision =
        evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: notify,
            update_available: result.update_available(),
            latest: &result.latest,
            last_notification: prepared
                .cache
                .entry(result.check_channel)
                .and_then(|entry| entry.last_notification.as_ref()),
        });
    match notification_decision {
        UpdateNotificationDecision::Notify { reason } => {
            let notification_result = result
                .notification_request()
                .and_then(|request| context.notifier.show_update_notification(&request));
            match notification_result {
                Ok(_) => {
                    prepared.cache.record_notification(
                        result.check_channel,
                        &result.latest,
                        context.now_unix_seconds,
                    );
                    writer.write_all(render_update_notification_sent(reason).as_bytes())?;
                }
                Err(err) => {
                    writer.write_all(render_update_notification_failure(reason).as_bytes())?;
                    prepared
                        .deferred_failures
                        .push(UpdatesDeferredFailure::Notification(err));
                }
            }
        }
        UpdateNotificationDecision::Skip { reason } => {
            if notify {
                writer.write_all(
                    render_update_notification_skip(reason, &result.latest).as_bytes(),
                )?;
            }
        }
    }
    if let Err(err) = context.cache_store.save(&prepared.cache) {
        prepared
            .deferred_failures
            .push(UpdatesDeferredFailure::Cache(Box::new(err)));
    }

    if !prepared.deferred_failures.is_empty() {
        return Err(UpdatesError::DeferredFailures(prepared.deferred_failures));
    }

    Ok(())
}

pub(super) fn check_updates<C: GitHubReleasesClient>(
    channel: UpdateChannel,
    current: VersionInfo,
    client: &C,
) -> Result<UpdateCheckResult, UpdatesError> {
    let mut cache = UpdateCheckCache::default();
    check_updates_with_cache(channel, current, client, &mut cache, current_unix_seconds())
}

pub(super) fn check_updates_with_cache<C: GitHubReleasesClient>(
    channel: UpdateChannel,
    current: VersionInfo,
    client: &C,
    cache: &mut UpdateCheckCache,
    now_unix_seconds: u64,
) -> Result<UpdateCheckResult, UpdatesError> {
    let current_version =
        Version::parse(current.version()).map_err(|source| UpdatesError::InvalidLocalVersion {
            version: current.version().to_string(),
            source,
        })?;
    let latest = fetch_latest_release(channel, current, client, cache, now_unix_seconds)?;

    Ok(UpdateCheckResult {
        check_channel: channel,
        current_version,
        current_channel: current.channel(),
        latest,
    })
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
