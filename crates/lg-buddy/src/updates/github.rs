// GitHub releases transport: the `GitHubReleasesClient` seam, the ureq
// implementation, `ReleaseEndpoint`, `GitHubReleaseResponse`, and the wire
// structs, the API constants, and the wire-JSON-to-ReleaseInfo
// mapping. Moved verbatim from updates.rs; the items the parent
// orchestrator and the colocated tests touch are promoted to `pub(super)`.
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;

use semver::Version;

use crate::version::VersionInfo;

use super::{
    CachedUpdateCheck, ReleaseAsset, ReleaseInfo, UpdateChannel, UpdateCheckCache, UpdatesError,
};

const GITHUB_RELEASES_API_BASE: &str =
    "https://api.github.com/repos/Staphylococcus/LG_Buddy/releases";
const GITHUB_API_VERSION: &str = "2026-03-10";
const GITHUB_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_CONNECT_TIMEOUT_SECONDS: u64 = 5;
const GITHUB_REQUEST_TIMEOUT_SECONDS: u64 = 20;
pub(super) const MAX_GITHUB_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_GITHUB_ERROR_BYTES: u64 = 16 * 1024;

pub(super) trait GitHubReleasesClient {
    fn get(
        &self,
        endpoint: ReleaseEndpoint,
        user_agent: &str,
        if_none_match: Option<&str>,
    ) -> Result<GitHubReleaseResponse, UpdatesError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GitHubReleaseResponse {
    Ok { body: String, etag: Option<String> },
    NotModified,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ReleaseEndpoint {
    LatestStable,
    LatestPublished,
}

impl ReleaseEndpoint {
    pub(super) fn url(self, base: &str) -> String {
        match self {
            Self::LatestStable => format!("{base}/latest"),
            Self::LatestPublished => format!("{base}?per_page=1"),
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::LatestStable => "latest",
            Self::LatestPublished => "releases",
        }
    }
}

pub(super) struct UreqGitHubReleasesClient {
    pub(super) base_url: &'static str,
    pub(super) agent: ureq::Agent,
}

impl Default for UreqGitHubReleasesClient {
    fn default() -> Self {
        Self {
            base_url: GITHUB_RELEASES_API_BASE,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(GITHUB_CONNECT_TIMEOUT_SECONDS))
                .timeout(Duration::from_secs(GITHUB_REQUEST_TIMEOUT_SECONDS))
                .https_only(true)
                .try_proxy_from_env(false)
                .redirects(0)
                .redirect_auth_headers(ureq::RedirectAuthHeaders::Never)
                .build(),
        }
    }
}

impl GitHubReleasesClient for UreqGitHubReleasesClient {
    fn get(
        &self,
        endpoint: ReleaseEndpoint,
        user_agent: &str,
        if_none_match: Option<&str>,
    ) -> Result<GitHubReleaseResponse, UpdatesError> {
        let url = endpoint.url(self.base_url);
        let mut request = self
            .agent
            .get(&url)
            .set("Accept", GITHUB_ACCEPT)
            .set("User-Agent", user_agent)
            .set("X-GitHub-Api-Version", GITHUB_API_VERSION);

        if let Some(etag) = if_none_match {
            request = request.set("If-None-Match", etag);
        }

        #[cfg(feature = "gui-test-fixtures")]
        let request = crate::gui_test_fixtures::request(request);
        let result = request.call();

        match result {
            Ok(response) if response.status() == 200 => {
                let etag = response.header("ETag").map(str::to_string);
                read_ureq_response_body(response, &url, MAX_GITHUB_RESPONSE_BYTES)
                    .map(|body| GitHubReleaseResponse::Ok { body, etag })
            }
            Ok(response) if response.status() == 304 => Ok(GitHubReleaseResponse::NotModified),
            Ok(response) => {
                let status = response.status();
                let body = read_ureq_response_body(response, &url, MAX_GITHUB_ERROR_BYTES)?;
                Err(UpdatesError::ApiStatus { url, status, body })
            }
            Err(ureq::Error::Status(304, _)) => Ok(GitHubReleaseResponse::NotModified),
            Err(ureq::Error::Status(status, response)) => {
                let body = read_ureq_response_body(response, &url, MAX_GITHUB_ERROR_BYTES)?;
                Err(UpdatesError::ApiStatus { url, status, body })
            }
            Err(ureq::Error::Transport(err)) => Err(UpdatesError::Http {
                url,
                message: err.to_string(),
            }),
        }
    }
}

pub(super) fn fetch_latest_release<C: GitHubReleasesClient>(
    channel: UpdateChannel,
    current: VersionInfo,
    client: &C,
    cache: &mut UpdateCheckCache,
    now_unix_seconds: u64,
) -> Result<ReleaseInfo, UpdatesError> {
    let user_agent = format!("lg-buddy/{}", current.version());
    let cached_etag = cache.entry(channel).and_then(|entry| entry.etag.as_deref());

    match channel {
        UpdateChannel::Stable => {
            let endpoint = ReleaseEndpoint::LatestStable;
            let response = client.get(endpoint, &user_agent, cached_etag)?;

            latest_from_response(channel, response, cache, now_unix_seconds, |body| {
                let release: GitHubRelease =
                    serde_json::from_str(body).map_err(|source| UpdatesError::ApiShape {
                        endpoint: endpoint.label(),
                        source,
                    })?;

                release_info_from_api_release(release, channel)
                    .ok_or(UpdatesError::NoMatchingRelease { channel })
            })
        }
        UpdateChannel::Prerelease => {
            let endpoint = ReleaseEndpoint::LatestPublished;
            let response = client.get(endpoint, &user_agent, cached_etag)?;

            latest_from_response(channel, response, cache, now_unix_seconds, |body| {
                let releases: Vec<GitHubRelease> =
                    serde_json::from_str(body).map_err(|source| UpdatesError::ApiShape {
                        endpoint: endpoint.label(),
                        source,
                    })?;

                releases
                    .into_iter()
                    .next()
                    .and_then(|release| release_info_from_api_release(release, channel))
                    .ok_or(UpdatesError::NoMatchingRelease { channel })
            })
        }
    }
}
pub(super) fn latest_from_response<F>(
    channel: UpdateChannel,
    response: GitHubReleaseResponse,
    cache: &mut UpdateCheckCache,
    now_unix_seconds: u64,
    parse_latest: F,
) -> Result<ReleaseInfo, UpdatesError>
where
    F: FnOnce(&str) -> Result<ReleaseInfo, UpdatesError>,
{
    match response {
        GitHubReleaseResponse::Ok { body, etag } => {
            let latest = parse_latest(&body)?;
            let last_notification = cache
                .entry(channel)
                .and_then(|entry| entry.last_notification.clone());
            cache.set_entry(
                channel,
                CachedUpdateCheck {
                    etag,
                    last_checked_at_unix_seconds: now_unix_seconds,
                    latest: latest.to_cached(),
                    last_notification,
                },
            );
            Ok(latest)
        }
        GitHubReleaseResponse::NotModified => {
            let mut entry = cache
                .entry(channel)
                .cloned()
                .ok_or(UpdatesError::NotModifiedWithoutCache { channel })?;
            let latest = ReleaseInfo::from_cached(&entry.latest)
                .ok_or(UpdatesError::NotModifiedWithoutCache { channel })?;
            entry.last_checked_at_unix_seconds = now_unix_seconds;
            cache.set_entry(channel, entry);
            Ok(latest)
        }
    }
}
fn release_info_from_api_release(
    release: GitHubRelease,
    channel: UpdateChannel,
) -> Option<ReleaseInfo> {
    if release.draft {
        return None;
    }

    match channel {
        UpdateChannel::Stable if release.prerelease => return None,
        UpdateChannel::Stable | UpdateChannel::Prerelease => {}
    }

    let release_channel = if release.prerelease {
        UpdateChannel::Prerelease
    } else {
        UpdateChannel::Stable
    };

    parse_release_version(&release.tag_name).map(|version| {
        ReleaseInfo::from_github(
            version,
            release_channel,
            release.html_url,
            release.tag_name,
            release
                .assets
                .into_iter()
                .map(|asset| {
                    ReleaseAsset::from_github(
                        asset.id,
                        asset.name,
                        asset.state,
                        asset.size,
                        asset.digest,
                        asset.url,
                        asset.browser_download_url,
                    )
                })
                .collect(),
        )
    })
}
pub(super) fn parse_release_version(tag_name: &str) -> Option<Version> {
    Version::parse(tag_name.strip_prefix('v').unwrap_or(tag_name)).ok()
}
fn read_ureq_response_body(
    response: ureq::Response,
    url: &str,
    max_bytes: u64,
) -> Result<String, UpdatesError> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| UpdatesError::Http {
            url: url.to_string(),
            message: err.to_string(),
        })?;
    if bytes.len() as u64 > max_bytes {
        return Err(UpdatesError::ResponseTooLarge {
            url: url.to_string(),
            max_bytes,
        });
    }

    String::from_utf8(bytes).map_err(|err| UpdatesError::Http {
        url: url.to_string(),
        message: format!("response was not valid UTF-8: {err}"),
    })
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GitHubReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubReleaseAsset {
    id: u64,
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    url: String,
    browser_download_url: String,
}
