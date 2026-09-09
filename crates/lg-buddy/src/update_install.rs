use std::error::Error;
use std::fmt;
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::thread;

use semver::Version;

use crate::release_bundle::{
    acquire_release_bundle, resolve_release_identity, verify_release_binary_identity,
    verify_release_gui_binary_identity, BundleAcquisitionError, ReleaseIdentity,
    VerifiedReleaseBundle,
};
use crate::updates::{
    discover_install_candidate_for_channel, ReleaseInfo, UpdateChannel, UpdatesError,
};
use crate::upgrade_preflight::{current_host_preflight, CompatibilityAdvice, CompatibilityReport};
use crate::version::VersionInfo;

const GUI_TARGET: &str = "x86_64-unknown-linux-gnu";

#[derive(Debug)]
pub enum UpdateInstallError {
    Updates(UpdatesError),
    Bundle(BundleAcquisitionError),
    InvalidCurrentVersion {
        version: String,
        source: semver::Error,
    },
    InitialPreflight(CompatibilityReport),
    DowngradeRefused {
        current: Version,
        candidate: Version,
    },
    Output(io::Error),
    Cancelled,
    AuthorizationDeclined,
    AuthorizationUnavailable(Option<i32>),
    ChannelChanged {
        prepared: crate::updates::UpdateChannel,
        current: crate::updates::UpdateChannel,
    },
    ConfirmationRequiresTerminal,
    ConfirmationIo(io::Error),
    TargetChanged {
        confirmed: Box<ReleaseIdentity>,
        acquired: Box<ReleaseIdentity>,
    },
    CandidatePreflightLaunch(io::Error),
    /// Legacy CLI refusal shape retained for callers that only need the exit
    /// status. Graphical installs use [`Self::CandidatePreflightFailedWithAdvice`].
    CandidatePreflightFailed(Option<i32>),
    CandidatePreflightFailedWithAdvice {
        code: Option<i32>,
        output: String,
        advice: Option<String>,
    },
    InstallerLaunch(io::Error),
    InstallerFailed(Option<i32>),
    InstallerFailedWithOutput {
        code: Option<i32>,
        output: String,
        mutation_started: bool,
    },
    InstalledIdentity(BundleAcquisitionError),
    InstalledIdentityMismatch {
        expected: Box<ReleaseIdentity>,
        observed: Box<ReleaseIdentity>,
    },
}

impl fmt::Display for UpdateInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Updates(error) => write!(formatter, "update discovery failed: {error}"),
            Self::Bundle(error) => write!(formatter, "release bundle verification failed: {error}"),
            Self::InvalidCurrentVersion { version, source } => {
                write!(formatter, "installed version `{version}` is invalid: {source}")
            }
            Self::InitialPreflight(report) => formatter.write_str(&report.render()),
            Self::DowngradeRefused { current, candidate } => write!(
                formatter,
                "refusing to downgrade LG Buddy from {current} to {candidate}"
            ),
            Self::Output(error) => write!(formatter, "could not write upgrade output: {error}"),
            Self::Cancelled => formatter.write_str("upgrade cancelled before installation started"),
            Self::AuthorizationDeclined => {
                formatter.write_str("upgrade authorization was declined")
            }
            Self::AuthorizationUnavailable(code) => write!(
                formatter,
                "upgrade authorization was unavailable{}",
                render_exit_code(*code)
            ),
            Self::ChannelChanged { prepared, current } => write!(
                formatter,
                "saved update channel changed after confirmation (prepared {}, current {})",
                prepared.as_str(),
                current.as_str()
            ),
            Self::ConfirmationRequiresTerminal => write!(
                formatter,
                "upgrade confirmation requires an interactive terminal"
            ),
            Self::ConfirmationIo(error) => {
                write!(formatter, "could not read upgrade confirmation: {error}")
            }
            Self::TargetChanged {
                confirmed,
                acquired,
            } => write!(
                formatter,
                "verified release identity changed after confirmation (confirmed {confirmed:?}, acquired {acquired:?})"
            ),
            Self::CandidatePreflightLaunch(error) => {
                write!(formatter, "could not run candidate upgrade preflight: {error}")
            }
            Self::CandidatePreflightFailed(code) => write!(
                formatter,
                "candidate upgrade preflight refused the host{}",
                render_exit_code(*code)
            ),
            Self::CandidatePreflightFailedWithAdvice { code, output, .. } => {
                write!(formatter, "candidate upgrade preflight refused the host{}", render_exit_code(*code))?;
                if !output.is_empty() {
                    write!(formatter, ": {output}")?;
                }
                Ok(())
            }
            Self::InstallerLaunch(error) => {
                write!(formatter, "could not start the verified upgrade installer: {error}")
            }
            Self::InstallerFailed(code) => write!(
                formatter,
                "verified upgrade installer failed{}",
                render_exit_code(*code)
            ),
            Self::InstallerFailedWithOutput {
                code,
                output,
                mutation_started,
            } => {
                write!(
                    formatter,
                    "verified upgrade installer failed{}{}",
                    render_exit_code(*code),
                    if *mutation_started {
                        " after installation changes began"
                    } else {
                        " before installation changes began"
                    }
                )?;
                if !output.is_empty() {
                    write!(formatter, ": {output}")?;
                }
                Ok(())
            }
            Self::InstalledIdentity(error) => {
                write!(formatter, "installed release identity verification failed: {error}")
            }
            Self::InstalledIdentityMismatch { expected, observed } => write!(
                formatter,
                "installed release identity {observed:?} does not match verified release identity {expected:?}"
            ),
        }
    }
}

impl Error for UpdateInstallError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Updates(error) => Some(error),
            Self::Bundle(error) => Some(error),
            Self::InvalidCurrentVersion { source, .. } => Some(source),
            Self::Output(error)
            | Self::ConfirmationIo(error)
            | Self::CandidatePreflightLaunch(error)
            | Self::InstallerLaunch(error) => Some(error),
            Self::InstalledIdentity(error) => Some(error),
            Self::InitialPreflight(_)
            | Self::DowngradeRefused { .. }
            | Self::Cancelled
            | Self::AuthorizationDeclined
            | Self::AuthorizationUnavailable(_)
            | Self::ChannelChanged { .. }
            | Self::ConfirmationRequiresTerminal
            | Self::TargetChanged { .. }
            | Self::CandidatePreflightFailed(_)
            | Self::CandidatePreflightFailedWithAdvice { .. }
            | Self::InstallerFailed(_)
            | Self::InstallerFailedWithOutput { .. }
            | Self::InstalledIdentityMismatch { .. } => None,
        }
    }
}

