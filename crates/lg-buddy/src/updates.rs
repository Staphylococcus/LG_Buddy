use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::session_notifications::UpdateNotificationError;
use crate::settings::{SettingsError, SettingsStore};

mod cache;
mod check_engine;
mod command;
mod notification;

pub use command::{UpdatesCommand, UpdatesParseError};
mod github;
pub use cache::UpdateCachePathError;
pub use check_engine::{
    check_for_updates, run_updates_command, UpdateCheckOutcome, UpdateCheckResult,
};
pub(crate) use check_engine::{discover_install_candidate_for_channel, saved_update_channel};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Stable,
    Prerelease,
}

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }
}

trait UpdateSettings {
    fn automatic_checks_enabled(&self) -> Result<bool, UpdatesError>;
    fn channel(&self) -> Result<UpdateChannel, UpdatesError>;
}

#[derive(Debug)]
pub(super) struct EnvUpdateSettings {
    store: SettingsStore,
}

impl EnvUpdateSettings {
    fn from_env() -> Result<Self, UpdatesError> {
        Ok(Self {
            store: SettingsStore::load_from_env()?,
        })
    }
}

impl UpdateSettings for EnvUpdateSettings {
    fn automatic_checks_enabled(&self) -> Result<bool, UpdatesError> {
        let store = &self.store;
        match required_enum_setting(store, "updates.auto_check")? {
            "disabled" => Ok(false),
            "enabled" => Ok(true),
            _ => Err(UpdatesError::SettingsInvariant(
                "updates.auto_check resolved to an unsupported value".to_string(),
            )),
        }
    }

    fn channel(&self) -> Result<UpdateChannel, UpdatesError> {
        match required_enum_setting(&self.store, "updates.channel")? {
            "stable" => Ok(UpdateChannel::Stable),
            "prerelease" => Ok(UpdateChannel::Prerelease),
            _ => Err(UpdatesError::SettingsInvariant(
                "updates.channel resolved to an unsupported value".to_string(),
            )),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(super) struct StaticUpdateSettings {
    automatic_checks_enabled: bool,
    channel: UpdateChannel,
}

#[cfg(test)]
impl StaticUpdateSettings {
    fn enabled(channel: UpdateChannel) -> Self {
        Self {
            automatic_checks_enabled: true,
            channel,
        }
    }

    fn disabled(channel: UpdateChannel) -> Self {
        Self {
            automatic_checks_enabled: false,
            channel,
        }
    }
}

#[cfg(test)]
impl UpdateSettings for StaticUpdateSettings {
    fn automatic_checks_enabled(&self) -> Result<bool, UpdatesError> {
        Ok(self.automatic_checks_enabled)
    }

    fn channel(&self) -> Result<UpdateChannel, UpdatesError> {
        Ok(self.channel)
    }
}

fn required_enum_setting(
    store: &SettingsStore,
    key: &'static str,
) -> Result<&'static str, UpdatesError> {
    store
        .effective_by_name(key)?
        .required_value()?
        .as_enum()
        .ok_or_else(|| {
            UpdatesError::SettingsInvariant(format!("{key} resolved to a non-enum value"))
        })
}

#[derive(Debug)]
pub enum UpdatesError {
    Http {
        url: String,
        message: String,
    },
    ApiStatus {
        url: String,
        status: u16,
        body: String,
    },
    ApiShape {
        endpoint: &'static str,
        source: serde_json::Error,
    },
    ResponseTooLarge {
        url: String,
        max_bytes: u64,
    },
    InvalidLocalVersion {
        version: String,
        source: semver::Error,
    },
    NoMatchingRelease {
        channel: UpdateChannel,
    },
    NotModifiedWithoutCache {
        channel: UpdateChannel,
    },
    CachePath(UpdateCachePathError),
    CacheDecode {
        path: PathBuf,
        source: serde_json::Error,
    },
    CacheEncode(serde_json::Error),
    Settings(SettingsError),
    SettingsInvariant(String),
    CommandInvariant(String),
    DeferredFailures(Vec<UpdatesDeferredFailure>),
    Notification(UpdateNotificationError),
    Io(io::Error),
}

#[derive(Debug)]
pub enum UpdatesDeferredFailure {
    Cache(Box<UpdatesError>),
    Notification(UpdateNotificationError),
}

impl fmt::Display for UpdatesDeferredFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cache(err) => write!(f, "update cache failed: {err}"),
            Self::Notification(err) => write!(f, "update notification handoff failed: {err}"),
        }
    }
}

impl Error for UpdatesDeferredFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cache(err) => Some(err.as_ref()),
            Self::Notification(err) => Some(err),
        }
    }
}

impl fmt::Display for UpdatesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { url, message } => {
                write!(f, "could not query GitHub releases API `{url}`: {message}")
            }
            Self::ApiStatus { url, status, body } => {
                if body.trim().is_empty() {
                    write!(
                        f,
                        "GitHub releases API `{url}` returned HTTP status {status}"
                    )
                } else {
                    write!(
                        f,
                        "GitHub releases API `{url}` returned HTTP status {status}: {}",
                        body.trim()
                    )
                }
            }
            Self::ApiShape { endpoint, source } => {
                write!(
                    f,
                    "could not parse GitHub releases API `{endpoint}` response: {source}"
                )
            }
            Self::ResponseTooLarge { url, max_bytes } => write!(
                f,
                "GitHub releases API `{url}` exceeded the {max_bytes}-byte response limit"
            ),
            Self::InvalidLocalVersion { version, source } => {
                write!(f, "invalid local LG Buddy version `{version}`: {source}")
            }
            Self::NoMatchingRelease { channel } => {
                write!(
                    f,
                    "GitHub releases API returned no matching release for {} channel",
                    channel.as_str()
                )
            }
            Self::NotModifiedWithoutCache { channel } => {
                write!(
                    f,
                    "GitHub releases API reported no changes for {} channel, but the local update cache has no usable release metadata",
                    channel.as_str()
                )
            }
            Self::CachePath(err) => write!(f, "{err}"),
            Self::CacheDecode { path, source } => {
                write!(
                    f,
                    "could not parse update check cache `{}`: {source}",
                    path.display()
                )
            }
            Self::CacheEncode(err) => write!(f, "could not encode update check cache: {err}"),
            Self::Settings(err) => write!(f, "could not read update settings: {err}"),
            Self::SettingsInvariant(message) => {
                write!(f, "invalid update settings metadata: {message}")
            }
            Self::CommandInvariant(message) => write!(f, "invalid update command: {message}"),
            Self::DeferredFailures(failures) => {
                write!(f, "update check completed with deferred failure")?;
                if failures.len() != 1 {
                    write!(f, "s")?;
                }
                write!(f, ": ")?;

                for (index, failure) in failures.iter().enumerate() {
                    if index > 0 {
                        write!(f, "; ")?;
                    }
                    write!(f, "{failure}")?;
                }

                Ok(())
            }
            Self::Notification(err) => write!(f, "could not request update notification: {err}"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for UpdatesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ApiShape { source, .. } => Some(source),
            Self::InvalidLocalVersion { source, .. } => Some(source),
            Self::CachePath(err) => Some(err),
            Self::CacheDecode { source, .. } => Some(source),
            Self::CacheEncode(err) => Some(err),
            Self::Settings(err) => Some(err),
            Self::DeferredFailures(failures) => {
                failures.iter().find_map(|failure| failure.source())
            }
            Self::Notification(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Http { .. }
            | Self::ApiStatus { .. }
            | Self::ResponseTooLarge { .. }
            | Self::NoMatchingRelease { .. }
            | Self::NotModifiedWithoutCache { .. }
            | Self::SettingsInvariant(_)
            | Self::CommandInvariant(_) => None,
        }
    }
}

