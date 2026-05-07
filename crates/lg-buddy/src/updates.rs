use std::error::Error;
use std::fmt;
use std::io;
use std::time::Duration;

use semver::Version;
use serde::Deserialize;

use crate::version::{ReleaseChannel, VersionInfo};

const GITHUB_RELEASES_API_BASE: &str =
    "https://api.github.com/repos/Staphylococcus/LG_Buddy/releases";
const GITHUB_API_VERSION: &str = "2026-03-10";
const GITHUB_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_CONNECT_TIMEOUT_SECONDS: u64 = 5;
const GITHUB_REQUEST_TIMEOUT_SECONDS: u64 = 20;
const PRERELEASE_PAGE_SIZE: u8 = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesCommand {
    Check { channel: Option<UpdateChannel> },
}

impl UpdatesCommand {
    pub fn parse<I, S>(args: I) -> Result<Self, UpdatesParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = args.into_iter();
        let Some(subcommand) = args.next() else {
            return Err(UpdatesParseError::MissingSubcommand);
        };

        match subcommand.as_ref() {
            "check" => parse_check_args(args),
            other => Err(UpdatesParseError::UnknownSubcommand(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Check { .. } => "check",
        }
    }
}

fn parse_check_args<I, S>(args: I) -> Result<UpdatesCommand, UpdatesParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let mut channel = None;

    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--channel" => {
                if channel.is_some() {
                    return Err(UpdatesParseError::DuplicateChannel);
                }

                let value = args.next().ok_or(UpdatesParseError::MissingChannelValue)?;
                channel = Some(UpdateChannel::parse(value.as_ref())?);
            }
            other => {
                let mut unexpected = vec![other.to_string()];
                unexpected.extend(args.map(|arg| arg.as_ref().to_string()));
                return Err(UpdatesParseError::UnexpectedArguments {
                    subcommand: "check",
                    arguments: unexpected,
                });
            }
        }
    }

    Ok(UpdatesCommand::Check { channel })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesParseError {
    MissingSubcommand,
    UnknownSubcommand(String),
    MissingChannelValue,
    DuplicateChannel,
    UnknownChannel(String),
    UnexpectedArguments {
        subcommand: &'static str,
        arguments: Vec<String>,
    },
}

impl fmt::Display for UpdatesParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSubcommand => write!(
                f,
                "missing updates command; expected `updates check [--channel stable|prerelease]`"
            ),
            Self::UnknownSubcommand(subcommand) => {
                write!(f, "unknown updates command `{subcommand}`")
            }
            Self::MissingChannelValue => write!(f, "missing channel value for `updates check`"),
            Self::DuplicateChannel => write!(f, "duplicate `--channel` option"),
            Self::UnknownChannel(channel) => write!(
                f,
                "unknown updates channel `{channel}`; expected stable or prerelease"
            ),
            Self::UnexpectedArguments {
                subcommand,
                arguments,
            } => write!(
                f,
                "unexpected arguments for `updates {subcommand}`: {}",
                arguments.join(" ")
            ),
        }
    }
}

impl Error for UpdatesParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    Stable,
    Prerelease,
}

impl UpdateChannel {
    fn parse(value: &str) -> Result<Self, UpdatesParseError> {
        match value {
            "stable" => Ok(Self::Stable),
            "prerelease" => Ok(Self::Prerelease),
            other => Err(UpdatesParseError::UnknownChannel(other.to_string())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }

    fn default_for(version: VersionInfo) -> Self {
        match version.channel() {
            ReleaseChannel::Prerelease => Self::Prerelease,
            ReleaseChannel::Dev | ReleaseChannel::Stable => Self::Stable,
        }
    }
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
    InvalidLocalVersion {
        version: String,
        source: semver::Error,
    },
    NoMatchingRelease {
        channel: UpdateChannel,
    },
    Io(io::Error),
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
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl Error for UpdatesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ApiShape { source, .. } => Some(source),
            Self::InvalidLocalVersion { source, .. } => Some(source),
            Self::Io(err) => Some(err),
            Self::Http { .. } | Self::ApiStatus { .. } | Self::NoMatchingRelease { .. } => None,
        }
    }
}