impl UpdateInstallError {
    /// A bounded, user-facing explanation that omits host paths and command
    /// output. The `Display` implementation remains the diagnostic detail.
    pub fn user_message(&self) -> String {
        match self {
            Self::InitialPreflight(report) => {
                let details = report
                    .failures()
                    .iter()
                    .map(|failure| format!("{}: {}", failure.check, failure.remedy))
                    .collect::<Vec<_>>();
                if details.is_empty() {
                    "This host is not ready for an LG Buddy upgrade.".to_string()
                } else {
                    format!(
                        "This host is not ready for an LG Buddy upgrade: {}",
                        details.join("; ")
                    )
                }
            }
            Self::Cancelled => "The upgrade was cancelled before installation started.".to_string(),
            Self::AuthorizationDeclined => {
                "Administrator authorization was declined; the upgrade was not installed."
                    .to_string()
            }
            Self::AuthorizationUnavailable(_) => {
                "The desktop authorization agent could not authorize the update. Start LG Buddy from a normal desktop session and try again.".to_string()
            }
            Self::ChannelChanged { .. } => {
                "The saved update channel changed. Start the update again using the current preference."
                    .to_string()
            }
            Self::CandidatePreflightFailed(_) => {
                "The downloaded update failed the host compatibility checks.".to_string()
            }
            Self::CandidatePreflightFailedWithAdvice { advice, .. } => {
                if let Some(advice) = advice {
                    format!(
                        "The downloaded update failed the host compatibility checks. Details: {}",
                        advice.replace('\n', "; ")
                    )
                } else {
                    "The downloaded update failed the host compatibility checks.".to_string()
                }
            }
            Self::CandidatePreflightLaunch(_) => {
                "The downloaded update could not run its host compatibility checks.".to_string()
            }
            Self::InstallerFailed(_) => {
                "The update installer could not complete; the installation may be partial."
                    .to_string()
            }
            Self::InstallerLaunch(_) => {
                "The update installer could not be started, so installed files were not changed. Check that the verified bundle is executable and retry.".to_string()
            }
            Self::InstallerFailedWithOutput {
                mutation_started, ..
            } if *mutation_started => {
                "The update installer failed after installation changes began. The installation may be partial. Review the error details and address the cause before retrying."
                    .to_string()
            }
            Self::InstallerFailedWithOutput { .. } => {
                "The update installer stopped before changing installed files. Review the error details and address the cause before retrying."
                    .to_string()
            }
            Self::InstalledIdentity(_) | Self::InstalledIdentityMismatch { .. } => {
                "The installed files did not match the verified update identity.".to_string()
            }
            Self::TargetChanged { .. } => {
                "The release changed after confirmation. Start the update again to review the current release."
                    .to_string()
            }
            Self::ConfirmationRequiresTerminal | Self::ConfirmationIo(_) => {
                "The update requires explicit confirmation in a terminal.".to_string()
            }
            Self::DowngradeRefused { .. } => {
                "The available release is older than the installed version.".to_string()
            }
            Self::InvalidCurrentVersion { .. } => {
                "The installed LG Buddy version could not be compared.".to_string()
            }
            Self::Updates(_) => "The release information could not be retrieved.".to_string(),
            Self::Bundle(_) => "The release bundle could not be verified.".to_string(),
            Self::Output(_) => "The upgrade result could not be reported.".to_string(),
        }
    }
}

fn render_exit_code(code: Option<i32>) -> String {
    code.map(|code| format!(" with exit status {code}"))
        .unwrap_or_else(|| " after termination by signal".to_string())
}

const INSTALL_CANCELLATION_ACTIVE: u8 = 0;
const INSTALL_CANCELLATION_CLAIMED: u8 = 1;
const INSTALL_CANCELLATION_CANCELLED: u8 = 2;

/// Cancellation shared by update-install workers.
#[derive(Debug, Clone)]
pub struct UpdateInstallCancellation {
    state: Arc<AtomicU8>,
}