impl From<io::Error> for UpdatesError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<SettingsError> for UpdatesError {
    fn from(value: SettingsError) -> Self {
        Self::Settings(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    version: Version,
    channel: UpdateChannel,
    url: String,
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseAsset {
    id: u64,
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    api_url: String,
    download_url: String,
}

impl ReleaseAsset {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    pub fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    pub fn download_url(&self) -> &str {
        &self.download_url
    }

    pub(crate) fn from_github(
        id: u64,
        name: String,
        state: String,
        size: u64,
        digest: Option<String>,
        api_url: String,
        download_url: String,
    ) -> Self {
        Self {
            id,
            name,
            state,
            size,
            digest,
            api_url,
            download_url,
        }
    }
}

impl ReleaseInfo {
    pub(crate) fn from_github(
        version: Version,
        channel: UpdateChannel,
        url: String,
        tag_name: String,
        assets: Vec<ReleaseAsset>,
    ) -> Self {
        Self {
            version,
            channel,
            url,
            tag_name,
            assets,
        }
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn channel(&self) -> UpdateChannel {
        self.channel
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn tag_name(&self) -> &str {
        &self.tag_name
    }

    pub fn assets(&self) -> &[ReleaseAsset] {
        &self.assets
    }

    fn to_cached(&self) -> CachedReleaseInfo {
        CachedReleaseInfo {
            version: self.version.to_string(),
            channel: self.channel,
            url: self.url.clone(),
            tag_name: Some(self.tag_name.clone()),
            assets: self.assets.clone(),
        }
    }

    fn from_cached(cached: &CachedReleaseInfo) -> Option<Self> {
        Version::parse(&cached.version).ok().map(|version| {
            let tag_name = cached
                .tag_name
                .clone()
                .unwrap_or_else(|| format!("v{version}"));
            Self {
                version,
                channel: cached.channel,
                url: cached.url.clone(),
                tag_name,
                assets: cached.assets.clone(),
            }
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct UpdateCheckCache {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stable: Option<CachedUpdateCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prerelease: Option<CachedUpdateCheck>,
}

impl UpdateCheckCache {
    fn entry(&self, channel: UpdateChannel) -> Option<&CachedUpdateCheck> {
        match channel {
            UpdateChannel::Stable => self.stable.as_ref(),
            UpdateChannel::Prerelease => self.prerelease.as_ref(),
        }
    }

    fn entry_mut(&mut self, channel: UpdateChannel) -> Option<&mut CachedUpdateCheck> {
        match channel {
            UpdateChannel::Stable => self.stable.as_mut(),
            UpdateChannel::Prerelease => self.prerelease.as_mut(),
        }
    }

    fn set_entry(&mut self, channel: UpdateChannel, entry: CachedUpdateCheck) {
        match channel {
            UpdateChannel::Stable => self.stable = Some(entry),
            UpdateChannel::Prerelease => self.prerelease = Some(entry),
        }
    }

    fn record_notification(
        &mut self,
        channel: UpdateChannel,
        release: &ReleaseInfo,
        shown_at: u64,
    ) {
        if let Some(entry) = self.entry_mut(channel) {
            entry.last_notification = Some(CachedUpdateNotification {
                shown_at_unix_seconds: shown_at,
                release: release.to_cached(),
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedUpdateCheck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    last_checked_at_unix_seconds: u64,
    latest: CachedReleaseInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_notification: Option<CachedUpdateNotification>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedUpdateNotification {
    shown_at_unix_seconds: u64,
    release: CachedReleaseInfo,
}

impl CachedUpdateNotification {
    fn matches_release(&self, release: &ReleaseInfo) -> bool {
        self.release.matches_release(release)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedReleaseInfo {
    version: String,
    channel: UpdateChannel,
    url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tag_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    assets: Vec<ReleaseAsset>,
}

impl CachedReleaseInfo {
    fn matches_release(&self, release: &ReleaseInfo) -> bool {
        self.version == release.version().to_string()
            && self.channel == release.channel()
            && self.url == release.url()
    }
}

#[cfg(test)]
mod tests {
    use super::cache::{
        DefaultUpdateCacheStore, FileUpdateCacheStore, UpdateCachePathError, UpdateCacheStore,
    };
    use super::check_engine::{
        check_updates, check_updates_with_cache, discover_install_candidate_with, run_update_check,
        run_updates_command_with, run_updates_command_with_update_settings, UpdatesRunContext,
    };
    use super::github::{GitHubReleaseResponse, GitHubReleasesClient, ReleaseEndpoint};
    use super::{
        CachedReleaseInfo, CachedUpdateCheck, CachedUpdateNotification, EnvUpdateSettings,
        ReleaseInfo, StaticUpdateSettings, UpdateChannel, UpdateCheckCache, UpdateSettings,
        UpdatesCommand, UpdatesDeferredFailure, UpdatesError,
    };
    use crate::session_notifications::{
        UpdateNotificationError, UpdateNotificationHandoff, UpdateNotificationOutcome,
        UpdateNotificationRequest,
    };
    use crate::settings::{ConfigEnvReader, SettingsError, SettingsStore};
    use crate::version::{ReleaseChannel, VersionInfo};
    use semver::Version;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::process;
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    };

    pub(super) const TEST_NOW: u64 = 1_778_234_400;

    #[derive(Debug)]
    struct MockGitHubReleasesClient {
        responses: RefCell<Vec<Result<GitHubReleaseResponse, UpdatesError>>>,
        requests: RefCell<Vec<(String, String, Option<String>)>>,
    }

    impl MockGitHubReleasesClient {
        fn new(responses: Vec<Result<String, UpdatesError>>) -> Self {
            Self::new_responses(
                responses
                    .into_iter()
                    .map(|response| {
                        response.map(|body| GitHubReleaseResponse::Ok { body, etag: None })
                    })
                    .collect(),
            )
        }

        fn new_responses(responses: Vec<Result<GitHubReleaseResponse, UpdatesError>>) -> Self {
            Self {
                responses: RefCell::new(responses),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<(String, String)> {
            self.requests
                .borrow()
                .iter()
                .map(|(url, user_agent, _)| (url.clone(), user_agent.clone()))
                .collect()
        }

        fn requests_with_etags(&self) -> Vec<(String, String, Option<String>)> {
            self.requests.borrow().clone()
        }
    }

    impl GitHubReleasesClient for MockGitHubReleasesClient {
        fn get(
            &self,
            endpoint: ReleaseEndpoint,
            user_agent: &str,
            if_none_match: Option<&str>,
        ) -> Result<GitHubReleaseResponse, UpdatesError> {
            self.requests.borrow_mut().push((
                endpoint.url("https://api.example.test/releases"),
                user_agent.to_string(),
                if_none_match.map(str::to_string),
            ));
            self.responses.borrow_mut().remove(0)
        }
    }

    #[derive(Debug, Default)]
    struct MemoryUpdateCacheStore {
        cache: RefCell<UpdateCheckCache>,
        load_count: Cell<usize>,
    }

    impl MemoryUpdateCacheStore {
        fn with_cache(cache: UpdateCheckCache) -> Self {
            Self {
                cache: RefCell::new(cache),
                load_count: Cell::new(0),
            }
        }

        fn cache(&self) -> UpdateCheckCache {
            self.cache.borrow().clone()
        }

        fn load_count(&self) -> usize {
            self.load_count.get()
        }
    }

    impl UpdateCacheStore for MemoryUpdateCacheStore {
        fn load(&self) -> Result<UpdateCheckCache, UpdatesError> {
            self.load_count.set(self.load_count.get() + 1);
            Ok(self.cache())
        }

        fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError> {
            self.cache.replace(cache.clone());
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct FailingSaveUpdateCacheStore {
        cache: RefCell<UpdateCheckCache>,
    }

    impl FailingSaveUpdateCacheStore {
        fn cache(&self) -> UpdateCheckCache {
            self.cache.borrow().clone()
        }
    }

    impl UpdateCacheStore for FailingSaveUpdateCacheStore {
        fn load(&self) -> Result<UpdateCheckCache, UpdatesError> {
            Ok(self.cache())
        }

        fn save(&self, cache: &UpdateCheckCache) -> Result<(), UpdatesError> {
            self.cache.replace(cache.clone());
            Err(UpdatesError::Io(io::Error::other("cache unwritable")))
        }
    }

    struct RecordingNotifier {
        notifications: RefCell<Vec<UpdateNotificationRequest>>,
        result: Result<UpdateNotificationOutcome, UpdateNotificationError>,
    }

    impl RecordingNotifier {
        fn failing(message: &str) -> Self {
            Self {
                notifications: RefCell::new(Vec::new()),
                result: Err(UpdateNotificationError::Transport(message.to_string())),
            }
        }

        fn notifications(&self) -> Vec<UpdateNotificationRequest> {
            self.notifications.borrow().clone()
        }
    }

    impl Default for RecordingNotifier {
        fn default() -> Self {
            Self {
                notifications: RefCell::new(Vec::new()),
                result: Ok(UpdateNotificationOutcome::Sent),
            }
        }
    }

    impl UpdateNotificationHandoff for RecordingNotifier {
        fn show_update_notification(
            &self,
            request: &UpdateNotificationRequest,
        ) -> Result<UpdateNotificationOutcome, UpdateNotificationError> {
            self.notifications.borrow_mut().push(request.clone());
            self.result.clone()
        }
    }

    fn updates_run_context<'a, C, N, S, U>(
        version: VersionInfo,
        client: &'a C,
        notifier: &'a N,
        cache_store: &'a S,
        update_settings: &'a U,
        now_unix_seconds: u64,
    ) -> UpdatesRunContext<'a, C, N, S, U> {
        UpdatesRunContext {
            version,
            client,
            notifier,
            cache_store,
            update_settings,
            now_unix_seconds,
        }
    }

    fn version_info(version: &'static str, channel: ReleaseChannel) -> VersionInfo {
        VersionInfo::for_testing(version, channel, Some("test"))
    }

    fn stored_update_settings(automatic_checks: &str, channel: UpdateChannel) -> EnvUpdateSettings {
        let config = format!(
            "updates_auto_check={automatic_checks}\nupdates_channel={}\n",
            channel.as_str()
        );
        EnvUpdateSettings {
            store: SettingsStore::from_reader(ConfigEnvReader::parse("/tmp/config.env", &config)),
        }
    }

    fn binary_identities() -> [VersionInfo; 3] {
        [
            version_info("1.1.0", ReleaseChannel::Stable),
            version_info("1.1.0-beta.1", ReleaseChannel::Prerelease),
            version_info("1.1.0", ReleaseChannel::Dev),
        ]
    }

    fn channel_response(channel: UpdateChannel) -> String {
        match channel {
            UpdateChannel::Stable => stable_release("v1.1.1"),
            UpdateChannel::Prerelease => format!("[{}]", prerelease("v1.2.0-beta.1")),
        }
    }

    fn channel_endpoint(channel: UpdateChannel) -> &'static str {
        match channel {
            UpdateChannel::Stable => "https://api.example.test/releases/latest",
            UpdateChannel::Prerelease => "https://api.example.test/releases?per_page=1",
        }
    }

    fn channel_latest_line(channel: UpdateChannel) -> &'static str {
        match channel {
            UpdateChannel::Stable => "latest: 1.1.1 (stable)",
            UpdateChannel::Prerelease => "latest: 1.2.0-beta.1 (prerelease)",
        }
    }

    fn stable_release(tag: &str) -> String {
        release_json(tag, false, false)
    }

    fn prerelease(tag: &str) -> String {
        release_json(tag, false, true)
    }

    fn draft_prerelease(tag: &str) -> String {
        release_json(tag, true, true)
    }

    fn release_json(tag: &str, draft: bool, prerelease: bool) -> String {
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.test/releases/tag/{tag}","draft":{draft},"prerelease":{prerelease}}}"#
        )
    }

    pub(super) fn release_info(version: &str, channel: UpdateChannel, url: &str) -> ReleaseInfo {
        let version = Version::parse(version).expect("test version should parse");
        ReleaseInfo {
            tag_name: format!("v{version}"),
            version,
            channel,
            url: url.to_string(),
            assets: Vec::new(),
        }
    }

    fn api_response(body: String, etag: Option<&str>) -> GitHubReleaseResponse {
        GitHubReleaseResponse::Ok {
            body,
            etag: etag.map(str::to_string),
        }
    }

    pub(super) fn cached_entry(
        etag: Option<&str>,
        version: &str,
        channel: UpdateChannel,
        url: &str,
        last_checked_at_unix_seconds: u64,
    ) -> CachedUpdateCheck {
        CachedUpdateCheck {
            etag: etag.map(str::to_string),
            last_checked_at_unix_seconds,
            latest: CachedReleaseInfo {
                version: version.to_string(),
                channel,
                url: url.to_string(),
                tag_name: None,
                assets: Vec::new(),
            },
            last_notification: None,
        }
    }

    pub(super) fn cached_notification(
        version: &str,
        channel: UpdateChannel,
        url: &str,
        shown_at_unix_seconds: u64,
    ) -> CachedUpdateNotification {
        CachedUpdateNotification {
            shown_at_unix_seconds,
            release: CachedReleaseInfo {
                version: version.to_string(),
                channel,
                url: url.to_string(),
                tag_name: None,
                assets: Vec::new(),
            },
        }
    }

    pub(super) fn cached_entry_with_notification(
        etag: Option<&str>,
        version: &str,
        channel: UpdateChannel,
        url: &str,
        last_checked_at_unix_seconds: u64,
        notification_shown_at_unix_seconds: u64,
    ) -> CachedUpdateCheck {
        let mut entry = cached_entry(etag, version, channel, url, last_checked_at_unix_seconds);
        entry.last_notification = Some(cached_notification(
            version,
            channel,
            url,
            notification_shown_at_unix_seconds,
        ));
        entry
    }

    fn rendered(output: &[u8]) -> String {
        String::from_utf8(output.to_vec()).expect("utf8 output")
    }

    fn check() -> UpdatesCommand {
        UpdatesCommand::Check { notify: false }
    }

    fn check_notify() -> UpdatesCommand {
        UpdatesCommand::Check { notify: true }
    }

    fn background_check() -> UpdatesCommand {
        UpdatesCommand::BackgroundCheck
    }

    static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    pub(super) fn unique_temp_dir(label: &str) -> PathBuf {
        let counter = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-updates-{label}-{}-{counter}",
            process::id()
        ));
        fs::create_dir_all(&path).expect("create test temp dir");
        path
    }

    pub(super) fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[cfg(unix)]
    pub(super) struct UmaskGuard {
        previous: libc::mode_t,
    }

    #[cfg(unix)]
    impl UmaskGuard {
        pub(super) fn set(mask: libc::mode_t) -> Self {
            Self {
                previous: unsafe { libc::umask(mask) },
            }
        }
    }

    #[cfg(unix)]
    impl Drop for UmaskGuard {
        fn drop(&mut self) {
            unsafe { libc::umask(self.previous) };
        }
    }

    #[test]
    fn update_check_sends_cached_etag_and_stores_response_etag() {
        let client = MockGitHubReleasesClient::new_responses(vec![Ok(api_response(
            stable_release("v1.2.0"),
            Some("\"next-etag\""),
        ))]);
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"cached-etag\""),
                "1.1.0",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.0",
                TEST_NOW - 1,
            ),
        );

        let result = check_updates_with_cache(
            UpdateChannel::Stable,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &mut cache,
            TEST_NOW,
        )
        .expect("stable update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string(),
                Some("\"cached-etag\"".to_string())
            )]
        );
        let entry = cache
            .entry(UpdateChannel::Stable)
            .expect("stable cache entry");
        assert_eq!(entry.etag.as_deref(), Some("\"next-etag\""));
        assert_eq!(entry.last_checked_at_unix_seconds, TEST_NOW);
        assert_eq!(entry.latest.version, "1.2.0");
    }

    #[test]
    fn release_discovery_preserves_complete_asset_metadata() {
        let body = r#"{
          "tag_name":"v1.4.0",
          "html_url":"https://github.com/Staphylococcus/LG_Buddy/releases/tag/v1.4.0",
          "draft":false,
          "prerelease":false,
          "assets":[{
            "id":123,
            "name":"lg-buddy-1.4.0-x86_64-unknown-linux-musl.tar.gz",
            "state":"uploaded",
            "size":456,
            "digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "url":"https://api.github.com/repos/Staphylococcus/LG_Buddy/releases/assets/123",
            "browser_download_url":"https://github.com/Staphylococcus/LG_Buddy/releases/download/v1.4.0/lg-buddy-1.4.0-x86_64-unknown-linux-musl.tar.gz"
          }]
        }"#;
        let client = MockGitHubReleasesClient::new(vec![Ok(body.to_string())]);
        let result = check_updates(
            UpdateChannel::Stable,
            version_info("1.3.0", ReleaseChannel::Stable),
            &client,
        )
        .expect("update check");

        assert_eq!(result.latest.tag_name(), "v1.4.0");
        assert_eq!(result.latest.assets().len(), 1);
        let asset = &result.latest.assets()[0];
        assert_eq!(asset.id(), 123);
        assert_eq!(asset.state(), "uploaded");
        assert_eq!(asset.size(), 456);
        assert_eq!(
            asset.digest(),
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn observed_beta_2_response_is_replayed_as_the_upgrade_baseline() {
        // Reduced to the fields consumed by GitHubRelease from the production
        // response recorded in https://github.com/Staphylococcus/LG_Buddy/issues/99.
        let body = include_str!("../testdata/github/releases-v1.4.0-beta.2.json");
        let client = MockGitHubReleasesClient::new(vec![Ok(body.to_string())]);
        let release = discover_install_candidate_with(
            version_info("1.4.0-beta.1", ReleaseChannel::Prerelease),
            &client,
            &StaticUpdateSettings::enabled(UpdateChannel::Prerelease),
        )
        .expect("observed prerelease response should select beta.2");

        assert_eq!(release.version(), &Version::parse("1.4.0-beta.2").unwrap());
        assert_eq!(release.channel(), UpdateChannel::Prerelease);
        assert_eq!(release.tag_name(), "v1.4.0-beta.2");
        assert_eq!(release.assets().len(), 2);
        let archive = &release.assets()[0];
        assert_eq!(archive.id(), 539980839);
        assert_eq!(archive.size(), 3_051_471);
        assert_eq!(
            archive.digest(),
            Some("sha256:883e6cb869cbe60988a195acac2e15864d904797edfefbb7d90052eff9a17d32")
        );
        assert_eq!(
            archive.api_url(),
            "https://api.github.com/repos/Staphylococcus/LG_Buddy/releases/assets/539980839"
        );
        assert_eq!(
            archive.download_url(),
            "https://github.com/Staphylococcus/LG_Buddy/releases/download/v1.4.0-beta.2/lg-buddy-1.4.0-beta.2-x86_64-unknown-linux-musl.tar.gz"
        );
        let checksums = &release.assets()[1];
        assert_eq!(checksums.id(), 539980872);
        assert_eq!(checksums.name(), "sha256sums.txt");
        assert_eq!(checksums.size(), 123);
    }

    #[test]
    fn stable_not_modified_uses_cached_release_metadata() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"stable-etag\""),
                "1.1.1",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.1",
                TEST_NOW - 10,
            ),
        );

        let result = check_updates_with_cache(
            UpdateChannel::Stable,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &mut cache,
            TEST_NOW,
        )
        .expect("cached stable update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            result.render(),
            "status: update available\ncurrent: 1.1.0 (stable)\nlatest: 1.1.1 (stable)\nurl: https://github.test/releases/tag/v1.1.1\ninstall: lg-buddy updates install\n"
        );
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string(),
                Some("\"stable-etag\"".to_string())
            )]
        );
        let entry = cache
            .entry(UpdateChannel::Stable)
            .expect("stable cache entry");
        assert_eq!(entry.etag.as_deref(), Some("\"stable-etag\""));
        assert_eq!(entry.last_checked_at_unix_seconds, TEST_NOW);
    }

    #[test]
    fn prerelease_not_modified_can_use_cached_stable_latest_release() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Prerelease,
            cached_entry(
                Some("\"prerelease-etag\""),
                "1.2.0",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.2.0",
                TEST_NOW - 10,
            ),
        );