impl From<io::Error> for UpdatesError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    version: Version,
    channel: UpdateChannel,
    url: String,
}

impl ReleaseInfo {
    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn channel(&self) -> UpdateChannel {
        self.channel
    }

    pub fn url(&self) -> &str {
        &self.url
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckResult {
    current_version: Version,
    current_channel: ReleaseChannel,
    latest: ReleaseInfo,
}

impl UpdateCheckResult {
    pub fn update_available(&self) -> bool {
        self.latest.version > self.current_version
    }

    pub fn render(&self) -> String {
        let status = if self.update_available() {
            "update available"
        } else {
            "up to date"
        };

        format!(
            "status: {status}\ncurrent: {} ({})\nlatest: {} ({})\nurl: {}\n",
            self.current_version,
            self.current_channel.as_str(),
            self.latest.version(),
            self.latest.channel().as_str(),
            self.latest.url()
        )
    }
}

trait GitHubReleasesClient {
    fn get(&self, endpoint: ReleaseEndpoint, user_agent: &str) -> Result<String, UpdatesError>;
}

#[derive(Debug, Clone, Copy)]
enum ReleaseEndpoint {
    LatestStable,
    ReleasesList { per_page: u8 },
}

impl ReleaseEndpoint {
    fn url(self, base: &str) -> String {
        match self {
            Self::LatestStable => format!("{base}/latest"),
            Self::ReleasesList { per_page } => format!("{base}?per_page={per_page}"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::LatestStable => "latest",
            Self::ReleasesList { .. } => "releases",
        }
    }
}

struct UreqGitHubReleasesClient {
    base_url: &'static str,
    agent: ureq::Agent,
}

impl Default for UreqGitHubReleasesClient {
    fn default() -> Self {
        Self {
            base_url: GITHUB_RELEASES_API_BASE,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(GITHUB_CONNECT_TIMEOUT_SECONDS))
                .timeout(Duration::from_secs(GITHUB_REQUEST_TIMEOUT_SECONDS))
                .build(),
        }
    }
}

impl GitHubReleasesClient for UreqGitHubReleasesClient {
    fn get(&self, endpoint: ReleaseEndpoint, user_agent: &str) -> Result<String, UpdatesError> {
        let url = endpoint.url(self.base_url);
        let result = self
            .agent
            .get(&url)
            .set("Accept", GITHUB_ACCEPT)
            .set("User-Agent", user_agent)
            .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .call();

        match result {
            Ok(response) => response.into_string().map_err(|err| UpdatesError::Http {
                url,
                message: err.to_string(),
            }),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                Err(UpdatesError::ApiStatus { url, status, body })
            }
            Err(ureq::Error::Transport(err)) => Err(UpdatesError::Http {
                url,
                message: err.to_string(),
            }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
}

pub fn run_updates_command<W: io::Write>(
    command: UpdatesCommand,
    writer: &mut W,
) -> Result<(), UpdatesError> {
    let client = UreqGitHubReleasesClient::default();
    let version = VersionInfo::current();
    let result = check_updates(command, version, &client)?;

    writer.write_all(result.render().as_bytes())?;
    Ok(())
}

fn check_updates<C: GitHubReleasesClient>(
    command: UpdatesCommand,
    current: VersionInfo,
    client: &C,
) -> Result<UpdateCheckResult, UpdatesError> {
    match command {
        UpdatesCommand::Check { channel } => {
            let channel = channel.unwrap_or_else(|| UpdateChannel::default_for(current));
            let current_version = Version::parse(current.version()).map_err(|source| {
                UpdatesError::InvalidLocalVersion {
                    version: current.version().to_string(),
                    source,
                }
            })?;
            let latest = fetch_latest_release(channel, current, client)?;

            Ok(UpdateCheckResult {
                current_version,
                current_channel: current.channel(),
                latest,
            })
        }
    }
}

fn fetch_latest_release<C: GitHubReleasesClient>(
    channel: UpdateChannel,
    current: VersionInfo,
    client: &C,
) -> Result<ReleaseInfo, UpdatesError> {
    let user_agent = format!("lg-buddy/{}", current.version());

    match channel {
        UpdateChannel::Stable => {
            let endpoint = ReleaseEndpoint::LatestStable;
            let body = client.get(endpoint, &user_agent)?;
            let release: GitHubRelease =
                serde_json::from_str(&body).map_err(|source| UpdatesError::ApiShape {
                    endpoint: endpoint.label(),
                    source,
                })?;

            release_info_from_api_release(release, channel)
                .ok_or(UpdatesError::NoMatchingRelease { channel })
        }
        UpdateChannel::Prerelease => {
            let endpoint = ReleaseEndpoint::ReleasesList {
                per_page: PRERELEASE_PAGE_SIZE,
            };
            let body = client.get(endpoint, &user_agent)?;
            let releases: Vec<GitHubRelease> =
                serde_json::from_str(&body).map_err(|source| UpdatesError::ApiShape {
                    endpoint: endpoint.label(),
                    source,
                })?;

            releases
                .into_iter()
                .filter_map(|release| release_info_from_api_release(release, channel))
                .max_by(|left, right| left.version.cmp(&right.version))
                .ok_or(UpdatesError::NoMatchingRelease { channel })
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

    parse_release_version(&release.tag_name).map(|version| ReleaseInfo {
        version,
        channel: release_channel,
        url: release.html_url,
    })
}

fn parse_release_version(tag_name: &str) -> Option<Version> {
    Version::parse(tag_name.strip_prefix('v').unwrap_or(tag_name)).ok()
}

#[cfg(test)]
mod tests {
    use super::{
        check_updates, parse_release_version, GitHubReleasesClient, ReleaseEndpoint, UpdateChannel,
        UpdatesCommand, UpdatesError,
    };
    use crate::version::{ReleaseChannel, VersionInfo};
    use std::cell::RefCell;

    #[derive(Debug)]
    struct MockGitHubReleasesClient {
        responses: RefCell<Vec<Result<String, UpdatesError>>>,
        requests: RefCell<Vec<(String, String)>>,
    }

    impl MockGitHubReleasesClient {
        fn new(responses: Vec<Result<String, UpdatesError>>) -> Self {
            Self {
                responses: RefCell::new(responses),
                requests: RefCell::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<(String, String)> {
            self.requests.borrow().clone()
        }
    }

    impl GitHubReleasesClient for MockGitHubReleasesClient {
        fn get(&self, endpoint: ReleaseEndpoint, user_agent: &str) -> Result<String, UpdatesError> {
            self.requests.borrow_mut().push((
                endpoint.url("https://api.example.test/releases"),
                user_agent.to_string(),
            ));
            self.responses.borrow_mut().remove(0)
        }
    }

    fn version_info(version: &'static str, channel: ReleaseChannel) -> VersionInfo {
        VersionInfo::for_testing(version, channel, Some("test"))
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

    #[test]
    fn updates_check_uses_stable_channel_for_stable_builds_by_default() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.1"))]);

        let result = check_updates(
            UpdatesCommand::Check { channel: None },
            version_info("1.1.0", ReleaseChannel::Stable),
            &client,
        )
        .expect("stable update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            result.render(),
            "status: update available\ncurrent: 1.1.0 (stable)\nlatest: 1.1.1 (stable)\nurl: https://github.test/releases/tag/v1.1.1\n"
        );
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases/latest".to_string(),
                "lg-buddy/1.1.0".to_string()
            )]
        );
    }

    #[test]
    fn updates_check_uses_prerelease_channel_for_prerelease_builds_by_default() {
        let client = MockGitHubReleasesClient::new(vec![Ok(format!(
            "[{},{}]",
            stable_release("v1.1.0"),
            prerelease("v1.2.0-beta.1")
        ))]);

        let result = check_updates(
            UpdatesCommand::Check { channel: None },
            version_info("1.1.0-beta.1", ReleaseChannel::Prerelease),
            &client,
        )
        .expect("prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(result.latest.channel(), UpdateChannel::Prerelease);
        assert_eq!(
            client.requests(),
            vec![(
                "https://api.example.test/releases?per_page=20".to_string(),
                "lg-buddy/1.1.0-beta.1".to_string()
            )]
        );
    }

    #[test]
    fn updates_check_uses_stable_channel_for_dev_builds_by_default() {
        let client = MockGitHubReleasesClient::new(vec![Ok(stable_release("v1.1.0"))]);

        let result = check_updates(
            UpdatesCommand::Check { channel: None },
            version_info("1.1.0", ReleaseChannel::Dev),
            &client,
        )
        .expect("dev update check should succeed");

        assert!(!result.update_available());
        assert_eq!(
            result.render(),
            "status: up to date\ncurrent: 1.1.0 (dev)\nlatest: 1.1.0 (stable)\nurl: https://github.test/releases/tag/v1.1.0\n"
        );
    }

    #[test]
    fn explicit_stable_channel_reports_up_to_date_for_equal_or_older_versions() {
        for tag in ["v1.1.0", "v1.0.9"] {
            let client = MockGitHubReleasesClient::new(vec![Ok(stable_release(tag))]);

            let result = check_updates(
                UpdatesCommand::Check {
                    channel: Some(UpdateChannel::Stable),
                },
                version_info("1.1.0", ReleaseChannel::Stable),
                &client,
            )
            .expect("stable update check should succeed");

            assert!(!result.update_available(), "{tag} should not be newer");
        }
    }

    #[test]
    fn prerelease_channel_includes_stable_releases_and_picks_highest_semver() {
        let client = MockGitHubReleasesClient::new(vec![Ok(format!(
            "[{},{},{},{}]",
            draft_prerelease("v1.3.0-beta.1"),
            stable_release("v1.2.0"),
            prerelease("release-0.6"),
            prerelease("v1.2.0-beta.2")
        ))]);

        let result = check_updates(
            UpdatesCommand::Check {
                channel: Some(UpdateChannel::Prerelease),
            },
            version_info("1.2.0-beta.1", ReleaseChannel::Prerelease),
            &client,
        )
        .expect("prerelease update check should succeed");

        assert!(result.update_available());
        assert_eq!(
            result.render(),
            "status: update available\ncurrent: 1.2.0-beta.1 (prerelease)\nlatest: 1.2.0 (stable)\nurl: https://github.test/releases/tag/v1.2.0\n"
        );
    }

    #[test]
    fn stable_channel_rejects_prerelease_response() {
        let client = MockGitHubReleasesClient::new(vec![Ok(prerelease("v1.1.1-beta.1"))]);

        let err = check_updates(
            UpdatesCommand::Check {
                channel: Some(UpdateChannel::Stable),
            },
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
            UpdatesCommand::Check {
                channel: Some(UpdateChannel::Stable),
            },
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
            UpdatesCommand::Check {
                channel: Some(UpdateChannel::Stable),
            },
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
            UpdatesCommand::Check {
                channel: Some(UpdateChannel::Stable),
            },
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
        let client = MockGitHubReleasesClient::new(vec![Ok(format!(
            "[{},{}]",
            prerelease("release-0.6"),
            draft_prerelease("v1.2.0-beta.1")
        ))]);

        let err = check_updates(
            UpdatesCommand::Check {
                channel: Some(UpdateChannel::Prerelease),
            },
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

    #[test]
    fn invalid_local_version_is_reported() {
        let client = MockGitHubReleasesClient::new(vec![]);

        let err = check_updates(
            UpdatesCommand::Check {
                channel: Some(UpdateChannel::Stable),
            },
            version_info("not-semver", ReleaseChannel::Dev),
            &client,
        )
        .expect_err("invalid local version should fail update check");

        assert!(matches!(err, UpdatesError::InvalidLocalVersion { .. }));
        assert!(client.requests().is_empty());
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