impl Default for UpdateInstallCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateInstallCancellation {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(INSTALL_CANCELLATION_ACTIVE)),
        }
    }

    /// Request cancellation. Returns false once cancellation was requested or
    /// the installer boundary has been claimed.
    pub fn cancel(&self) -> bool {
        self.state
            .compare_exchange(
                INSTALL_CANCELLATION_ACTIVE,
                INSTALL_CANCELLATION_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == INSTALL_CANCELLATION_CANCELLED
    }

    pub fn can_cancel(&self) -> bool {
        self.state.load(Ordering::Acquire) == INSTALL_CANCELLATION_ACTIVE
    }

    fn ensure_active(&self) -> Result<(), UpdateInstallError> {
        if self.is_cancelled() {
            Err(UpdateInstallError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Claim the point of no cancellation before invoking an installer.
    ///
    /// A successful claim makes subsequent [`Self::cancel`] calls return
    /// `false`; callers must invoke this immediately before the operation that
    /// may mutate the host.
    pub fn claim_installer_boundary(&self) -> Result<(), UpdateInstallError> {
        match self.state.compare_exchange(
            INSTALL_CANCELLATION_ACTIVE,
            INSTALL_CANCELLATION_CLAIMED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(INSTALL_CANCELLATION_CANCELLED) => Err(UpdateInstallError::Cancelled),
            Err(INSTALL_CANCELLATION_CLAIMED) => Err(UpdateInstallError::Cancelled),
            Err(_) => Err(UpdateInstallError::Cancelled),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateInstallStage {
    InitialPreflight,
    Discovering,
    Resolving,
    Offered,
    Acquiring,
    CandidatePreflight,
    Installing,
    VerifyingInstalled,
}

/// A release offer whose identity was resolved before the user confirms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedUpdateInstall {
    current: VersionInfo,
    release: ReleaseInfo,
    target: ReleaseIdentity,
    channel: UpdateChannel,
}

impl PreparedUpdateInstall {
    /// Construct a typed offer for a non-networking caller or test fixture.
    /// Production callers should prefer [`prepare_gui_update`].
    pub fn from_parts(
        current: VersionInfo,
        version: Version,
        channel: crate::updates::UpdateChannel,
        url: impl Into<String>,
        release_tag: impl Into<String>,
        target: impl Into<String>,
        commit: impl Into<String>,
    ) -> Self {
        let release_channel = if version.pre.is_empty() {
            UpdateChannel::Stable
        } else {
            UpdateChannel::Prerelease
        };
        let release = ReleaseInfo::from_github(
            version.clone(),
            release_channel,
            url.into(),
            release_tag.into(),
            Vec::new(),
        );
        let target = ReleaseIdentity::from_parts(
            release.tag_name().to_string(),
            version,
            release_channel,
            target,
            commit,
        );
        Self {
            current,
            release,
            target,
            channel,
        }
    }

    pub fn identity(&self) -> &ReleaseIdentity {
        &self.target
    }

    pub fn current(&self) -> VersionInfo {
        self.current
    }

    pub fn release(&self) -> &ReleaseInfo {
        &self.release
    }

    pub fn target(&self) -> &ReleaseIdentity {
        &self.target
    }

    pub fn channel(&self) -> crate::updates::UpdateChannel {
        self.channel
    }

    pub fn release_channel(&self) -> crate::updates::UpdateChannel {
        self.release.channel()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledUpdate {
    identity: ReleaseIdentity,
    gui_identity: ReleaseIdentity,
    runtime_path: PathBuf,
    gui_path: PathBuf,
}

impl InstalledUpdate {
    /// Construct an installed-result record for a non-networking caller or
    /// test fixture. Production callers receive this only after verification.
    pub fn from_parts(
        version: Version,
        channel: crate::updates::UpdateChannel,
        release_tag: impl Into<String>,
        target: impl Into<String>,
        commit: impl Into<String>,
        runtime_path: impl Into<PathBuf>,
        gui_path: impl Into<PathBuf>,
    ) -> Self {
        let release_tag = release_tag.into();
        let target = target.into();
        let commit = commit.into();
        let identity = ReleaseIdentity::from_parts(
            release_tag.clone(),
            version.clone(),
            channel,
            target,
            commit.clone(),
        );
        let gui_identity =
            ReleaseIdentity::from_parts(release_tag, version, channel, GUI_TARGET, commit);
        Self {
            identity,
            gui_identity,
            runtime_path: runtime_path.into(),
            gui_path: gui_path.into(),
        }
    }

    pub fn identity(&self) -> &ReleaseIdentity {
        &self.identity
    }

    pub fn gui_identity(&self) -> &ReleaseIdentity {
        &self.gui_identity
    }

    pub fn runtime_path(&self) -> &Path {
        &self.runtime_path
    }

    pub fn gui_path(&self) -> &Path {
        &self.gui_path
    }
}

trait BundleView {
    fn identity(&self) -> &ReleaseIdentity;
}

impl BundleView for VerifiedReleaseBundle {
    fn identity(&self) -> &ReleaseIdentity {
        self.identity()
    }
}

trait UpdateInstallRuntime {
    type Bundle: BundleView;

    fn current_version(&mut self) -> VersionInfo;
    fn current_channel(&mut self) -> Result<crate::updates::UpdateChannel, UpdateInstallError>;
    fn initial_preflight(&mut self) -> Result<(), UpdateInstallError>;
    fn discover_candidate(
        &mut self,
        current: VersionInfo,
        channel: UpdateChannel,
    ) -> Result<ReleaseInfo, UpdateInstallError>;
    fn resolve_target(
        &mut self,
        release: &ReleaseInfo,
    ) -> Result<ReleaseIdentity, UpdateInstallError>;
    fn confirm(&mut self) -> Result<bool, UpdateInstallError>;
    fn acquire(&mut self, release: &ReleaseInfo) -> Result<Self::Bundle, UpdateInstallError>;
    fn recheck_installed_state(&mut self) -> Result<(), UpdateInstallError> {
        Ok(())
    }
    fn candidate_preflight(&mut self, bundle: &Self::Bundle) -> Result<(), UpdateInstallError>;
    fn run_installer(&mut self, bundle: &Self::Bundle) -> Result<(), UpdateInstallError>;
    fn installed_update(
        &mut self,
        expected: &ReleaseIdentity,
    ) -> Result<InstalledUpdate, UpdateInstallError>;
}

struct SystemUpdateInstallRuntime {
    require_gui: bool,
}

impl UpdateInstallRuntime for SystemUpdateInstallRuntime {
    type Bundle = VerifiedReleaseBundle;

    fn current_version(&mut self) -> VersionInfo {
        VersionInfo::current()
    }

    fn current_channel(&mut self) -> Result<crate::updates::UpdateChannel, UpdateInstallError> {
        crate::updates::saved_update_channel().map_err(UpdateInstallError::Updates)
    }

    fn initial_preflight(&mut self) -> Result<(), UpdateInstallError> {
        let report = if self.require_gui {
            crate::upgrade_preflight::current_gui_host_preflight()
        } else {
            current_host_preflight()
        };
        if report.compatible() {
            Ok(())
        } else {
            Err(UpdateInstallError::InitialPreflight(report))
        }
    }

    fn discover_candidate(
        &mut self,
        current: VersionInfo,
        channel: UpdateChannel,
    ) -> Result<ReleaseInfo, UpdateInstallError> {
        discover_install_candidate_for_channel(current, channel)
            .map_err(UpdateInstallError::Updates)
    }

    fn resolve_target(
        &mut self,
        release: &ReleaseInfo,
    ) -> Result<ReleaseIdentity, UpdateInstallError> {
        resolve_release_identity(release).map_err(UpdateInstallError::Bundle)
    }

    fn recheck_installed_state(&mut self) -> Result<(), UpdateInstallError> {
        self.initial_preflight()
    }

    fn confirm(&mut self) -> Result<bool, UpdateInstallError> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(UpdateInstallError::ConfirmationRequiresTerminal);
        }
        let mut answer = String::new();
        io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(UpdateInstallError::ConfirmationIo)?;
        Ok(confirmation_is_yes(&answer))
    }

    fn acquire(&mut self, release: &ReleaseInfo) -> Result<Self::Bundle, UpdateInstallError> {
        acquire_release_bundle(release).map_err(UpdateInstallError::Bundle)
    }

    fn candidate_preflight(&mut self, bundle: &Self::Bundle) -> Result<(), UpdateInstallError> {
        if self.require_gui {
            let output = run_bounded_command(candidate_preflight_command_with_json(bundle.root()))
                .map_err(UpdateInstallError::CandidatePreflightLaunch)?;
            if output.status.success() {
                Ok(())
            } else {
                let (output, advice) = candidate_preflight_failure(&output);
                Err(UpdateInstallError::CandidatePreflightFailedWithAdvice {
                    code: output.0,
                    output: output.1,
                    advice,
                })
            }
        } else {
            let status = candidate_preflight_command(bundle.root())
                .status()
                .map_err(UpdateInstallError::CandidatePreflightLaunch)?;
            if status.success() {
                Ok(())
            } else {
                Err(UpdateInstallError::CandidatePreflightFailed(status.code()))
            }
        }
    }

    fn run_installer(&mut self, bundle: &Self::Bundle) -> Result<(), UpdateInstallError> {
        let mut command = if self.require_gui {
            gui_installer_command(bundle.root())
        } else {
            installer_command(bundle.root())
        };
        if self.require_gui {
            let output =
                run_bounded_command(command).map_err(UpdateInstallError::InstallerLaunch)?;
            classify_gui_installer_output(&output)
        } else {
            let status = command
                .status()
                .map_err(UpdateInstallError::InstallerLaunch)?;
            if status.success() {
                Ok(())
            } else {
                Err(UpdateInstallError::InstallerFailed(status.code()))
            }
        }
    }

    fn installed_update(
        &mut self,
        expected: &ReleaseIdentity,
    ) -> Result<InstalledUpdate, UpdateInstallError> {
        let runtime_path = installed_binary_path();
        let gui_path = installed_gui_binary_path();
        let identity = verify_release_binary_identity(&runtime_path, expected)
            .map_err(UpdateInstallError::InstalledIdentity)?;
        let gui_identity = verify_release_gui_binary_identity(&gui_path, expected)
            .map_err(UpdateInstallError::InstalledIdentity)?;
        Ok(InstalledUpdate {
            identity,
            gui_identity,
            runtime_path,
            gui_path,
        })
    }
}

/// Prepare a GUI-triggered update using the currently saved update channel.
/// The release is discovered again so the confirmation dialog describes the
/// latest qualifying release at the time it opens.
pub fn prepare_gui_update() -> Result<Option<PreparedUpdateInstall>, UpdateInstallError> {
    let cancellation = UpdateInstallCancellation::new();
    let mut runtime = SystemUpdateInstallRuntime { require_gui: true };
    let current = runtime.current_version();
    prepare_gui_update_with(&mut runtime, &cancellation, current)
}

/// Install a previously prepared and explicitly confirmed GUI update.
pub fn install_gui_update(
    prepared: &PreparedUpdateInstall,
    cancellation: &UpdateInstallCancellation,
    progress: &mut dyn FnMut(UpdateInstallStage),
) -> Result<InstalledUpdate, UpdateInstallError> {
    let mut runtime = SystemUpdateInstallRuntime { require_gui: true };
    install_prepared_update_with(prepared, &mut runtime, cancellation, progress)
}

fn prepare_gui_update_with<R: UpdateInstallRuntime>(
    runtime: &mut R,
    cancellation: &UpdateInstallCancellation,
    current: VersionInfo,
) -> Result<Option<PreparedUpdateInstall>, UpdateInstallError> {
    match prepare_update_install_with(runtime, cancellation, current) {
        Err(UpdateInstallError::DowngradeRefused { .. }) => Ok(None),
        result => result,
    }
}

fn prepare_update_install_with<R: UpdateInstallRuntime>(
    runtime: &mut R,
    cancellation: &UpdateInstallCancellation,
    current: VersionInfo,
) -> Result<Option<PreparedUpdateInstall>, UpdateInstallError> {
    cancellation.ensure_active()?;
    runtime.initial_preflight()?;
    cancellation.ensure_active()?;
    let current_version = Version::parse(current.version()).map_err(|source| {
        UpdateInstallError::InvalidCurrentVersion {
            version: current.version().to_string(),
            source,
        }
    })?;
    let channel = runtime.current_channel()?;
    let release = runtime.discover_candidate(current, channel)?;
    cancellation.ensure_active()?;

    match release.version().cmp(&current_version) {
        std::cmp::Ordering::Equal => return Ok(None),
        std::cmp::Ordering::Less => {
            return Err(UpdateInstallError::DowngradeRefused {
                current: current_version,
                candidate: release.version().clone(),
            });
        }
        std::cmp::Ordering::Greater => {}
    }

    let target = runtime.resolve_target(&release)?;
    cancellation.ensure_active()?;
    Ok(Some(PreparedUpdateInstall {
        current,
        release,
        target,
        channel,
    }))
}

fn install_prepared_update_with<R: UpdateInstallRuntime>(
    prepared: &PreparedUpdateInstall,
    runtime: &mut R,
    cancellation: &UpdateInstallCancellation,
    progress: &mut dyn FnMut(UpdateInstallStage),
) -> Result<InstalledUpdate, UpdateInstallError> {
    cancellation.ensure_active()?;
    let current_channel = runtime.current_channel()?;
    if current_channel != prepared.channel() {
        return Err(UpdateInstallError::ChannelChanged {
            prepared: prepared.channel(),
            current: current_channel,
        });
    }
    cancellation.ensure_active()?;

    progress(UpdateInstallStage::Acquiring);
    cancellation.ensure_active()?;
    let bundle = runtime.acquire(prepared.release())?;
    cancellation.ensure_active()?;
    if bundle.identity() != prepared.target() {
        return Err(UpdateInstallError::TargetChanged {
            confirmed: Box::new(prepared.target().clone()),
            acquired: Box::new(bundle.identity().clone()),
        });
    }

    // The confirmation may have been held while another process changed the
    // installed layout. Recheck it while the verified bundle remains alive so
    // stale offers cannot proceed to candidate checks or installation.
    runtime.recheck_installed_state()?;
    cancellation.ensure_active()?;

    progress(UpdateInstallStage::CandidatePreflight);
    cancellation.ensure_active()?;
    runtime.candidate_preflight(&bundle)?;
    cancellation.ensure_active()?;

    // This compare-exchange is the final cancellation boundary. Once it wins,
    // cancellation returns false and the installer may mutate the host.
    cancellation.claim_installer_boundary()?;
    progress(UpdateInstallStage::Installing);
    runtime.run_installer(&bundle)?;

    progress(UpdateInstallStage::VerifyingInstalled);
    let installed = runtime.installed_update(bundle.identity())?;
    if installed.identity() != bundle.identity() {
        return Err(UpdateInstallError::InstalledIdentityMismatch {
            expected: Box::new(bundle.identity().clone()),
            observed: Box::new(installed.identity().clone()),
        });
    }
    let expected_gui = ReleaseIdentity::from_parts(
        bundle.identity().release_tag().to_string(),
        bundle.identity().version().clone(),
        bundle.identity().channel(),
        GUI_TARGET,
        bundle.identity().commit().to_string(),
    );
    if installed.gui_identity() != &expected_gui {
        return Err(UpdateInstallError::InstalledIdentityMismatch {
            expected: Box::new(expected_gui),
            observed: Box::new(installed.gui_identity().clone()),
        });
    }
    Ok(installed)
}

fn candidate_preflight_command(candidate_root: &Path) -> Command {
    let mut command = Command::new(candidate_root.join("lg-buddy"));
    command.arg("upgrade-preflight").arg(candidate_root);
    command
}

fn candidate_preflight_command_with_json(candidate_root: &Path) -> Command {
    let mut command = candidate_preflight_command(candidate_root);
    command.arg("--json");
    command
}

fn installer_command(candidate_root: &Path) -> Command {
    let mut command = Command::new(candidate_root.join("install.sh"));
    command.arg("--upgrade");
    command
}

fn gui_installer_command(candidate_root: &Path) -> Command {
    let mut command = installer_command(candidate_root);
    command
        .env("LG_BUDDY_AUTO_INSTALL_DEPS", "n")
        .env("LG_BUDDY_NONINTERACTIVE", "1")
        .env("LG_BUDDY_SUDO_CMD", "pkexec")
        .stdin(Stdio::null());
    // The install-root override is the supported isolated-install test
    // environment. Outside it, inherited test/debug switches must not allow a
    // graphical upgrade to publish without system integration actions.
    if std::env::var_os("LG_BUDDY_INSTALL_ROOT").is_none() {
        command
            .env("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "0")
            .env("LG_BUDDY_SKIP_PIP_INSTALL", "0");
    }
    command
}

fn installer_mutation_started(output: &str) -> bool {
    output
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .any(|line| line == "LG_BUDDY_INSTALL_STATUS=mutation_started")
}

fn classify_gui_installer_output(output: &BoundedCommandOutput) -> Result<(), UpdateInstallError> {
    if output.status.success() {
        return Ok(());
    }
    let mutation_started =
        output.stdout_mutation_started || installer_mutation_started(&output.stdout);
    if !mutation_started && output.status.code() == Some(126) {
        Err(UpdateInstallError::AuthorizationDeclined)
    } else if !mutation_started && output.status.code() == Some(127) {
        Err(UpdateInstallError::AuthorizationUnavailable(
            output.status.code(),
        ))
    } else {
        Err(UpdateInstallError::InstallerFailedWithOutput {
            code: output.status.code(),
            mutation_started,
            output: output.diagnostic(),
        })
    }
}

fn confirmation_is_yes(answer: &str) -> bool {
    answer.trim() == "yes"
}

const MAX_CANDIDATE_PREFLIGHT_OUTPUT_BYTES: usize = 64 * 1024;

struct BoundedCommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    stdout_mutation_started: bool,
}

impl BoundedCommandOutput {
    fn diagnostic(&self) -> String {
        bounded_combined_text(
            &self.stderr,
            &self.stdout,
            self.stdout_truncated || self.stderr_truncated,
        )
    }
}

/// Run a diagnostic subprocess while draining both pipes. Each reader keeps
/// only a bounded prefix, so a noisy candidate cannot exhaust memory or block
/// waiting for the other pipe to be drained.
fn run_bounded_command(mut command: Command) -> io::Result<BoundedCommandOutput> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = thread::spawn(|| read_bounded(stdout));
    let stderr_reader = thread::spawn(|| read_bounded(stderr));
    let status = child.wait()?;
    let (stdout, stdout_truncated, stdout_mutation_started) = join_bounded_reader(stdout_reader)?;
    let (stderr, stderr_truncated, _) = join_bounded_reader(stderr_reader)?;

    Ok(BoundedCommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).trim().to_string(),
        stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
        stdout_truncated,
        stderr_truncated,
        stdout_mutation_started,
    })
}

const INSTALL_MUTATION_STATUS: &[u8] = b"LG_BUDDY_INSTALL_STATUS=mutation_started";

fn read_bounded(mut reader: impl Read) -> io::Result<(Vec<u8>, bool, bool)> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    let mut line = Vec::new();
    let mut mutation_started = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            if line == INSTALL_MUTATION_STATUS
                || (line.last() == Some(&b'\r')
                    && line[..line.len() - 1] == INSTALL_MUTATION_STATUS[..])
            {
                mutation_started = true;
            }
            break;
        }
        for byte in &buffer[..read] {
            if *byte == b'\n' {
                if line == INSTALL_MUTATION_STATUS
                    || (line.last() == Some(&b'\r')
                        && line[..line.len() - 1] == INSTALL_MUTATION_STATUS[..])
                {
                    mutation_started = true;
                }
                line.clear();
            } else if line.len() <= INSTALL_MUTATION_STATUS.len() {
                line.push(*byte);
            }
        }
        let remaining = MAX_CANDIDATE_PREFLIGHT_OUTPUT_BYTES.saturating_sub(bytes.len());
        if remaining > 0 {
            let keep = read.min(remaining);
            bytes.extend_from_slice(&buffer[..keep]);
            truncated |= keep < read;
        } else {
            truncated = true;
        }
    }
    Ok((bytes, truncated, mutation_started))
}

