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

#[cfg(test)]
mod tests {
    use super::super::UpdatesError;
    use super::{
        parse_release_version, GitHubReleaseResponse, GitHubReleasesClient, ReleaseEndpoint,
        UreqGitHubReleasesClient, MAX_GITHUB_RESPONSE_BYTES,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn ureq_client_maps_not_modified_status_to_cached_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let address = listener.local_addr().expect("read local test address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client connection");
            let mut buffer = [0; 2048];
            let length = stream.read(&mut buffer).expect("read request");
            let request = String::from_utf8_lossy(&buffer[..length]);

            assert!(request.starts_with("GET /releases?per_page=1 "));
            assert!(request.contains("If-None-Match: \"cached-etag\""));

            stream
                .write_all(
                    b"HTTP/1.1 304 Not Modified\r\nETag: \"cached-etag\"\r\nContent-Length: 0\r\n\r\n",
                )
                .expect("write response");
        });
        let base_url = Box::leak(format!("http://{address}/releases").into_boxed_str());
        let client = UreqGitHubReleasesClient {
            base_url,
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build(),
        };

        let response = client
            .get(
                ReleaseEndpoint::LatestPublished,
                "lg-buddy/1.1.0-alpha.0",
                Some("\"cached-etag\""),
            )
            .expect("304 response should succeed");

        assert_eq!(response, GitHubReleaseResponse::NotModified);
        server.join().expect("server thread should finish");
    }

    #[test]
    fn ureq_client_refuses_release_discovery_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let address = listener.local_addr().expect("read local test address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client connection");
            let mut request = [0; 2048];
            let _ = stream.read(&mut request).expect("read request");
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/untrusted\r\nContent-Length: 0\r\n\r\n",
                )
                .expect("write redirect");
        });
        let base_url = Box::leak(format!("http://{address}/releases").into_boxed_str());
        let client = UreqGitHubReleasesClient {
            base_url,
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .try_proxy_from_env(false)
                .redirects(0)
                .redirect_auth_headers(ureq::RedirectAuthHeaders::Never)
                .build(),
        };

        assert!(matches!(
            client.get(ReleaseEndpoint::LatestStable, "lg-buddy/1.3.0", None),
            Err(UpdatesError::ApiStatus { status: 302, .. })
        ));
        server.join().expect("server thread should finish");
    }

    #[test]
    fn ureq_client_rejects_oversized_release_metadata() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test server");
        let address = listener.local_addr().expect("read local test address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept client connection");
            let mut request = [0; 2048];
            let _ = stream.read(&mut request).expect("read request");
            let body = vec![b' '; MAX_GITHUB_RESPONSE_BYTES as usize + 1];
            stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .expect("write response header");
            stream.write_all(&body).expect("write response body");
        });
        let base_url = Box::leak(format!("http://{address}/releases").into_boxed_str());
        let client = UreqGitHubReleasesClient {
            base_url,
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(5))
                .build(),
        };

        assert!(matches!(
            client.get(ReleaseEndpoint::LatestStable, "lg-buddy/1.3.0", None),
            Err(UpdatesError::ResponseTooLarge { .. })
        ));
        server.join().expect("server thread should finish");
    }

    #[test]
    fn release_version_parser_accepts_leading_v_and_rejects_legacy_tags() {
        assert_eq!(
            parse_release_version("v1.1.0")
                .expect("leading-v version should parse")
                .to_string(),
            "1.1.0"
        );
        assert!(parse_release_version("release-0.6").is_none());
    }
}