        let result = check_updates_with_cache(
            UpdateChannel::Prerelease,
            version_info("1.2.0-beta.1", ReleaseChannel::Prerelease),
            &client,
            &mut cache,
            TEST_NOW,
        )
        .expect("cached prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(result.latest.channel(), UpdateChannel::Stable);
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases?per_page=1".to_string(),
                "lg-buddy/1.2.0-beta.1".to_string(),
                Some("\"prerelease-etag\"".to_string())
            )]
        );
    }

    #[test]
    fn not_modified_without_cached_release_metadata_is_reported() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let mut cache = UpdateCheckCache::default();

        let err = check_updates_with_cache(
            UpdateChannel::Stable,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &mut cache,
            TEST_NOW,
        )
        .expect_err("304 without cache should fail");

        assert!(matches!(
            err,
            UpdatesError::NotModifiedWithoutCache {
                channel: UpdateChannel::Stable
            }
        ));
    }

    #[test]
    fn manual_update_check_reuses_cache_on_not_modified() {
        let client = MockGitHubReleasesClient::new_responses(vec![
            Ok(api_response(
                stable_release("v1.1.1"),
                Some("\"stable-etag\""),
            )),
            Ok(GitHubReleaseResponse::NotModified),
        ]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        run_updates_command_with(
            check(),
            &mut first_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("initial update check should succeed");
        run_updates_command_with(
            check(),
            &mut second_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW + 1,
        )
        .expect("cached update check should succeed");

        assert_eq!(rendered(&first_output), rendered(&second_output));
        assert_eq!(
            client.requests_with_etags(),
            vec![
                (
                    "https://api.example.test/releases/latest".to_string(),
                    "lg-buddy/1.1.0".to_string(),
                    None
                ),
                (
                    "https://api.example.test/releases/latest".to_string(),
                    "lg-buddy/1.1.0".to_string(),
                    Some("\"stable-etag\"".to_string())
                )
            ]
        );
        let cache = cache_store.cache();
        assert_eq!(
            cache
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_checked_at_unix_seconds,
            TEST_NOW + 1
        );
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn background_update_check_skips_without_github_or_cache_when_disabled() {
        let client = MockGitHubReleasesClient::new_responses(vec![]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let update_settings = StaticUpdateSettings::disabled(UpdateChannel::Stable);
        let mut output = Vec::new();

        run_updates_command_with_update_settings(
            background_check(),
            &mut output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &update_settings,
                TEST_NOW,
            ),
        )
        .expect("disabled background update check should succeed");

        assert_eq!(
            rendered(&output),
            "background: skipped (automatic update checks disabled)\n"
        );
        assert!(client.requests_with_etags().is_empty());
        assert!(notifier.notifications().is_empty());
        assert_eq!(cache_store.cache(), UpdateCheckCache::default());
    }

    #[test]
    fn background_update_check_uses_default_stable_channel_and_notifies() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let update_settings = StaticUpdateSettings::enabled(UpdateChannel::Stable);
        let mut output = Vec::new();

        run_updates_command_with_update_settings(
            background_check(),
            &mut output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &update_settings,
                TEST_NOW,
            ),
        )
        .expect("enabled background update check should succeed");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: sent (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn disabled_background_update_check_ignores_invalid_channel_setting() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/tmp/config.env",
            "updates_auto_check=disabled\nupdates_channel=bogus\n",
        ));

        let settings = EnvUpdateSettings { store };

        assert!(!settings
            .automatic_checks_enabled()
            .expect("disabled background checks should not parse the channel setting"));
    }

    #[test]
    fn enabled_background_update_check_defaults_to_stable_channel() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/tmp/config.env",
            "updates_auto_check=enabled\n",
        ));

        let settings = EnvUpdateSettings { store };

        assert!(settings
            .automatic_checks_enabled()
            .expect("automatic check setting should resolve"));
        assert_eq!(
            settings
                .channel()
                .expect("default update channel should resolve"),
            UpdateChannel::Stable
        );
    }

    #[test]
    fn background_update_check_uses_configured_prerelease_channel() {
        let client =
            MockGitHubReleasesClient::new(vec![Ok(format!("[{}]", prerelease("v1.2.0-beta.1")))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let update_settings = StaticUpdateSettings::enabled(UpdateChannel::Prerelease);
        let mut output = Vec::new();

        run_updates_command_with_update_settings(
            background_check(),
            &mut output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &update_settings,
                TEST_NOW,
            ),
        )
        .expect("configured prerelease background update check should succeed");

        assert!(rendered(&output).contains("latest: 1.2.0-beta.1 (prerelease)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases?per_page=1".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn background_update_check_reuses_notification_policy_for_repeated_release() {
        let client = MockGitHubReleasesClient::new_responses(vec![
            Ok(api_response(
                stable_release("v1.1.1"),
                Some("\"stable-etag\""),
            )),
            Ok(GitHubReleaseResponse::NotModified),
        ]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let update_settings = StaticUpdateSettings::enabled(UpdateChannel::Stable);
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        run_updates_command_with_update_settings(
            background_check(),
            &mut first_output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &update_settings,
                TEST_NOW,
            ),
        )
        .expect("initial background update check should succeed");
        run_updates_command_with_update_settings(
            background_check(),
            &mut second_output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &update_settings,
                TEST_NOW + 1,
            ),
        )
        .expect("repeated background update check should succeed");

        assert!(rendered(&first_output).contains("notification: sent (new release)"));
        assert!(
            rendered(&second_output).contains("notification: skipped (already shown for 1.1.1)")
        );
        assert_eq!(notifier.notifications().len(), 1);
    }

    #[test]
    fn unavailable_cache_path_does_not_block_update_check_but_fails_after_result() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = DefaultUpdateCacheStore::Unavailable(UpdateCachePathError::NotConfigured);
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("unavailable cache path should be reported after update check");

        assert!(rendered(&output).contains("status: update available"));
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string(),
                None
            )]
        );
        assert!(notifier.notifications().is_empty());
        let UpdatesError::DeferredFailures(failures) = err else {
            panic!("expected deferred cache failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::CachePath(UpdateCachePathError::NotConfigured))
        ));
    }

    #[test]
    fn malformed_cache_does_not_block_update_check_but_fails_after_result() {
        let dir = unique_temp_dir("malformed-cache-command");
        let path = dir.join("lg-buddy").join("update-check.json");
        fs::create_dir_all(path.parent().expect("cache path parent")).expect("create cache dir");
        fs::write(&path, "{").expect("write malformed cache");

        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = FileUpdateCacheStore::new(path.clone());
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("malformed cache should be reported after update check");

        assert!(rendered(&output).contains("status: update available"));
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string(),
                None
            )]
        );
        assert!(notifier.notifications().is_empty());
        let UpdatesError::DeferredFailures(failures) = err else {
            panic!("expected deferred cache decode failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::CacheDecode { path: error_path, .. } if error_path == &path)
        ));
        assert_eq!(
            cache_store
                .load()
                .expect("successful check should replace malformed cache")
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .latest
                .version
                .as_str(),
            "1.1.1"
        );

        fs::remove_dir_all(dir).expect("remove test temp dir");
    }

    #[test]
    fn cache_save_failure_does_not_block_requested_notification_but_fails_afterwards() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = FailingSaveUpdateCacheStore::default();
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check_notify(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("cache save failure should be reported after notification");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: sent (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification
                .as_ref()
                .expect("notification state")
                .release
                .version,
            "1.1.1"
        );
        let UpdatesError::DeferredFailures(failures) = err else {
            panic!("expected deferred cache failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::Io(_))
        ));
    }

    #[test]
    fn notification_and_cache_save_failures_are_reported_together() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::failing("bus unavailable");
        let cache_store = FailingSaveUpdateCacheStore::default();
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check_notify(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("notification and cache failures should both be reported");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: failed (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert!(cache_store
            .cache()
            .entry(UpdateChannel::Stable)
            .expect("stable cache entry")
            .last_notification
            .is_none());
        let UpdatesError::DeferredFailures(failures) = err else {
            panic!("expected deferred failures");
        };
        assert_eq!(failures.len(), 2);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Notification(_)
        ));
        assert!(matches!(
            &failures[1],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::Io(_))
        ));
    }

    #[test]
    fn notify_sends_notification_when_cached_not_modified_update_is_available() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let notifier = RecordingNotifier::default();
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"stable-etag\""),
                "1.1.1",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.1",
                TEST_NOW - 10,
            ),
        );
        let cache_store = MemoryUpdateCacheStore::with_cache(cache);
        let mut output = Vec::new();

        run_updates_command_with(
            check_notify(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("notifying cached update check should succeed");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: sent (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification
                .as_ref()
                .expect("notification state")
                .release
                .version,
            "1.1.1"
        );
    }

    #[test]
    fn notify_does_not_send_notification_when_cached_not_modified_is_up_to_date() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let notifier = RecordingNotifier::default();
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Stable,
            cached_entry(
                Some("\"stable-etag\""),
                "1.1.0",
                UpdateChannel::Stable,
                "https://github.test/releases/tag/v1.1.0",
                TEST_NOW - 10,
            ),
        );
        let cache_store = MemoryUpdateCacheStore::with_cache(cache);
        let mut output = Vec::new();

        run_updates_command_with(
            check_notify(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("notifying cached update check should succeed");

        assert!(rendered(&output).contains("status: up to date"));
        assert!(rendered(&output).contains("notification: skipped (no update available)"));
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn manual_update_check_uses_saved_stable_channel_for_prerelease_build() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let update_settings = StaticUpdateSettings::enabled(UpdateChannel::Stable);
        let mut output = Vec::new();

        run_updates_command_with_update_settings(
            check(),
            &mut output,
            updates_run_context(
                version_info("1.1.0-beta.1", ReleaseChannel::Prerelease),
                &client,
                &notifier,
                &cache_store,
                &update_settings,
                TEST_NOW,
            ),
        )
        .expect("saved stable channel should drive the manual check");

        assert!(rendered(&output).contains("latest: 1.1.1 (stable)"));
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0-beta.1".to_string()
            )]
        );
    }

    #[test]
    fn manual_update_check_uses_saved_prerelease_channel_when_auto_check_is_disabled() {
        let client =
            MockGitHubReleasesClient::new(vec![Ok(format!("[{}]", prerelease("v1.2.0-beta.1")))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let update_settings = StaticUpdateSettings::disabled(UpdateChannel::Prerelease);
        let mut output = Vec::new();

        run_updates_command_with_update_settings(
            check(),
            &mut output,
            updates_run_context(
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
                &notifier,
                &cache_store,
                &update_settings,
                TEST_NOW,
            ),
        )
        .expect("manual check should ignore the automatic-check gate");

        assert!(rendered(&output).contains("latest: 1.2.0-beta.1 (prerelease)"));
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases?per_page=1".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn manual_update_outcome_uses_saved_channel_when_auto_check_is_disabled() {
        for automatic_checks in ["disabled", "bogus"] {
            for (channel, response, endpoint, expected_version) in [
                (
                    UpdateChannel::Stable,
                    stable_release("v1.1.1"),
                    "https://api.example.test/releases/latest",
                    "1.1.1",
                ),
                (
                    UpdateChannel::Prerelease,
                    format!("[{}]", prerelease("v1.2.0-beta.1")),
                    "https://api.example.test/releases?per_page=1",
                    "1.2.0-beta.1",
                ),
            ] {
                let config = format!(
                    "updates_auto_check={automatic_checks}\nupdates_channel={}\n",
                    channel.as_str()
                );
                let config_dir = unique_temp_dir("manual-update-settings");
                let config_path = config_dir.join("config.env");
                fs::write(&config_path, &config).expect("write update settings");
                let settings = EnvUpdateSettings {
                    store: SettingsStore::load(&config_path).expect("load update settings"),
                };
                let client = MockGitHubReleasesClient::new(vec![Ok(response)]);
                let cache_store = MemoryUpdateCacheStore::default();

                let outcome = run_update_check(
                    version_info("1.1.0", ReleaseChannel::Stable),
                    &client,
                    &cache_store,
                    &settings,
                    TEST_NOW,
                )
                .expect("manual update check should use saved settings");

                assert_eq!(outcome.result().check_channel(), channel);
                assert_eq!(
                    outcome.result().current_version(),
                    &Version::parse("1.1.0").unwrap()
                );
                assert_eq!(outcome.result().current_channel(), ReleaseChannel::Stable);
                assert_eq!(
                    outcome.result().latest().version(),
                    &Version::parse(expected_version).unwrap()
                );
                assert!(outcome.result().update_available());
                assert!(outcome.warnings().is_empty());
                assert_eq!(
                    client.requests(),
                    vec![(endpoint.to_string(), "lg-buddy/1.1.0".to_string())]
                );

                let cli_client =
                    MockGitHubReleasesClient::new(vec![Ok(if channel == UpdateChannel::Stable {
                        stable_release("v1.1.1")
                    } else {
                        format!("[{}]", prerelease("v1.2.0-beta.1"))
                    })]);
                let cli_notifier = RecordingNotifier::default();
                let cli_cache_store = MemoryUpdateCacheStore::default();
                let mut cli_output = Vec::new();
                run_updates_command_with_update_settings(
                    check(),
                    &mut cli_output,
                    updates_run_context(
                        version_info("1.1.0", ReleaseChannel::Stable),
                        &cli_client,
                        &cli_notifier,
                        &cli_cache_store,
                        &settings,
                        TEST_NOW,
                    ),
                )
                .expect("equivalent CLI update check should succeed");
                assert_eq!(rendered(&cli_output), outcome.result().render());
                assert!(cli_notifier.notifications().is_empty());
                assert_eq!(
                    fs::read(&config_path).expect("read update settings"),
                    config.as_bytes()
                );

                fs::remove_dir_all(config_dir).expect("remove update settings temp dir");
            }
        }
    }

    #[test]
    fn manual_update_outcome_reuses_cached_not_modified_release() {
        let client =
            MockGitHubReleasesClient::new_responses(vec![Ok(GitHubReleaseResponse::NotModified)]);
        let mut cache = UpdateCheckCache::default();
        cache.set_entry(
            UpdateChannel::Prerelease,
            cached_entry(
                Some("\"prerelease-etag\""),
                "1.2.0-beta.1",
                UpdateChannel::Prerelease,
                "https://github.test/releases/tag/v1.2.0-beta.1",
                TEST_NOW - 10,
            ),
        );
        let cache_store = MemoryUpdateCacheStore::with_cache(cache);
        let settings = StaticUpdateSettings::disabled(UpdateChannel::Prerelease);

        let outcome = run_update_check(
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &cache_store,
            &settings,
            TEST_NOW,
        )
        .expect("cached 304 update check should succeed");

        assert_eq!(
            outcome.result().latest().version(),
            &Version::parse("1.2.0-beta.1").unwrap()
        );
        assert!(outcome.result().update_available());
        assert!(outcome.warnings().is_empty());
        assert_eq!(
            client.requests_with_etags(),
            vec![(
                "https://api.example.test/releases?per_page=1".to_string(),
                "lg-buddy/1.1.0".to_string(),
                Some("\"prerelease-etag\"".to_string()),
            )]
        );
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Prerelease)
                .expect("prerelease cache entry")
                .last_checked_at_unix_seconds,
            TEST_NOW
        );
    }

    #[test]
    fn manual_update_outcome_preserves_result_when_cache_save_fails() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let cache_store = FailingSaveUpdateCacheStore::default();
        let settings = StaticUpdateSettings::disabled(UpdateChannel::Stable);

        let outcome = run_update_check(
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &cache_store,
            &settings,
            TEST_NOW,
        )
        .expect("cache failures should be deferred until after a valid result");

        assert_eq!(
            outcome.result().latest().version(),
            &Version::parse("1.1.1").unwrap()
        );
        assert_eq!(outcome.warnings().len(), 1);
        assert!(matches!(
            &outcome.warnings()[0],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::Io(_))
        ));
    }

    #[test]
    fn manual_update_outcome_preserves_result_when_cache_load_fails() {
        let cache_dir = unique_temp_dir("manual-update-cache-load");
        let cache_path = cache_dir.join("lg-buddy").join("update-check.json");
        fs::create_dir_all(cache_path.parent().expect("cache path parent"))
            .expect("create cache directory");
        fs::write(&cache_path, "{").expect("write malformed cache");

        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let cache_store = FileUpdateCacheStore::new(cache_path.clone());
        let settings = StaticUpdateSettings::disabled(UpdateChannel::Stable);

        let outcome = run_update_check(
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &cache_store,
            &settings,
            TEST_NOW,
        )
        .expect("cache load failures should be deferred until after a valid result");

        assert_eq!(
            outcome.result().latest().version(),
            &Version::parse("1.1.1").unwrap()
        );
        assert_eq!(outcome.warnings().len(), 1);
        assert!(matches!(
            &outcome.warnings()[0],
            UpdatesDeferredFailure::Cache(cache_err)
                if matches!(cache_err.as_ref(), UpdatesError::CacheDecode { path, .. } if path == &cache_path)
        ));
        assert_eq!(
            cache_store
                .load()
                .expect("successful check should replace malformed cache")
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .latest
                .version,
            "1.1.1"
        );

        fs::remove_dir_all(cache_dir).expect("remove cache temp dir");
    }

    #[test]
    fn manual_update_outcome_returns_network_failures_without_cache_warning() {
        let client = MockGitHubReleasesClient::new(vec![Err(UpdatesError::ApiStatus {
            url: "https://api.example.test/releases/latest".to_string(),
            status: 503,
            body: "unavailable".to_string(),
        })]);
        let cache_store = MemoryUpdateCacheStore::default();
        let settings = StaticUpdateSettings::disabled(UpdateChannel::Stable);

        let err = run_update_check(
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &cache_store,
            &settings,
            TEST_NOW,
        )
        .expect_err("network failures should remain check errors");

        assert!(matches!(err, UpdatesError::ApiStatus { status: 503, .. }));
        assert_eq!(cache_store.load_count(), 1);
        assert!(cache_store.cache().entry(UpdateChannel::Stable).is_none());
    }

    #[test]
    fn install_discovery_uses_saved_channel_and_ignores_automatic_check_gate() {
        let client =
            MockGitHubReleasesClient::new(vec![Ok(format!("[{}]", prerelease("v1.2.0-beta.1")))]);
        let update_settings = StaticUpdateSettings::disabled(UpdateChannel::Prerelease);

        let release = discover_install_candidate_with(
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &update_settings,
        )
        .expect("install discovery should use the saved prerelease channel");

        assert_eq!(release.version(), &Version::parse("1.2.0-beta.1").unwrap());
        assert_eq!(release.channel(), UpdateChannel::Prerelease);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases?per_page=1".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn saved_channel_drives_manual_checks_for_every_binary_identity() {
        for saved_channel in [UpdateChannel::Stable, UpdateChannel::Prerelease] {
            let update_settings = stored_update_settings("disabled", saved_channel);

            for current in binary_identities() {
                let current_version = current.version();
                let current_channel = current.channel();
                let client =
                    MockGitHubReleasesClient::new(vec![Ok(channel_response(saved_channel))]);
                let notifier = RecordingNotifier::default();
                let cache_store = MemoryUpdateCacheStore::default();
                let mut output = Vec::new();

                run_updates_command_with_update_settings(
                    check(),
                    &mut output,
                    updates_run_context(
                        current,
                        &client,
                        &notifier,
                        &cache_store,
                        &update_settings,
                        TEST_NOW,
                    ),
                )
                .unwrap_or_else(|err| {
                    panic!(
                        "saved {} channel should drive a manual check for a {} binary: {err}",
                        saved_channel.as_str(),
                        current_channel.as_str()
                    )
                });

                assert!(
                    rendered(&output).contains(channel_latest_line(saved_channel)),
                    "saved {} channel produced the wrong result for a {} binary",
                    saved_channel.as_str(),
                    current_channel.as_str()
                );
                assert_eq!(
                    client.requests(),
                    vec![(
                        channel_endpoint(saved_channel).to_string(),
                        format!("lg-buddy/{current_version}")
                    )],
                    "saved {} channel used the wrong endpoint for a {} binary",
                    saved_channel.as_str(),
                    current_channel.as_str()
                );
                assert!(notifier.notifications().is_empty());
                assert_eq!(
                    cache_store
                        .cache()
                        .entry(saved_channel)
                        .expect("selected channel should have a cache entry")
                        .latest
                        .channel,
                    saved_channel
                );
            }
        }
    }

    #[test]
    fn saved_channel_drives_background_checks_for_every_binary_identity() {
        for saved_channel in [UpdateChannel::Stable, UpdateChannel::Prerelease] {
            let update_settings = stored_update_settings("enabled", saved_channel);

            for current in binary_identities() {
                let current_version = current.version();
                let current_channel = current.channel();
                let client =
                    MockGitHubReleasesClient::new(vec![Ok(channel_response(saved_channel))]);
                let notifier = RecordingNotifier::default();
                let cache_store = MemoryUpdateCacheStore::default();
                let mut output = Vec::new();

                run_updates_command_with_update_settings(
                    background_check(),
                    &mut output,
                    updates_run_context(
                        current,
                        &client,
                        &notifier,
                        &cache_store,
                        &update_settings,
                        TEST_NOW,
                    ),
                )
                .unwrap_or_else(|err| {
                    panic!(
                        "saved {} channel should drive a background check for a {} binary: {err}",
                        saved_channel.as_str(),
                        current_channel.as_str()
                    )
                });

                assert!(
                    rendered(&output).contains(channel_latest_line(saved_channel)),
                    "saved {} channel produced the wrong result for a {} binary",
                    saved_channel.as_str(),
                    current_channel.as_str()
                );
                assert!(rendered(&output).contains("notification: sent (new release)"));
                assert_eq!(
                    client.requests(),
                    vec![(
                        channel_endpoint(saved_channel).to_string(),
                        format!("lg-buddy/{current_version}")
                    )],
                    "saved {} channel used the wrong endpoint for a {} binary",
                    saved_channel.as_str(),
                    current_channel.as_str()
                );
                let notifications = notifier.notifications();
                assert_eq!(notifications.len(), 1);
                let notification = notifications[0].to_dbus_fields();
                assert_eq!(notification.0, saved_channel.as_str());
                assert_eq!(notification.4, saved_channel.as_str());
                assert_eq!(
                    cache_store
                        .cache()
                        .entry(saved_channel)
                        .expect("selected channel should have a cache entry")
                        .last_notification
                        .as_ref()
                        .expect("background check should record its notification")
                        .release
                        .channel,
                    saved_channel
                );
            }
        }
    }

    #[test]
    fn invalid_saved_channel_fails_active_checks_before_github_or_cache_work() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/tmp/config.env",
            "updates_auto_check=enabled\nupdates_channel=bogus\n",
        ));
        let update_settings = EnvUpdateSettings { store };

        for command in [check(), background_check()] {
            let client = MockGitHubReleasesClient::new_responses(vec![]);
            let notifier = RecordingNotifier::default();
            let cache_store = MemoryUpdateCacheStore::default();
            let mut output = Vec::new();

            let err = run_updates_command_with_update_settings(
                command,
                &mut output,
                updates_run_context(
                    version_info("1.1.0", ReleaseChannel::Stable),
                    &client,
                    &notifier,
                    &cache_store,
                    &update_settings,
                    TEST_NOW,
                ),
            )
            .expect_err("an active check should reject an invalid saved channel");

            assert!(matches!(
                &err,
                UpdatesError::Settings(SettingsError::InvalidValue { key, value, .. })
                    if key == "updates.channel" && value == "bogus"
            ));
            assert!(client.requests_with_etags().is_empty());
            assert!(notifier.notifications().is_empty());
            assert_eq!(cache_store.load_count(), 0);
            assert_eq!(cache_store.cache(), UpdateCheckCache::default());
            assert!(rendered(&output).is_empty());
        }
    }

    #[test]
    fn plain_updates_check_does_not_send_notification() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        run_updates_command_with(
            check(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("plain update check should succeed");

        assert!(rendered(&output).contains("status: update available"));
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn notify_sends_notification_when_update_is_available() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        run_updates_command_with(
            check_notify(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("notifying update check should succeed");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: sent (new release)"));
        let notifications = notifier.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].to_dbus_fields(),
            (
                "stable".to_string(),
                "1.1.0".to_string(),
                "stable".to_string(),
                "1.1.1".to_string(),
                "stable".to_string(),
                "https://github.test/releases/tag/v1.1.1".to_string()
            )
        );
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification
                .as_ref()
                .expect("notification state")
                .release
                .version,
            "1.1.1"
        );
    }

    #[test]
    fn notify_skips_repeated_notification_for_same_cached_release() {
        let client = MockGitHubReleasesClient::new_responses(vec![
            Ok(api_response(
                stable_release("v1.1.1"),
                Some("\"stable-etag\""),
            )),
            Ok(GitHubReleaseResponse::NotModified),
        ]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        run_updates_command_with(
            check_notify(),
            &mut first_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("initial notifying update check should succeed");
        run_updates_command_with(
            check_notify(),
            &mut second_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW + 1,
        )
        .expect("repeated notifying update check should succeed");

        assert!(rendered(&first_output).contains("notification: sent (new release)"));
        assert!(
            rendered(&second_output).contains("notification: skipped (already shown for 1.1.1)")
        );
        assert_eq!(notifier.notifications().len(), 1);
        assert_eq!(
            client.requests_with_etags(),
            vec![
                (
                    "https://api.example.test/releases/latest".to_string(),
                    "lg-buddy/1.1.0".to_string(),
                    None
                ),
                (
                    "https://api.example.test/releases/latest".to_string(),
                    "lg-buddy/1.1.0".to_string(),
                    Some("\"stable-etag\"".to_string())
                )
            ]
        );
    }

    #[test]
    fn notify_sends_again_when_a_newer_release_is_available() {
        let client = MockGitHubReleasesClient::new_responses(vec![
            Ok(api_response(
                stable_release("v1.1.1"),
                Some("\"stable-etag-1\""),
            )),
            Ok(api_response(
                stable_release("v1.1.2"),
                Some("\"stable-etag-2\""),
            )),
        ]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut first_output = Vec::new();
        let mut second_output = Vec::new();

        run_updates_command_with(
            check_notify(),
            &mut first_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("initial notifying update check should succeed");
        run_updates_command_with(
            check_notify(),
            &mut second_output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW + 1,
        )
        .expect("newer release notifying update check should succeed");

        assert!(rendered(&first_output).contains("notification: sent (new release)"));
        assert!(rendered(&second_output).contains("notification: sent (new release)"));
        assert_eq!(notifier.notifications().len(), 2);
        assert_eq!(
            cache_store
                .cache()
                .entry(UpdateChannel::Stable)
                .expect("stable cache entry")
                .last_notification
                .as_ref()
                .expect("notification state")
                .release
                .version,
            "1.1.2"
        );
    }

    #[test]
    fn notify_does_not_send_notification_when_up_to_date() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.0"))]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        run_updates_command_with(
            check_notify(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect("notifying update check should succeed");

        assert!(rendered(&output).contains("status: up to date"));
        assert!(rendered(&output).contains("notification: skipped (no update available)"));
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn notify_failure_after_available_update_returns_error_after_rendering_output() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);
        let notifier = RecordingNotifier::failing("bus unavailable");
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check_notify(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("notification failure should fail notifying update check");

        assert!(rendered(&output).contains("status: update available"));
        assert!(rendered(&output).contains("notification: failed (new release)"));
        assert_eq!(notifier.notifications().len(), 1);
        assert!(cache_store
            .cache()
            .entry(UpdateChannel::Stable)
            .expect("stable cache entry")
            .last_notification
            .is_none());
        let UpdatesError::DeferredFailures(failures) = &err else {
            panic!("expected deferred notification failure");
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0],
            UpdatesDeferredFailure::Notification(_)
        ));
        assert_eq!(
            err.to_string(),
            "update check completed with deferred failure: update notification handoff failed: could not request update notification from LG Buddy session service: bus unavailable"
        );
    }

    #[test]
    fn notify_does_not_send_notification_when_update_check_fails() {
        let client = MockGitHubReleasesClient::new(vec![Err(UpdatesError::ApiStatus {
            url: "https://api.example.test/releases/latest".to_string(),
            status: 500,
            body: "server error".to_string(),
        })]);
        let notifier = RecordingNotifier::default();
        let cache_store = MemoryUpdateCacheStore::default();
        let mut output = Vec::new();

        let err = run_updates_command_with(
            check_notify(),
            &mut output,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
            &notifier,
            &cache_store,
            TEST_NOW,
        )
        .expect_err("API failure should fail before notification");

        assert!(matches!(err, UpdatesError::ApiStatus { .. }));
        assert!(rendered(&output).is_empty());
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn explicit_stable_channel_reports_up_to_date_for_equal_or_older_versions() {
        for tag in ["v1.1.0", "v1.0.9"] {
            let client = MockGitHubReleasesClient::new(vec![Ok(stable_release(tag))]);

            let result = check_updates(
                UpdateChannel::Stable,
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
            )
            .expect("stable update check should succeed");

            assert!(!result.update_available(), "{tag} should not be newer");
        }
    }

    #[test]
    fn stable_channel_uses_github_latest_endpoint_for_stable_only_ordering() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.2.0"))]);

        let result = check_updates(
            UpdateChannel::Stable,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect("stable update check should succeed");

        assert!(result.update_available());
        assert_eq!(result.latest.version().to_string(), "1.2.0");
        assert_eq!(result.latest.channel(), UpdateChannel::Stable);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn prerelease_channel_accepts_the_newest_published_stable_release() {
        let client =
            MockGitHubReleasesClient::new(vec![Ok(format!("[{}]", stable_release("v1.2.0")))]);

        let result = check_updates(
            UpdateChannel::Prerelease,
            version_info("1.2.0-beta.1", ReleaseChannel::Prerelease),
            &client,
        )
        .expect("prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            result.render(),
            "status: update available\ncurrent: 1.2.0-beta.1 (prerelease)\nlatest: 1.2.0 (stable)\nurl: https://github.test/releases/tag/v1.2.0\ninstall: lg-buddy updates install\n"
        );
    }

    #[test]
    fn prerelease_channel_accepts_the_newest_published_prerelease() {
        let client =
            MockGitHubReleasesClient::new(vec![Ok(format!("[{}]", prerelease("v1.3.0-alpha.1")))]);

        let result = check_updates(
            UpdateChannel::Prerelease,
            version_info("1.2.0-beta.1", ReleaseChannel::Prerelease),
            &client,
        )
        .expect("prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(result.latest.version().to_string(), "1.3.0-alpha.1");
        assert_eq!(result.latest.channel(), UpdateChannel::Prerelease);
        assert_eq!(
            result.latest.url(),
            "https://github.test/releases/tag/v1.3.0-alpha.1"
        );
    }

    #[test]
    fn stable_channel_rejects_prerelease_response() {
        let client = MockGitHubReleasesClient::new(vec![Ok(prerelease("v1.1.1-beta.1"))]);

        let err = check_updates(
            UpdateChannel::Stable,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect_err("prerelease latest response should fail stable check");

        assert!(matches!(
            err,
            UpdatesError::NoMatchingRelease {
                channel: UpdateChannel::Stable
            }
        ));
    }

    #[test]
    fn api_errors_are_reported() {
        let client = MockGitHubReleasesClient::new(vec![Err(UpdatesError::ApiStatus {
            url: "https://api.example.test/releases/latest".to_string(),
            status: 500,
            body: "server error".to_string(),
        })]);

        let err = check_updates(
            UpdateChannel::Stable,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect_err("HTTP status should fail update check");

        assert_eq!(
            err.to_string(),
            "GitHub releases API `https://api.example.test/releases/latest` returned HTTP status 500: server error"
        );
    }

    #[test]
    fn malformed_json_is_reported() {
        let client = MockGitHubReleasesClient::new(vec![Ok("{".to_string())]);

        let err = check_updates(
            UpdateChannel::Stable,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect_err("malformed JSON should fail update check");

        assert!(matches!(
            err,
            UpdatesError::ApiShape {
                endpoint: "latest",
                ..
            }
        ));
    }

    #[test]
    fn missing_required_release_fields_are_reported() {
        let client = MockGitHubReleasesClient::new(vec![Ok(
            r#"{"tag_name":"v1.1.1","draft":false,"prerelease":false}"#.to_string(),
        )]);

        let err = check_updates(
            UpdateChannel::Stable,
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect_err("missing html_url should fail update check");

        assert!(matches!(
            err,
            UpdatesError::ApiShape {
                endpoint: "latest",
                ..
            }
        ));
    }

    #[test]
    fn missing_release_candidate_for_prerelease_channel_is_reported() {
        for response in [
            "[]".to_string(),
            format!("[{}]", prerelease("release-0.6")),
            format!("[{}]", draft_prerelease("v1.2.0-beta.1")),
        ] {
            let client = MockGitHubReleasesClient::new(vec![Ok(response)]);

            let err = check_updates(
                UpdateChannel::Prerelease,
                version_info("1.1.0-beta.1", ReleaseChannel::Prerelease),
                &client,
            )
            .expect_err("missing prerelease candidate should fail update check");

            assert!(matches!(
                err,
                UpdatesError::NoMatchingRelease {
                    channel: UpdateChannel::Prerelease
                }
            ));
        }
    }

    #[test]
    fn invalid_local_version_is_reported() {
        let client = MockGitHubReleasesClient::new(vec![]);

        let err = check_updates(
            UpdateChannel::Stable,
            version_info("not-semver", ReleaseChannel::Dev),
            &client,
        )
        .expect_err("invalid local version should fail update check");

        assert!(matches!(err, UpdatesError::InvalidLocalVersion { .. }));
        assert!(client.requests().is_empty());
    }
}