fn join_bounded_reader(
    reader: thread::JoinHandle<io::Result<(Vec<u8>, bool, bool)>>,
) -> io::Result<(Vec<u8>, bool, bool)> {
    reader
        .join()
        .map_err(|_| io::Error::other("bounded output reader panicked"))?
}

fn candidate_preflight_failure(
    output: &BoundedCommandOutput,
) -> ((Option<i32>, String), Option<String>) {
    let advice = serde_json::from_str::<CompatibilityAdvice>(&output.stdout).ok();
    let safe_advice = advice
        .as_ref()
        .filter(|advice| !advice.compatible && !advice.failures.is_empty())
        .map(|advice| {
            advice
                .failures
                .iter()
                .map(|failure| format!("- {} Remedy: {}", failure.check, failure.remedy))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let advice = (!safe_advice.is_empty()).then_some(safe_advice.clone());
    let diagnostic = if output.stderr.is_empty() {
        if advice.is_none() {
            output.stdout.as_str()
        } else {
            ""
        }
    } else {
        output.stderr.as_str()
    };
    let diagnostic = if !safe_advice.is_empty() && !diagnostic.is_empty() {
        format!("[candidate diagnostic]\n{diagnostic}")
    } else {
        diagnostic.to_string()
    };
    let diagnostic = bounded_combined_text(
        &safe_advice,
        &diagnostic,
        output.stdout_truncated || output.stderr_truncated,
    );
    ((output.status.code(), diagnostic), advice)
}

fn bounded_combined_text(first: &str, second: &str, source_truncated: bool) -> String {
    let mut bytes = Vec::with_capacity(first.len() + second.len() + 1);
    bytes.extend_from_slice(first.as_bytes());
    if !first.is_empty() && !second.is_empty() {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(second.as_bytes());
    let truncated = source_truncated || bytes.len() > MAX_CANDIDATE_PREFLIGHT_OUTPUT_BYTES;
    bytes.truncate(MAX_CANDIDATE_PREFLIGHT_OUTPUT_BYTES);
    let mut text = String::from_utf8_lossy(&bytes).trim().to_string();
    if truncated {
        text.push_str(" [output truncated]");
    }
    text
}

fn installed_binary_path() -> PathBuf {
    let install_root = std::env::var_os("LG_BUDDY_INSTALL_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from);
    installed_binary_path_for_root(install_root)
}

fn installed_gui_binary_path() -> PathBuf {
    let install_root = std::env::var_os("LG_BUDDY_INSTALL_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from);
    installed_binary_path_for_root(install_root).with_file_name("lg-buddy-gui")
}

fn installed_binary_path_for_root(install_root: Option<PathBuf>) -> PathBuf {
    install_root
        .unwrap_or_else(|| PathBuf::from("/"))
        .join("usr/bin/lg-buddy")
}

pub fn run_update_install<W: Write>(writer: &mut W) -> Result<(), UpdateInstallError> {
    run_update_install_with(
        writer,
        &mut SystemUpdateInstallRuntime { require_gui: false },
    )
}

fn run_update_install_with<W: Write, R: UpdateInstallRuntime>(
    writer: &mut W,
    runtime: &mut R,
) -> Result<(), UpdateInstallError> {
    let cancellation = UpdateInstallCancellation::new();
    let current = runtime.current_version();
    let Some(prepared) = prepare_update_install_with(runtime, &cancellation, current)? else {
        writeln!(
            writer,
            "LG Buddy {} ({}) is already up to date.",
            current.version(),
            current.channel().as_str()
        )
        .map_err(UpdateInstallError::Output)?;
        return Ok(());
    };

    let current = prepared.current();
    writeln!(
        writer,
        "Current: {} ({}, commit {})",
        current.version(),
        current.channel().as_str(),
        current.commit().unwrap_or("unknown")
    )
    .map_err(UpdateInstallError::Output)?;
    writeln!(
        writer,
        "Target: {} ({}, commit {})",
        prepared.target().version(),
        prepared.target().channel().as_str(),
        prepared.target().commit()
    )
    .map_err(UpdateInstallError::Output)?;
    writeln!(writer, "Release: {}", prepared.release().url())
        .map_err(UpdateInstallError::Output)?;
    write!(writer, "Type `yes` to install this update: ").map_err(UpdateInstallError::Output)?;
    writer.flush().map_err(UpdateInstallError::Output)?;

    if !runtime.confirm()? {
        writeln!(writer, "Upgrade cancelled.").map_err(UpdateInstallError::Output)?;
        return Ok(());
    }

    let installed = install_prepared_update_with(&prepared, runtime, &cancellation, &mut |_| {})?;
    writeln!(
        writer,
        "Installed: {} ({}, commit {})",
        installed.identity().version(),
        installed.identity().channel().as_str(),
        installed.identity().commit()
    )
    .map_err(UpdateInstallError::Output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        candidate_preflight_command, classify_gui_installer_output, confirmation_is_yes,
        install_prepared_update_with, installed_binary_path_for_root, installer_command,
        prepare_gui_update_with, prepare_update_install_with, run_bounded_command,
        run_update_install_with, BundleView, CompatibilityReport, InstalledUpdate,
        UpdateInstallCancellation, UpdateInstallError, UpdateInstallRuntime, UpdateInstallStage,
        GUI_TARGET,
    };
    use crate::release_bundle::{BundleAcquisitionError, ReleaseIdentity};
    use crate::updates::{ReleaseInfo, UpdateChannel};
    use crate::version::{ReleaseChannel, VersionInfo};
    use semver::Version;
    use std::cell::RefCell;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::rc::Rc;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Failure {
        Initial,
        Resolve,
        Confirmation,
        Acquire,
        CandidatePreflight,
        Installer,
        InstalledIdentity,
        Recheck,
    }

    struct FakeBundle {
        identity: ReleaseIdentity,
        events: Rc<RefCell<Vec<&'static str>>>,
    }

    impl BundleView for FakeBundle {
        fn identity(&self) -> &ReleaseIdentity {
            &self.identity
        }
    }

    impl Drop for FakeBundle {
        fn drop(&mut self) {
            self.events.borrow_mut().push("drop_bundle");
        }
    }

    struct FakeRuntime {
        events: Rc<RefCell<Vec<&'static str>>>,
        current_version: &'static str,
        candidate_version: &'static str,
        saved_channel: UpdateChannel,
        discovered_channel: Option<UpdateChannel>,
        confirmed: bool,
        failure: Option<Failure>,
        recheck: bool,
        resolved_identity: ReleaseIdentity,
        acquired_identity: ReleaseIdentity,
        installed_identity: ReleaseIdentity,
    }

    impl FakeRuntime {
        fn new(candidate_version: &'static str) -> Self {
            let events = Rc::new(RefCell::new(Vec::new()));
            let identity = identity(candidate_version, "target-commit");
            Self {
                events,
                current_version: "1.4.0",
                candidate_version,
                saved_channel: UpdateChannel::Stable,
                discovered_channel: None,
                confirmed: true,
                failure: None,
                recheck: false,
                resolved_identity: identity.clone(),
                acquired_identity: identity.clone(),
                installed_identity: identity,
            }
        }

        fn event_names(&self) -> Vec<&'static str> {
            self.events.borrow().clone()
        }
    }

    impl UpdateInstallRuntime for FakeRuntime {
        type Bundle = FakeBundle;

        fn current_version(&mut self) -> VersionInfo {
            self.events.borrow_mut().push("current");
            VersionInfo::for_testing(
                self.current_version,
                ReleaseChannel::Stable,
                Some("current-commit"),
            )
        }

        fn current_channel(&mut self) -> Result<UpdateChannel, UpdateInstallError> {
            Ok(self.saved_channel)
        }

        fn initial_preflight(&mut self) -> Result<(), UpdateInstallError> {
            self.events.borrow_mut().push("initial_preflight");
            if self.failure == Some(Failure::Initial) {
                Err(UpdateInstallError::ConfirmationRequiresTerminal)
            } else {
                Ok(())
            }
        }

        fn discover_candidate(
            &mut self,
            _current: VersionInfo,
            channel: UpdateChannel,
        ) -> Result<ReleaseInfo, UpdateInstallError> {
            self.events.borrow_mut().push("discover");
            self.discovered_channel = Some(channel);
            Ok(ReleaseInfo::from_github(
                Version::parse(self.candidate_version).unwrap(),
                UpdateChannel::Stable,
                format!("https://example.test/releases/v{}", self.candidate_version),
                format!("v{}", self.candidate_version),
                Vec::new(),
            ))
        }

        fn resolve_target(
            &mut self,
            _release: &ReleaseInfo,
        ) -> Result<ReleaseIdentity, UpdateInstallError> {
            self.events.borrow_mut().push("resolve");
            if self.failure == Some(Failure::Resolve) {
                Err(bundle_error())
            } else {
                Ok(self.resolved_identity.clone())
            }
        }

        fn confirm(&mut self) -> Result<bool, UpdateInstallError> {
            self.events.borrow_mut().push("confirm");
            if self.failure == Some(Failure::Confirmation) {
                Err(UpdateInstallError::ConfirmationRequiresTerminal)
            } else {
                Ok(self.confirmed)
            }
        }

        fn acquire(&mut self, _release: &ReleaseInfo) -> Result<Self::Bundle, UpdateInstallError> {
            self.events.borrow_mut().push("acquire");
            if self.failure == Some(Failure::Acquire) {
                Err(UpdateInstallError::Bundle(
                    BundleAcquisitionError::ConcurrentAcquisition,
                ))
            } else {
                Ok(FakeBundle {
                    identity: self.acquired_identity.clone(),
                    events: Rc::clone(&self.events),
                })
            }
        }

        fn recheck_installed_state(&mut self) -> Result<(), UpdateInstallError> {
            if self.recheck {
                self.events.borrow_mut().push("recheck");
                if self.failure == Some(Failure::Recheck) {
                    return Err(UpdateInstallError::InitialPreflight(
                        CompatibilityReport::default(),
                    ));
                }
            }
            Ok(())
        }

        fn candidate_preflight(
            &mut self,
            _bundle: &Self::Bundle,
        ) -> Result<(), UpdateInstallError> {
            self.events.borrow_mut().push("candidate_preflight");
            if self.failure == Some(Failure::CandidatePreflight) {
                Err(UpdateInstallError::CandidatePreflightFailed(Some(1)))
            } else {
                Ok(())
            }
        }

        fn run_installer(&mut self, _bundle: &Self::Bundle) -> Result<(), UpdateInstallError> {
            self.events.borrow_mut().push("installer");
            if self.failure == Some(Failure::Installer) {
                Err(UpdateInstallError::InstallerFailed(Some(1)))
            } else {
                Ok(())
            }
        }

        fn installed_update(
            &mut self,
            _expected: &ReleaseIdentity,
        ) -> Result<InstalledUpdate, UpdateInstallError> {
            self.events.borrow_mut().push("installed_identity");
            if self.failure == Some(Failure::InstalledIdentity) {
                Err(UpdateInstallError::InstalledIdentity(
                    BundleAcquisitionError::Binary("mismatch".to_string()),
                ))
            } else {
                let gui_identity = ReleaseIdentity::from_parts(
                    self.installed_identity.release_tag().to_string(),
                    self.installed_identity.version().clone(),
                    self.installed_identity.channel(),
                    GUI_TARGET,
                    self.installed_identity.commit().to_string(),
                );
                Ok(InstalledUpdate {
                    identity: self.installed_identity.clone(),
                    gui_identity,
                    runtime_path: PathBuf::from("/usr/bin/lg-buddy"),
                    gui_path: PathBuf::from("/usr/bin/lg-buddy-gui"),
                })
            }
        }
    }

    fn identity(version: &str, commit: &str) -> ReleaseIdentity {
        ReleaseIdentity::from_parts(
            format!("v{version}"),
            Version::parse(version).unwrap(),
            UpdateChannel::Stable,
            "x86_64-unknown-linux-musl",
            commit,
        )
    }

    fn bundle_error() -> UpdateInstallError {
        UpdateInstallError::Bundle(BundleAcquisitionError::ReleaseMetadata(
            "test failure".to_string(),
        ))
    }

    #[test]
    fn candidate_and_installer_processes_use_exact_argv_without_a_shell() {
        let root = Path::new("/tmp/release bundle; touch escaped");
        let candidate = candidate_preflight_command(root);
        assert_eq!(
            candidate.get_program(),
            OsStr::new("/tmp/release bundle; touch escaped/lg-buddy")
        );
        assert_eq!(
            candidate.get_args().collect::<Vec<_>>(),
            [OsStr::new("upgrade-preflight"), root.as_os_str()]
        );

        let installer = installer_command(root);
        assert_eq!(
            installer.get_program(),
            OsStr::new("/tmp/release bundle; touch escaped/install.sh")
        );
        assert_eq!(
            installer.get_args().collect::<Vec<_>>(),
            [OsStr::new("--upgrade")]
        );
    }

    #[test]
    fn confirmation_accepts_only_an_explicit_lowercase_yes() {
        assert!(confirmation_is_yes("yes\n"));
        for answer in ["", "y", "YES", "yes please", "no"] {
            assert!(!confirmation_is_yes(answer), "accepted `{answer}`");
        }
    }

    fn installer_fixture(script: &str) -> super::BoundedCommandOutput {
        let mut command = Command::new("sh");
        command.arg("-c").arg(script);
        run_bounded_command(command).unwrap()
    }

    #[test]
    fn graphical_installer_status_distinguishes_authorization_and_partial_failures() {
        let declined = installer_fixture("exit 126");
        assert!(matches!(
            classify_gui_installer_output(&declined),
            Err(UpdateInstallError::AuthorizationDeclined)
        ));

        let unavailable = installer_fixture("exit 127");
        assert!(matches!(
            classify_gui_installer_output(&unavailable),
            Err(UpdateInstallError::AuthorizationUnavailable(Some(127)))
        ));

        let partial =
            installer_fixture("printf 'LG_BUDDY_INSTALL_STATUS=mutation_started\\n'; exit 126");
        assert!(matches!(
            classify_gui_installer_output(&partial),
            Err(UpdateInstallError::InstallerFailedWithOutput {
                mutation_started: true,
                code: Some(126),
                ..
            })
        ));
    }

    #[test]
    fn installer_diagnostics_prioritize_stderr_over_progress_output() {
        let output = installer_fixture(
            "printf '%70000s' progress; printf 'install: No space left on device\\n' >&2; exit 1",
        );
        let diagnostic = output.diagnostic();
        assert!(diagnostic.starts_with("install: No space left on device"));
        assert!(diagnostic.ends_with("[output truncated]"));
    }

    #[test]
    fn graphical_installer_status_is_found_after_bounded_output_prefix() {
        let output = installer_fixture(
            "awk 'BEGIN { for (i = 0; i < 70000; i++) printf \"x\" }'; printf '\\nLG_BUDDY_INSTALL_STATUS=mutation_started\\n'; exit 126",
        );
        assert!(matches!(
            classify_gui_installer_output(&output),
            Err(UpdateInstallError::InstallerFailedWithOutput {
                mutation_started: true,
                code: Some(126),
                ..
            })
        ));
    }

    #[test]
    fn installed_binary_path_is_absolute_without_an_install_root_override() {
        assert_eq!(
            installed_binary_path_for_root(None),
            Path::new("/usr/bin/lg-buddy")
        );
        assert_eq!(
            installed_binary_path_for_root(Some(PathBuf::from("/tmp/install-root"))),
            Path::new("/tmp/install-root/usr/bin/lg-buddy")
        );
    }

    #[test]
    fn initial_refusal_stops_before_discovery() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Initial);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(runtime.event_names(), ["current", "initial_preflight"]);
    }

    #[test]
    fn invalid_current_version_stops_before_discovery() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.current_version = "invalid";

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::InvalidCurrentVersion { .. }
        ));
        assert_eq!(runtime.event_names(), ["current", "initial_preflight"]);
    }

    #[test]
    fn equal_version_stops_before_resolution_and_confirmation() {
        let mut runtime = FakeRuntime::new("1.4.0");
        let mut output = Vec::new();

        run_update_install_with(&mut output, &mut runtime).unwrap();

        assert_eq!(
            runtime.event_names(),
            ["current", "initial_preflight", "discover"]
        );
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("already up to date"));
    }

    #[test]
    fn gui_preparation_uses_saved_channel_and_resolves_latest_without_acquiring() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.saved_channel = UpdateChannel::Prerelease;
        let cancellation = UpdateInstallCancellation::new();
        let current = runtime.current_version();

        let prepared = prepare_gui_update_with(&mut runtime, &cancellation, current)
            .unwrap()
            .unwrap();

        assert_eq!(runtime.discovered_channel, Some(UpdateChannel::Prerelease));
        assert_eq!(prepared.channel(), UpdateChannel::Prerelease);
        assert_eq!(prepared.release_channel(), UpdateChannel::Stable);
        assert_eq!(
            prepared.identity().version(),
            &Version::parse("1.5.0").unwrap()
        );
        assert_eq!(
            runtime.event_names(),
            ["current", "initial_preflight", "discover", "resolve"]
        );
    }

    #[test]
    fn gui_preparation_returns_none_for_equal_or_older_release() {
        for candidate_version in ["1.4.0", "1.3.0"] {
            let mut runtime = FakeRuntime::new(candidate_version);
            let cancellation = UpdateInstallCancellation::new();
            let current = runtime.current_version();

            let prepared = prepare_gui_update_with(&mut runtime, &cancellation, current).unwrap();

            assert!(
                prepared.is_none(),
                "candidate {candidate_version} should not be offered"
            );
            assert_eq!(
                runtime.event_names(),
                ["current", "initial_preflight", "discover"]
            );
        }
    }

    #[test]
    fn downgrade_stops_before_resolution_and_confirmation() {
        let mut runtime = FakeRuntime::new("1.3.0");

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(error, UpdateInstallError::DowngradeRefused { .. }));
        assert_eq!(
            runtime.event_names(),
            ["current", "initial_preflight", "discover"]
        );
    }

    #[test]
    fn declined_confirmation_does_not_acquire_or_mutate() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.confirmed = false;

        run_update_install_with(&mut Vec::new(), &mut runtime).unwrap();

        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm"
            ]
        );
    }

    #[test]
    fn unavailable_terminal_stops_before_acquisition() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Confirmation);

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::ConfirmationRequiresTerminal
        ));
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm"
            ]
        );
    }

    #[test]
    fn acquisition_failure_does_not_run_candidate_or_installer() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Acquire);

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::Bundle(BundleAcquisitionError::ConcurrentAcquisition)
        ));
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire"
            ]
        );
    }

    #[test]
    fn changed_identity_stops_before_candidate_preflight() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.acquired_identity = identity("1.5.0", "different-commit");

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(error, UpdateInstallError::TargetChanged { .. }));
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn installed_state_is_rechecked_after_acquisition_before_candidate_preflight() {
        let mut runtime = FakeRuntime::new("1.5.0");
        let cancellation = UpdateInstallCancellation::new();
        let current = runtime.current_version();
        let prepared = prepare_update_install_with(&mut runtime, &cancellation, current)
            .unwrap()
            .unwrap();
        runtime.events.borrow_mut().clear();
        runtime.recheck = true;
        runtime.failure = Some(Failure::Recheck);

        let error =
            install_prepared_update_with(&prepared, &mut runtime, &cancellation, &mut |_| {})
                .unwrap_err();

        assert!(matches!(error, UpdateInstallError::InitialPreflight(_)));
        assert_eq!(runtime.event_names(), ["acquire", "recheck", "drop_bundle"]);
    }

    #[test]
    fn candidate_refusal_stops_before_installer() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::CandidatePreflight);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn installer_failure_skips_post_install_verification() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Installer);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "installer",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn installed_identity_failure_is_reported_before_bundle_cleanup() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::InstalledIdentity);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "installer",
                "installed_identity",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn installed_identity_mismatch_is_rejected_before_bundle_cleanup() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.installed_identity = identity("1.5.0", "wrong-commit");

        let error = run_update_install_with(&mut Vec::new(), &mut runtime).unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::InstalledIdentityMismatch { .. }
        ));
        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "installer",
                "installed_identity",
                "drop_bundle"
            ]
        );
    }

    #[test]
    fn successful_upgrade_preserves_order_and_reports_identities() {
        let mut runtime = FakeRuntime::new("1.5.0");
        let mut output = Vec::new();

        run_update_install_with(&mut output, &mut runtime).unwrap();

        assert_eq!(
            runtime.event_names(),
            [
                "current",
                "initial_preflight",
                "discover",
                "resolve",
                "confirm",
                "acquire",
                "candidate_preflight",
                "installer",
                "installed_identity",
                "drop_bundle"
            ]
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Current: 1.4.0 (stable, commit current-commit)"));
        assert!(output.contains("Target: 1.5.0 (stable, commit target-commit)"));
        assert!(output.contains("Installed: 1.5.0 (stable, commit target-commit)"));
    }

    #[test]
    fn prerelease_selection_can_install_a_newer_stable_release() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.saved_channel = UpdateChannel::Prerelease;
        let mut output = Vec::new();

        run_update_install_with(&mut output, &mut runtime).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Target: 1.5.0 (stable, commit target-commit)"));
        assert!(output.contains("Installed: 1.5.0 (stable, commit target-commit)"));
    }

    #[test]
    fn cancellation_before_acquisition_has_no_mutating_side_effects() {
        let mut runtime = FakeRuntime::new("1.5.0");
        let cancellation = UpdateInstallCancellation::new();
        let current = runtime.current_version();
        let prepared = prepare_update_install_with(&mut runtime, &cancellation, current)
            .unwrap()
            .unwrap();
        runtime.events.borrow_mut().clear();
        assert!(cancellation.cancel());

        let error =
            install_prepared_update_with(&prepared, &mut runtime, &cancellation, &mut |_| {})
                .unwrap_err();

        assert!(matches!(error, UpdateInstallError::Cancelled));
        assert!(runtime.event_names().is_empty());
    }

    #[test]
    fn cancellation_during_candidate_preflight_stops_before_installer() {
        let mut runtime = FakeRuntime::new("1.5.0");
        let cancellation = UpdateInstallCancellation::new();
        let current = runtime.current_version();
        let prepared = prepare_update_install_with(&mut runtime, &cancellation, current)
            .unwrap()
            .unwrap();
        runtime.events.borrow_mut().clear();
        let mut stages = Vec::new();

        let error =
            install_prepared_update_with(&prepared, &mut runtime, &cancellation, &mut |stage| {
                stages.push(stage);
                if stage == UpdateInstallStage::CandidatePreflight {
                    assert!(cancellation.cancel());
                }
            })
            .unwrap_err();

        assert!(matches!(error, UpdateInstallError::Cancelled));
        assert_eq!(
            stages,
            [
                UpdateInstallStage::Acquiring,
                UpdateInstallStage::CandidatePreflight,
            ]
        );
        assert_eq!(runtime.event_names(), ["acquire", "drop_bundle"]);
    }

    #[test]
    fn installer_boundary_claim_prevents_late_cancellation() {
        let mut runtime = FakeRuntime::new("1.5.0");
        let cancellation = UpdateInstallCancellation::new();
        let current = runtime.current_version();
        let prepared = prepare_update_install_with(&mut runtime, &cancellation, current)
            .unwrap()
            .unwrap();
        runtime.events.borrow_mut().clear();

        let installed =
            install_prepared_update_with(&prepared, &mut runtime, &cancellation, &mut |stage| {
                if stage == UpdateInstallStage::Installing {
                    assert!(!cancellation.cancel());
                }
            })
            .unwrap();

        assert_eq!(installed.identity(), prepared.identity());
        assert!(!cancellation.can_cancel());
        assert!(!cancellation.is_cancelled());
        assert_eq!(
            runtime.event_names(),
            [
                "acquire",
                "candidate_preflight",
                "installer",
                "installed_identity",
                "drop_bundle",
            ]
        );
    }

    #[test]
    fn changed_saved_channel_is_rejected_before_acquisition() {
        let mut runtime = FakeRuntime::new("1.5.0");
        let cancellation = UpdateInstallCancellation::new();
        let current = runtime.current_version();
        let prepared = prepare_update_install_with(&mut runtime, &cancellation, current)
            .unwrap()
            .unwrap();
        runtime.events.borrow_mut().clear();
        runtime.saved_channel = UpdateChannel::Prerelease;

        let error =
            install_prepared_update_with(&prepared, &mut runtime, &cancellation, &mut |_| {})
                .unwrap_err();

        assert!(matches!(
            error,
            UpdateInstallError::ChannelChanged {
                prepared: UpdateChannel::Stable,
                current: UpdateChannel::Prerelease,
            }
        ));
        assert!(runtime.event_names().is_empty());
    }

    #[test]
    fn resolver_failure_stops_before_confirmation() {
        let mut runtime = FakeRuntime::new("1.5.0");
        runtime.failure = Some(Failure::Resolve);

        assert!(run_update_install_with(&mut Vec::new(), &mut runtime).is_err());
        assert_eq!(
            runtime.event_names(),
            ["current", "initial_preflight", "discover", "resolve"]
        );
    }
}
