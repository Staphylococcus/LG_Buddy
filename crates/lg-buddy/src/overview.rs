use std::error::Error;
use std::fmt;
use std::net::Ipv4Addr;

use crate::audio::{
    apply_audio_operation_with, read_audio_status_with, AudioOperation, AudioWriteError,
    AudioWriteOutcome,
};
use crate::brightness::{
    user_facing_read_error, user_facing_write_error, BrightnessReadError, BrightnessReader,
    BrightnessWriteError, BrightnessWriteOutcome, BrightnessWriter, EnvironmentBrightnessReader,
    EnvironmentBrightnessWriter,
};
use crate::config::{load_config, resolve_config_path_from_env, HdmiInput, TvPlatform};
use crate::presentation::brightness::{BrightnessPresentation, UserFacingError};
use crate::presentation::overview::{
    AudioPresentation, OverviewPresentation, TvConnectionState, TvSummaryPresentation,
};
use crate::tv::{
    build_tv_client, CurrentVolume, OledBrightness, TvClientBuildOptions, VolumeLevel,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewTvIdentity {
    address: Ipv4Addr,
    input: HdmiInput,
    platform: TvPlatform,
}

impl OverviewTvIdentity {
    pub fn new(address: Ipv4Addr, input: HdmiInput, platform: TvPlatform) -> Self {
        Self {
            address,
            input,
            platform,
        }
    }
    pub fn address(&self) -> Ipv4Addr {
        self.address
    }
    pub fn input(&self) -> HdmiInput {
        self.input
    }
    pub fn platform(&self) -> TvPlatform {
        self.platform
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewIntent {
    SetBrightness(u8),
    RetryBrightness,
    SetVolume(u8),
    SetMuted(bool),
    RetryAudio,
    RetrySummary,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewSummaryOperation(u64);
pub use crate::brightness::{
    BrightnessReadOperation as OverviewBrightnessReadOperation,
    BrightnessWriteOperation as OverviewBrightnessWriteOperation,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewAudioReadOperation(u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverviewAudioWriteOperation {
    id: u64,
    operation: AudioOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PendingAudio {
    volume: Option<VolumeLevel>,
    muted: Option<bool>,
}

impl OverviewAudioWriteOperation {
    pub fn operation(self) -> AudioOperation {
        self.operation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewOperation {
    ReadSummary(OverviewSummaryOperation),
    ReadBrightness(OverviewBrightnessReadOperation),
    ReadAudio(OverviewAudioReadOperation),
    WriteBrightness(OverviewBrightnessWriteOperation),
    WriteAudio(OverviewAudioWriteOperation),
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverviewFrontendUpdate {
    Present(OverviewPresentation),
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewTransition {
    update: OverviewFrontendUpdate,
    operations: Vec<OverviewOperation>,
    diagnostic: Option<String>,
}

impl OverviewTransition {
    pub fn update(&self) -> &OverviewFrontendUpdate {
        &self.update
    }
    pub fn operations(&self) -> &[OverviewOperation] {
        &self.operations
    }
    pub fn operation(&self) -> Option<OverviewOperation> {
        self.operations.first().copied()
    }
    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }
}

pub trait OverviewBackend: Send + Sync + 'static {
    fn read_summary(&self) -> Result<OverviewTvIdentity, OverviewSummaryError>;
    fn read_brightness(&self) -> Result<OledBrightness, BrightnessReadError>;
    fn read_audio(&self) -> Result<crate::tv::AudioStatus, AudioReadError>;
    fn write_brightness(
        &self,
        brightness: OledBrightness,
    ) -> Result<BrightnessWriteOutcome, BrightnessWriteError>;
    fn write_audio(&self, operation: AudioOperation) -> Result<AudioWriteOutcome, AudioWriteError>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentOverviewBackend;

impl OverviewBackend for EnvironmentOverviewBackend {
    fn read_summary(&self) -> Result<OverviewTvIdentity, OverviewSummaryError> {
        let path = resolve_config_path_from_env().map_err(|error| {
            OverviewSummaryError::new(OverviewSummaryFailure::NotConfigured, error.to_string())
        })?;
        let config = load_config(&path).map_err(|error| {
            OverviewSummaryError::new(
                OverviewSummaryFailure::InvalidConfiguration,
                error.to_string(),
            )
        })?;
        Ok(OverviewTvIdentity::new(
            config.tv_ip,
            config.input,
            config.tv_platform,
        ))
    }

    fn read_brightness(&self) -> Result<OledBrightness, BrightnessReadError> {
        EnvironmentBrightnessReader.read_current_brightness()
    }

    fn read_audio(&self) -> Result<crate::tv::AudioStatus, AudioReadError> {
        let (config, client) = environment_client().map_err(AudioReadError::from)?;
        read_audio_status_with(&config, &client).map_err(AudioReadError::from)
    }

    fn write_brightness(
        &self,
        brightness: OledBrightness,
    ) -> Result<BrightnessWriteOutcome, BrightnessWriteError> {
        EnvironmentBrightnessWriter.write_brightness(brightness)
    }

    fn write_audio(&self, operation: AudioOperation) -> Result<AudioWriteOutcome, AudioWriteError> {
        let (config, client) = environment_client().map_err(AudioWriteError::from)?;
        apply_audio_operation_with(&config, &client, operation)
    }
}

fn environment_client(
) -> Result<(crate::config::Config, crate::tv::SelectedTvClient), EnvironmentClientError> {
    let path = resolve_config_path_from_env()
        .map_err(|error| EnvironmentClientError::NotConfigured(error.to_string()))?;
    let config =
        load_config(&path).map_err(|error| EnvironmentClientError::Config(error.to_string()))?;
    let client = build_tv_client(
        &path,
        config.tv_ip,
        config.tv_platform,
        TvClientBuildOptions::production().stored_token_only(),
    )
    .map_err(|error| EnvironmentClientError::Client(error.to_string()))?;
    Ok((config, client))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EnvironmentClientError {
    NotConfigured(String),
    Config(String),
    Client(String),
}
impl fmt::Display for EnvironmentClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured(s) | Self::Config(s) | Self::Client(s) => f.write_str(s),
        }
    }
}
impl Error for EnvironmentClientError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewSummaryFailure {
    NotConfigured,
    InvalidConfiguration,
    Internal,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewSummaryError {
    failure: OverviewSummaryFailure,
    diagnostic: String,
}
impl OverviewSummaryError {
    pub fn new(failure: OverviewSummaryFailure, diagnostic: impl Into<String>) -> Self {
        Self {
            failure,
            diagnostic: diagnostic.into(),
        }
    }
    pub fn failure(&self) -> OverviewSummaryFailure {
        self.failure
    }
}
impl fmt::Display for OverviewSummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic)
    }
}
impl Error for OverviewSummaryError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioReadFailure {
    NotConfigured,
    InvalidConfiguration,
    CredentialsUnavailable,
    Unreachable,
    Rejected,
    InvalidResponse,
    ScreenNotVisible,
    Internal,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioReadError {
    failure: AudioReadFailure,
    diagnostic: String,
}
impl AudioReadError {
    pub fn new(failure: AudioReadFailure, diagnostic: impl Into<String>) -> Self {
        Self {
            failure,
            diagnostic: diagnostic.into(),
        }
    }
    pub fn failure(&self) -> AudioReadFailure {
        self.failure
    }
}
impl fmt::Display for AudioReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic)
    }
}
impl Error for AudioReadError {}
impl From<EnvironmentClientError> for AudioReadError {
    fn from(error: EnvironmentClientError) -> Self {
        Self::new(
            match error {
                EnvironmentClientError::NotConfigured(_) => AudioReadFailure::NotConfigured,
                EnvironmentClientError::Config(_) => AudioReadFailure::InvalidConfiguration,
                EnvironmentClientError::Client(_) => AudioReadFailure::CredentialsUnavailable,
            },
            error.to_string(),
        )
    }
}
impl From<crate::tv::TvError> for AudioReadError {
    fn from(error: crate::tv::TvError) -> Self {
        Self::new(
            match error.kind() {
                crate::tv::TvErrorKind::Transport => AudioReadFailure::Unreachable,
                crate::tv::TvErrorKind::Authentication => AudioReadFailure::CredentialsUnavailable,
                crate::tv::TvErrorKind::Rejected => AudioReadFailure::Rejected,
                crate::tv::TvErrorKind::InvalidResponse => AudioReadFailure::InvalidResponse,
                crate::tv::TvErrorKind::ScreenNotVisible => AudioReadFailure::ScreenNotVisible,
                crate::tv::TvErrorKind::Internal => AudioReadFailure::Internal,
            },
            error.to_string(),
        )
    }
}

impl From<EnvironmentClientError> for AudioWriteError {
    fn from(error: EnvironmentClientError) -> Self {
        AudioWriteError::new(
            match error {
                EnvironmentClientError::NotConfigured(_) => {
                    crate::audio::AudioWriteFailure::NotConfigured
                }
                EnvironmentClientError::Config(_) => {
                    crate::audio::AudioWriteFailure::InvalidConfiguration
                }
                EnvironmentClientError::Client(_) => {
                    crate::audio::AudioWriteFailure::CredentialsUnavailable
                }
            },
            error.to_string(),
            None,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SummaryState {
    Loading(OverviewSummaryOperation),
    Ready(OverviewTvIdentity),
    Failed(UserFacingError),
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BrightnessState {
    Loading(OverviewBrightnessReadOperation),
    Ready {
        current: OledBrightness,
        proposed: OledBrightness,
    },
    Applying {
        current: OledBrightness,
        proposed: OledBrightness,
        pending: Option<OledBrightness>,
        operation: OverviewBrightnessWriteOperation,
    },
    Failed {
        current: OledBrightness,
        proposed: OledBrightness,
        failed: OledBrightness,
        pending: Option<OledBrightness>,
        error: UserFacingError,
    },
    ReadFailed(UserFacingError),
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AudioState {
    Loading(OverviewAudioReadOperation),
    Ready {
        current: CurrentVolume,
        proposed: Option<VolumeLevel>,
        muted: bool,
    },
    ReadFailed(UserFacingError),
    Applying {
        current: CurrentVolume,
        proposed: Option<VolumeLevel>,
        muted: bool,
        pending: PendingAudio,
        operation: OverviewAudioWriteOperation,
    },
    Failed {
        current: CurrentVolume,
        proposed: Option<VolumeLevel>,
        muted: bool,
        pending: PendingAudio,
        error: UserFacingError,
        retry: AudioOperation,
    },
    Closed,
}

#[derive(Debug)]
pub struct OverviewApplication {
    summary: SummaryState,
    brightness: BrightnessState,
    brightness_presentation: BrightnessPresentation,
    audio: AudioState,
    next_id: u64,
}

impl OverviewApplication {
    pub fn open() -> (Self, OverviewTransition) {
        let brightness_read = OverviewBrightnessReadOperation::new(0);
        let app = Self {
            summary: SummaryState::Loading(OverviewSummaryOperation(0)),
            brightness: BrightnessState::Loading(brightness_read),
            brightness_presentation: BrightnessPresentation::loading(),
            audio: AudioState::Loading(OverviewAudioReadOperation(2)),
            next_id: 3,
        };
        let transition = app.transition(
            vec![
                OverviewOperation::ReadSummary(OverviewSummaryOperation(0)),
                OverviewOperation::ReadBrightness(brightness_read),
                OverviewOperation::ReadAudio(OverviewAudioReadOperation(2)),
            ],
            None,
        );
        (app, transition)
    }

    pub fn handle_intent(&mut self, intent: OverviewIntent) -> Option<OverviewTransition> {
        match intent {
            OverviewIntent::Cancel => {
                if self.is_closed() {
                    None
                } else {
                    self.shutdown();
                    Some(self.transition(Vec::new(), None).close())
                }
            }
            OverviewIntent::RetrySummary => match self.summary {
                SummaryState::Failed(_) => {
                    let op = self.new_summary();
                    Some(self.transition(vec![OverviewOperation::ReadSummary(op)], None))
                }
                _ => None,
            },
            OverviewIntent::SetBrightness(value) => self.set_brightness(value),
            OverviewIntent::RetryBrightness => self.retry_brightness(),
            OverviewIntent::SetVolume(value) => self.set_volume(value),
            OverviewIntent::SetMuted(muted) => self.set_muted(muted),
            OverviewIntent::RetryAudio => self.retry_audio(),
        }
    }

    /// Reload after the first primary profile has been saved. Preserve the
    /// operation sequence so late results from the empty state stay stale.
    pub(crate) fn profile_created(&mut self) -> Option<OverviewTransition> {
        if self.is_closed()
            || matches!(self.brightness, BrightnessState::Applying { .. })
            || matches!(self.audio, AudioState::Applying { .. })
        {
            return None;
        }
        let summary = self.new_summary();
        let brightness = OverviewBrightnessReadOperation::new(self.next_id);
        self.next_id += 1;
        self.brightness = BrightnessState::Loading(brightness);
        self.brightness_presentation = BrightnessPresentation::loading();
        let audio = self.new_audio_read();
        Some(self.transition(
            vec![
                OverviewOperation::ReadSummary(summary),
                OverviewOperation::ReadBrightness(brightness),
                OverviewOperation::ReadAudio(audio),
            ],
            None,
        ))
    }

    pub fn complete_summary(
        &mut self,
        operation: OverviewSummaryOperation,
        result: Result<OverviewTvIdentity, OverviewSummaryError>,
    ) -> Option<OverviewTransition> {
        if !matches!(self.summary, SummaryState::Loading(active) if active == operation) {
            return None;
        }
        let diagnostic = result
            .as_ref()
            .err()
            .map(|error| format!("could not load primary TV configuration: {error}"));
        match result {
            Ok(identity) => self.summary = SummaryState::Ready(identity),
            Err(error) => self.summary = SummaryState::Failed(summary_error(error.failure())),
        };
        Some(self.transition(Vec::new(), diagnostic))
    }
    pub fn complete_brightness_read(
        &mut self,
        operation: OverviewBrightnessReadOperation,
        result: Result<OledBrightness, BrightnessReadError>,
    ) -> Option<OverviewTransition> {
        if !matches!(self.brightness, BrightnessState::Loading(active) if active == operation) {
            return None;
        }
        let diagnostic = result
            .as_ref()
            .err()
            .map(|error| format!("could not load current brightness: {error}"));
        match result {
            Ok(brightness) => {
                self.brightness = BrightnessState::Ready {
                    current: brightness,
                    proposed: brightness,
                };
                self.brightness_presentation =
                    BrightnessPresentation::overview_ready(brightness, brightness);
            }
            Err(error) => {
                self.brightness =
                    BrightnessState::ReadFailed(user_facing_read_error(error.failure()));
                self.brightness_presentation =
                    BrightnessPresentation::read_failed(user_facing_read_error(error.failure()));
            }
        }
        Some(self.transition(Vec::new(), diagnostic))
    }

    pub fn complete_audio_read(
        &mut self,
        operation: OverviewAudioReadOperation,
        result: Result<crate::tv::AudioStatus, AudioReadError>,
    ) -> Option<OverviewTransition> {
        if !matches!(self.audio, AudioState::Loading(active) if active == operation) {
            return None;
        }
        let diagnostic = result
            .as_ref()
            .err()
            .map(|error| format!("could not read TV audio: {error}"));
        match result {
            Ok(status) => {
                let proposed = match status.volume() {
                    CurrentVolume::Level(level) => Some(level),
                    CurrentVolume::Unknown => None,
                };
                self.audio = AudioState::Ready {
                    current: status.volume(),
                    proposed,
                    muted: status.is_muted(),
                };
            }
            Err(error) => self.audio = AudioState::ReadFailed(audio_error(error.failure())),
        };
        Some(self.transition(Vec::new(), diagnostic))
    }
    pub fn complete_brightness_write(
        &mut self,
        operation: OverviewBrightnessWriteOperation,
        result: Result<BrightnessWriteOutcome, BrightnessWriteError>,
    ) -> Option<OverviewTransition> {
        let BrightnessState::Applying {
            current,
            proposed: _,
            pending,
            operation: active,
        } = self.brightness.clone()
        else {
            return None;
        };
        if active != operation {
            return None;
        }
        match result {
            Ok(outcome) => {
                let diagnostic = outcome.diagnostic().map(str::to_string);
                let applied = operation.brightness();
                if let Some(next) = pending.filter(|next| *next != applied) {
                    let next_operation = self.new_brightness_write(applied, next, None);
                    return Some(self.transition(
                        vec![OverviewOperation::WriteBrightness(next_operation)],
                        diagnostic,
                    ));
                }
                self.brightness = BrightnessState::Ready {
                    current: applied,
                    proposed: applied,
                };
                self.brightness_presentation =
                    BrightnessPresentation::overview_ready(applied, applied);
                Some(self.transition(Vec::new(), diagnostic))
            }
            Err(error) => {
                let user_error = user_facing_write_error(error.failure());
                let shown = pending.unwrap_or(current);
                self.brightness = BrightnessState::Failed {
                    current,
                    proposed: shown,
                    failed: operation.brightness(),
                    pending,
                    error: user_error.clone(),
                };
                self.brightness_presentation =
                    BrightnessPresentation::overview_write_failed(current, shown, user_error);
                Some(self.transition(
                    Vec::new(),
                    Some(format!("could not apply brightness: {error}")),
                ))
            }
        }
    }

    pub fn complete_audio_write(
        &mut self,
        operation: OverviewAudioWriteOperation,
        result: Result<AudioWriteOutcome, AudioWriteError>,
    ) -> Option<OverviewTransition> {
        let AudioState::Applying {
            current,
            proposed,
            muted,
            pending,
            operation: active,
        } = self.audio.clone()
        else {
            return None;
        };
        if active != operation {
            return None;
        }
        match result {
            Ok(AudioWriteOutcome::Applied {
                volume,
                muted: changed_mute,
            }) => {
                let current = volume.map(CurrentVolume::Level).unwrap_or(current);
                let muted = changed_mute.unwrap_or(muted);
                self.finish_audio_write(current, volume.or(proposed), muted, pending)
            }
            Err(error) => {
                let current = error
                    .volume_applied()
                    .map(CurrentVolume::Level)
                    .unwrap_or(current);
                let retry = if error.failure() == crate::audio::AudioWriteFailure::UnmuteAfterVolume
                {
                    AudioOperation::SetMuted(false)
                } else {
                    operation.operation()
                };
                let proposed = pending
                    .volume
                    .or_else(|| error.volume_applied().or_else(|| current_level(current)));
                let user = match error.failure() {
                    crate::audio::AudioWriteFailure::UnmuteAfterVolume => UserFacingError::new(
                        "Volume changed, but unmuting failed.",
                        "Retry unmuting the TV. The volume value has already been changed.",
                    ),
                    crate::audio::AudioWriteFailure::NotConfigured => audio_error(AudioReadFailure::NotConfigured),
                    crate::audio::AudioWriteFailure::InvalidConfiguration => audio_error(AudioReadFailure::InvalidConfiguration),
                    crate::audio::AudioWriteFailure::CredentialsUnavailable => audio_error(AudioReadFailure::CredentialsUnavailable),
                    _ => UserFacingError::new(
                        "The TV could not apply the audio change.",
                        "Retry the audio operation. If this continues, check that the TV is on and reachable.",
                    ),
                };
                self.audio = AudioState::Failed {
                    current,
                    proposed,
                    muted,
                    pending,
                    error: user,
                    retry,
                };
                Some(self.transition(Vec::new(), Some(format!("could not apply audio: {error}"))))
            }
        }
    }
    pub fn shutdown(&mut self) {
        self.summary = SummaryState::Closed;
        self.brightness = BrightnessState::Closed;
        self.brightness_presentation = BrightnessPresentation::loading();
        self.audio = AudioState::Closed;
    }

    fn new_summary(&mut self) -> OverviewSummaryOperation {
        let id = OverviewSummaryOperation(self.next_id);
        self.next_id += 1;
        self.summary = SummaryState::Loading(id);
        id
    }
    fn set_brightness(&mut self, value: u8) -> Option<OverviewTransition> {
        let proposed = OledBrightness::new(value).ok()?;
        match self.brightness.clone() {
            BrightnessState::Ready {
                current,
                proposed: previous,
            } => {
                if proposed == previous {
                    return None;
                }
                let operation = self.new_brightness_write(current, proposed, None);
                Some(self.transition(vec![OverviewOperation::WriteBrightness(operation)], None))
            }
            BrightnessState::Applying {
                current,
                proposed: previous,
                pending,
                operation,
            } => {
                let next_pending = if proposed == operation.brightness() {
                    None
                } else {
                    Some(proposed)
                };
                if proposed == previous && next_pending == pending {
                    return None;
                }
                self.brightness = BrightnessState::Applying {
                    current,
                    proposed,
                    pending: next_pending,
                    operation,
                };
                self.brightness_presentation =
                    BrightnessPresentation::overview_applying(current, proposed);
                Some(self.transition(Vec::new(), None))
            }
            BrightnessState::Failed { current, .. } => {
                if proposed == current {
                    self.brightness = BrightnessState::Ready { current, proposed };
                    self.brightness_presentation =
                        BrightnessPresentation::overview_ready(current, proposed);
                    Some(self.transition(Vec::new(), None))
                } else {
                    let operation = self.new_brightness_write(current, proposed, None);
                    Some(self.transition(vec![OverviewOperation::WriteBrightness(operation)], None))
                }
            }
            _ => None,
        }
    }

    fn retry_brightness(&mut self) -> Option<OverviewTransition> {
        match self.brightness.clone() {
            BrightnessState::ReadFailed(_) => {
                let operation = OverviewBrightnessReadOperation::new(self.next_id);
                self.next_id += 1;
                self.brightness = BrightnessState::Loading(operation);
                self.brightness_presentation = BrightnessPresentation::loading();
                Some(self.transition(vec![OverviewOperation::ReadBrightness(operation)], None))
            }
            BrightnessState::Failed {
                current,
                failed,
                pending,
                ..
            } => {
                let operation = self.new_brightness_write(current, failed, pending);
                Some(self.transition(vec![OverviewOperation::WriteBrightness(operation)], None))
            }
            _ => None,
        }
    }

    fn new_brightness_write(
        &mut self,
        current: OledBrightness,
        proposed: OledBrightness,
        pending: Option<OledBrightness>,
    ) -> OverviewBrightnessWriteOperation {
        let operation = OverviewBrightnessWriteOperation::new(self.next_id, proposed);
        self.next_id += 1;
        self.brightness = BrightnessState::Applying {
            current,
            proposed: pending.unwrap_or(proposed),
            pending,
            operation,
        };
        self.brightness_presentation =
            BrightnessPresentation::overview_applying(current, pending.unwrap_or(proposed));
        operation
    }

    fn set_volume(&mut self, value: u8) -> Option<OverviewTransition> {
        let volume = VolumeLevel::new(value).ok()?;
        match self.audio.clone() {
            AudioState::Ready {
                current,
                proposed,
                muted,
            } => {
                if proposed == Some(volume) && !muted {
                    return None;
                }
                let operation = self.start_audio_write(
                    current,
                    Some(volume),
                    muted,
                    PendingAudio::default(),
                    AudioOperation::SetVolumeAndUnmute(volume),
                );
                Some(self.transition(vec![OverviewOperation::WriteAudio(operation)], None))
            }
            AudioState::Applying {
                current,
                proposed,
                muted,
                mut pending,
                operation,
            } => {
                let next_pending = match operation.operation() {
                    AudioOperation::SetVolumeAndUnmute(active) if active == volume => None,
                    _ => Some(volume),
                };
                if proposed == Some(volume) && pending.volume == next_pending {
                    return None;
                }
                pending.volume = next_pending;
                self.audio = AudioState::Applying {
                    current,
                    proposed: Some(volume),
                    muted,
                    pending,
                    operation,
                };
                Some(self.transition(Vec::new(), None))
            }
            AudioState::Failed {
                current,
                proposed: _,
                muted,
                pending,
                ..
            } => {
                if proposed_is_current(current, volume) && !muted && pending.muted.is_none() {
                    self.audio = AudioState::Ready {
                        current,
                        proposed: current_level(current),
                        muted,
                    };
                    return Some(self.transition(Vec::new(), None));
                }
                let operation = self.start_audio_write(
                    current,
                    Some(volume),
                    muted,
                    PendingAudio {
                        volume: None,
                        muted: pending.muted,
                    },
                    AudioOperation::SetVolumeAndUnmute(volume),
                );
                Some(self.transition(vec![OverviewOperation::WriteAudio(operation)], None))
            }
            _ => None,
        }
    }

    fn set_muted(&mut self, muted: bool) -> Option<OverviewTransition> {
        match self.audio.clone() {
            AudioState::Ready {
                current,
                proposed,
                muted: current_muted,
            } => {
                if current_muted == muted {
                    return None;
                }
                let operation = self.start_audio_write(
                    current,
                    proposed,
                    current_muted,
                    PendingAudio::default(),
                    AudioOperation::SetMuted(muted),
                );
                Some(self.transition(vec![OverviewOperation::WriteAudio(operation)], None))
            }
            AudioState::Applying {
                current,
                proposed,
                muted: current_muted,
                mut pending,
                operation,
            } => {
                let expected = pending
                    .volume
                    .map(|_| false)
                    .unwrap_or_else(|| operation_muted(operation.operation()));
                let next_pending = (muted != expected).then_some(muted);
                if pending.muted == next_pending {
                    return None;
                }
                pending.muted = next_pending;
                self.audio = AudioState::Applying {
                    current,
                    proposed,
                    muted: current_muted,
                    pending,
                    operation,
                };
                Some(self.transition(Vec::new(), None))
            }
            AudioState::Failed {
                current,
                proposed,
                muted: current_muted,
                pending,
                ..
            } => {
                if current_muted == muted && pending.muted.is_none() && pending.volume.is_none() {
                    self.audio = AudioState::Ready {
                        current,
                        proposed,
                        muted: current_muted,
                    };
                    return Some(self.transition(Vec::new(), None));
                }
                let operation = self.start_audio_write(
                    current,
                    proposed,
                    current_muted,
                    PendingAudio {
                        volume: pending.volume,
                        // The queued volume will unmute, even if mute is unchanged now.
                        muted: pending.volume.map(|_| muted),
                    },
                    AudioOperation::SetMuted(muted),
                );
                Some(self.transition(vec![OverviewOperation::WriteAudio(operation)], None))
            }
            _ => None,
        }
    }

    fn start_audio_write(
        &mut self,
        current: CurrentVolume,
        proposed: Option<VolumeLevel>,
        muted: bool,
        pending: PendingAudio,
        operation: AudioOperation,
    ) -> OverviewAudioWriteOperation {
        let write = OverviewAudioWriteOperation {
            id: self.next_id,
            operation,
        };
        self.next_id += 1;
        self.audio = AudioState::Applying {
            current,
            proposed,
            muted,
            pending,
            operation: write,
        };
        write
    }

    fn finish_audio_write(
        &mut self,
        current: CurrentVolume,
        proposed: Option<VolumeLevel>,
        muted: bool,
        mut pending: PendingAudio,
    ) -> Option<OverviewTransition> {
        if let Some(volume) = pending.volume.take() {
            // A volume write always unmutes. An explicit request to stay muted
            // remains queued for the next operation.
            if pending.muted == Some(false) {
                pending.muted = None;
            }
            let operation = self.start_audio_write(
                current,
                Some(volume),
                muted,
                pending,
                AudioOperation::SetVolumeAndUnmute(volume),
            );
            return Some(self.transition(vec![OverviewOperation::WriteAudio(operation)], None));
        }
        if let Some(requested) = pending.muted {
            if requested != muted {
                let operation = self.start_audio_write(
                    current,
                    proposed,
                    muted,
                    PendingAudio::default(),
                    AudioOperation::SetMuted(requested),
                );
                return Some(self.transition(vec![OverviewOperation::WriteAudio(operation)], None));
            }
        }
        self.audio = AudioState::Ready {
            current,
            proposed: proposed.or_else(|| current_level(current)),
            muted,
        };
        Some(self.transition(Vec::new(), None))
    }
    fn retry_audio(&mut self) -> Option<OverviewTransition> {
        match self.audio.clone() {
            AudioState::ReadFailed(_) => {
                let op = self.new_audio_read();
                Some(self.transition(vec![OverviewOperation::ReadAudio(op)], None))
            }
            AudioState::Failed {
                current,
                proposed,
                muted,
                pending,
                retry,
                ..
            } => {
                let operation = self.start_audio_write(current, proposed, muted, pending, retry);
                Some(self.transition(vec![OverviewOperation::WriteAudio(operation)], None))
            }
            _ => None,
        }
    }
    fn new_audio_read(&mut self) -> OverviewAudioReadOperation {
        let id = OverviewAudioReadOperation(self.next_id);
        self.next_id += 1;
        self.audio = AudioState::Loading(id);
        id
    }
    fn is_closed(&self) -> bool {
        matches!(self.summary, SummaryState::Closed)
    }
    fn transition(
        &self,
        operations: Vec<OverviewOperation>,
        diagnostic: Option<String>,
    ) -> OverviewTransition {
        OverviewTransition {
            update: OverviewFrontendUpdate::Present(self.presentation()),
            operations,
            diagnostic,
        }
    }
    fn presentation(&self) -> OverviewPresentation {
        let summary = match &self.summary {
            SummaryState::Loading(_) => TvSummaryPresentation::loading(),
            SummaryState::Ready(identity) => {
                TvSummaryPresentation::ready(identity.clone(), self.connection_state())
            }
            SummaryState::Failed(error) => TvSummaryPresentation::failed(error.clone()),
            SummaryState::Closed => TvSummaryPresentation::loading(),
        };
        let audio = match &self.audio {
            AudioState::Loading(_) => AudioPresentation::loading(),
            AudioState::Ready {
                current,
                proposed,
                muted,
            } => AudioPresentation::ready(*current, *proposed, *muted),
            AudioState::ReadFailed(error) => AudioPresentation::failed(
                None,
                None,
                None,
                None,
                error.clone(),
                OverviewIntent::RetryAudio,
            ),
            AudioState::Applying {
                current,
                proposed,
                muted,
                pending,
                operation,
            } => AudioPresentation::applying(
                *current,
                *proposed,
                *muted,
                operation.operation(),
                requested_mute(*pending, operation.operation()),
            ),
            AudioState::Failed {
                current,
                proposed,
                muted,
                pending,
                error,
                ..
            } => AudioPresentation::failed(
                Some(*current),
                *proposed,
                Some(*muted),
                Some(requested_mute(*pending, AudioOperation::SetMuted(*muted))),
                error.clone(),
                OverviewIntent::RetryAudio,
            ),
            AudioState::Closed => AudioPresentation::loading(),
        };
        OverviewPresentation::new(summary, self.brightness_presentation.clone(), audio)
    }
    fn connection_state(&self) -> TvConnectionState {
        if matches!(self.brightness, BrightnessState::Loading(_))
            || matches!(self.audio, AudioState::Loading(_))
        {
            TvConnectionState::Connecting
        } else if matches!(
            self.brightness,
            BrightnessState::Ready { .. } | BrightnessState::Applying { .. }
        ) || matches!(
            self.audio,
            AudioState::Ready { .. } | AudioState::Applying { .. }
        ) {
            TvConnectionState::Connected
        } else {
            TvConnectionState::Disconnected
        }
    }
}

impl OverviewTransition {
    fn close(mut self) -> Self {
        self.update = OverviewFrontendUpdate::Close;
        self
    }
}

fn summary_error(failure: OverviewSummaryFailure) -> UserFacingError {
    match failure {
        OverviewSummaryFailure::NotConfigured => UserFacingError::new(
            "LG Buddy is not configured.",
            "Complete LG Buddy setup, then retry.",
        ),
        OverviewSummaryFailure::InvalidConfiguration => UserFacingError::new(
            "LG Buddy could not load its TV configuration.",
            "Check the saved TV address and platform settings, then retry.",
        ),
        OverviewSummaryFailure::Internal => UserFacingError::new(
            "LG Buddy could not load the primary TV.",
            "Retry. If this continues, check the LG Buddy logs.",
        ),
    }
}
fn audio_error(failure: AudioReadFailure) -> UserFacingError {
    let (s, d) = match failure {
        AudioReadFailure::NotConfigured | AudioReadFailure::InvalidConfiguration => (
            "LG Buddy could not load its TV configuration.",
            "Check the saved TV settings, then retry.",
        ),
        AudioReadFailure::CredentialsUnavailable => (
            "LG Buddy cannot authenticate with this TV.",
            "Use a headless TV command to pair the TV, then retry.",
        ),
        AudioReadFailure::Unreachable => (
            "The TV could not be reached for audio.",
            "Make sure the TV is on and connected to the same network, then retry.",
        ),
        AudioReadFailure::Rejected => (
            "The TV rejected the audio request.",
            "Retry after turning the TV screen on.",
        ),
        AudioReadFailure::InvalidResponse => (
            "The TV returned an invalid audio value.",
            "Retry and check the configured TV platform if this continues.",
        ),
        AudioReadFailure::ScreenNotVisible | AudioReadFailure::Internal => (
            "LG Buddy could not read the TV audio state.",
            "Retry. If this continues, check the LG Buddy logs.",
        ),
    };
    UserFacingError::new(s, d)
}

fn current_level(current: CurrentVolume) -> Option<VolumeLevel> {
    match current {
        CurrentVolume::Level(level) => Some(level),
        CurrentVolume::Unknown => None,
    }
}

fn proposed_is_current(current: CurrentVolume, proposed: VolumeLevel) -> bool {
    current_level(current) == Some(proposed)
}

fn operation_muted(operation: AudioOperation) -> bool {
    match operation {
        AudioOperation::SetVolumeAndUnmute(_) => false,
        AudioOperation::SetMuted(muted) => muted,
    }
}

fn requested_mute(pending: PendingAudio, operation: AudioOperation) -> bool {
    pending
        .muted
        .or_else(|| pending.volume.map(|_| false))
        .unwrap_or_else(|| operation_muted(operation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioWriteFailure;
    use crate::presentation::brightness::BrightnessStatus;
    use crate::presentation::overview::AudioStatus;

    #[test]
    fn first_profile_reload_rejects_results_from_the_empty_state() {
        let (mut app, original) = OverviewApplication::open();
        let refreshed = app.profile_created().unwrap();
        assert_eq!(refreshed.operations().len(), 3);
        for operation in original.operations() {
            assert!(!refreshed.operations().contains(operation));
            let stale = match *operation {
                OverviewOperation::ReadSummary(op) => app.complete_summary(
                    op,
                    Err(OverviewSummaryError::new(
                        OverviewSummaryFailure::NotConfigured,
                        "old config",
                    )),
                ),
                OverviewOperation::ReadBrightness(op) => {
                    app.complete_brightness_read(op, Ok(OledBrightness::new(30).unwrap()))
                }
                OverviewOperation::ReadAudio(op) => app.complete_audio_read(
                    op,
                    Ok(crate::tv::AudioStatus::new(
                        CurrentVolume::Level(VolumeLevel::new(20).unwrap()),
                        false,
                    )),
                ),
                _ => unreachable!(),
            };
            assert!(stale.is_none());
        }
    }

    fn ready_application() -> OverviewApplication {
        let (mut app, opening) = OverviewApplication::open();
        for operation in opening.operations() {
            match *operation {
                OverviewOperation::ReadSummary(op) => {
                    app.complete_summary(
                        op,
                        Ok(OverviewTvIdentity::new(
                            Ipv4Addr::LOCALHOST,
                            HdmiInput::Hdmi2,
                            TvPlatform::Bscpylgtv,
                        )),
                    );
                }
                OverviewOperation::ReadBrightness(op) => {
                    app.complete_brightness_read(op, Ok(OledBrightness::new(50).unwrap()));
                }
                OverviewOperation::ReadAudio(op) => {
                    app.complete_audio_read(
                        op,
                        Ok(crate::tv::AudioStatus::new(
                            CurrentVolume::Level(VolumeLevel::new(20).unwrap()),
                            true,
                        )),
                    );
                }
                _ => panic!("opening must only read"),
            }
        }
        app
    }

    fn present(transition: &OverviewTransition) -> &OverviewPresentation {
        let OverviewFrontendUpdate::Present(presentation) = transition.update() else {
            panic!("expected open Overview")
        };
        presentation
    }

    #[test]
    fn cancellation_and_shutdown_reject_all_outstanding_reads_and_new_intents() {
        for shutdown in [false, true] {
            let (mut app, opening) = OverviewApplication::open();
            if shutdown {
                app.shutdown();
            } else {
                app.handle_intent(OverviewIntent::Cancel).unwrap();
            }
            for operation in opening.operations() {
                let completion = match *operation {
                    OverviewOperation::ReadSummary(op) => app.complete_summary(
                        op,
                        Ok(OverviewTvIdentity::new(
                            Ipv4Addr::LOCALHOST,
                            HdmiInput::Hdmi2,
                            TvPlatform::Bscpylgtv,
                        )),
                    ),
                    OverviewOperation::ReadBrightness(op) => {
                        app.complete_brightness_read(op, Ok(OledBrightness::new(50).unwrap()))
                    }
                    OverviewOperation::ReadAudio(op) => app.complete_audio_read(
                        op,
                        Ok(crate::tv::AudioStatus::new(CurrentVolume::Unknown, true)),
                    ),
                    _ => panic!("opening must only read"),
                };
                assert!(completion.is_none());
            }
            for intent in [
                OverviewIntent::RetrySummary,
                OverviewIntent::RetryAudio,
                OverviewIntent::RetryBrightness,
                OverviewIntent::SetVolume(40),
                OverviewIntent::SetMuted(false),
                OverviewIntent::SetBrightness(40),
                OverviewIntent::Cancel,
            ] {
                assert!(app.handle_intent(intent).is_none());
            }
        }
    }

    #[test]
    fn independent_writes_reject_duplicates_and_completions_after_close() {
        for shutdown in [false, true] {
            let mut app = ready_application();
            let brightness = app
                .handle_intent(OverviewIntent::SetBrightness(60))
                .unwrap();
            let OverviewOperation::WriteBrightness(brightness_op) = brightness.operations()[0]
            else {
                panic!("brightness write")
            };
            let audio = app.handle_intent(OverviewIntent::SetVolume(30)).unwrap();
            let OverviewOperation::WriteAudio(audio_op) = audio.operations()[0] else {
                panic!("audio write")
            };
            assert!(matches!(
                present(&audio).brightness().status(),
                BrightnessStatus::Applying { .. }
            ));
            assert!(matches!(
                present(&audio).audio().status(),
                AudioStatus::Applying { .. }
            ));
            assert!(app.handle_intent(OverviewIntent::SetMuted(false)).is_none());
            if shutdown {
                app.shutdown();
            } else {
                app.handle_intent(OverviewIntent::Cancel).unwrap();
            }
            assert!(app
                .complete_brightness_write(brightness_op, Ok(BrightnessWriteOutcome::Applied))
                .is_none());
            assert!(app
                .complete_audio_write(
                    audio_op,
                    Ok(AudioWriteOutcome::Applied {
                        volume: Some(VolumeLevel::new(30).unwrap()),
                        muted: Some(false)
                    })
                )
                .is_none());
        }
    }

    #[test]
    fn volume_and_mute_changes_stay_usable_and_preserve_mute_choice() {
        let mut app = ready_application();
        let applying = app.handle_intent(OverviewIntent::SetVolume(45)).unwrap();
        let volume = present(&applying).audio().volume().unwrap();
        assert_eq!(volume.proposed().as_percent(), 45);
        assert!(volume.enabled());
        let queued = app.handle_intent(OverviewIntent::SetMuted(true)).unwrap();
        let mute = present(&queued).audio().mute().unwrap();
        assert!(mute.current());
        assert!(mute.proposed());
        assert!(mute.enabled());
        assert!(present(&applying).brightness().control().unwrap().enabled());
        let OverviewOperation::WriteAudio(op) = applying.operations()[0] else {
            panic!("volume operation")
        };
        let queued_mute = app
            .complete_audio_write(
                op,
                Ok(AudioWriteOutcome::Applied {
                    volume: Some(VolumeLevel::new(45).unwrap()),
                    muted: Some(false),
                }),
            )
            .unwrap();
        let OverviewOperation::WriteAudio(mute_op) = queued_mute.operations()[0] else {
            panic!("queued mute operation")
        };
        assert_eq!(mute_op.operation(), AudioOperation::SetMuted(true));
        let done = app
            .complete_audio_write(
                mute_op,
                Ok(AudioWriteOutcome::Applied {
                    volume: None,
                    muted: Some(true),
                }),
            )
            .unwrap();
        assert_eq!(
            present(&done)
                .audio()
                .volume()
                .unwrap()
                .proposed()
                .as_percent(),
            45
        );
        assert!(present(&done).audio().mute().unwrap().current());
    }

    #[test]
    fn brightness_slider_coalesces_repeated_moves_while_write_is_pending() {
        let mut app = ready_application();
        let first = app
            .handle_intent(OverviewIntent::SetBrightness(60))
            .unwrap();
        let OverviewOperation::WriteBrightness(first_op) = first.operations()[0] else {
            panic!("first brightness operation")
        };
        app.handle_intent(OverviewIntent::SetBrightness(70));
        let latest = app
            .handle_intent(OverviewIntent::SetBrightness(80))
            .unwrap();
        assert!(latest.operations().is_empty());
        assert_eq!(
            present(&latest)
                .brightness()
                .control()
                .unwrap()
                .proposed()
                .as_percent(),
            80
        );
        let second = app
            .complete_brightness_write(first_op, Ok(BrightnessWriteOutcome::Applied))
            .unwrap();
        let OverviewOperation::WriteBrightness(second_op) = second.operations()[0] else {
            panic!("coalesced brightness operation")
        };
        assert_eq!(second_op.brightness().as_percent(), 80);
        app.complete_brightness_write(second_op, Ok(BrightnessWriteOutcome::Applied))
            .unwrap();
    }

    #[test]
    fn volume_slider_coalesces_repeated_moves_to_the_latest_write() {
        let mut app = ready_application();
        let first = app.handle_intent(OverviewIntent::SetVolume(30)).unwrap();
        let OverviewOperation::WriteAudio(first_op) = first.operations()[0] else {
            panic!("first volume operation")
        };
        app.handle_intent(OverviewIntent::SetVolume(40));
        app.handle_intent(OverviewIntent::SetVolume(50));
        let second = app
            .complete_audio_write(
                first_op,
                Ok(AudioWriteOutcome::Applied {
                    volume: Some(VolumeLevel::new(30).unwrap()),
                    muted: Some(false),
                }),
            )
            .unwrap();
        let OverviewOperation::WriteAudio(second_op) = second.operations()[0] else {
            panic!("coalesced volume operation")
        };
        assert_eq!(
            second_op.operation(),
            AudioOperation::SetVolumeAndUnmute(VolumeLevel::new(50).unwrap())
        );
    }

    #[test]
    fn explicit_mute_survives_a_later_queued_volume_move() {
        let mut app = ready_application();
        let first = app.handle_intent(OverviewIntent::SetVolume(30)).unwrap();
        let OverviewOperation::WriteAudio(first_op) = first.operations()[0] else {
            panic!("first volume operation")
        };
        app.handle_intent(OverviewIntent::SetMuted(true));
        app.handle_intent(OverviewIntent::SetVolume(55));

        let second = app
            .complete_audio_write(
                first_op,
                Ok(AudioWriteOutcome::Applied {
                    volume: Some(VolumeLevel::new(30).unwrap()),
                    muted: Some(false),
                }),
            )
            .unwrap();
        let OverviewOperation::WriteAudio(second_op) = second.operations()[0] else {
            panic!("queued volume operation")
        };
        assert_eq!(
            second_op.operation(),
            AudioOperation::SetVolumeAndUnmute(VolumeLevel::new(55).unwrap())
        );
        let third = app
            .complete_audio_write(
                second_op,
                Ok(AudioWriteOutcome::Applied {
                    volume: Some(VolumeLevel::new(55).unwrap()),
                    muted: Some(false),
                }),
            )
            .unwrap();
        let OverviewOperation::WriteAudio(third_op) = third.operations()[0] else {
            panic!("queued mute operation")
        };
        assert_eq!(third_op.operation(), AudioOperation::SetMuted(true));
    }

    #[test]
    fn explicit_mute_after_failure_survives_the_queued_volume() {
        let mut app = ready_application();
        let first = app.handle_intent(OverviewIntent::SetVolume(30)).unwrap();
        let OverviewOperation::WriteAudio(first_op) = first.operations()[0] else {
            panic!("first volume operation")
        };
        app.handle_intent(OverviewIntent::SetVolume(55)).unwrap();
        app.complete_audio_write(
            first_op,
            Err(AudioWriteError::new(
                AudioWriteFailure::SetVolume,
                "planned failure".to_string(),
                None,
            )),
        )
        .unwrap();

        let mut transition = app.handle_intent(OverviewIntent::SetMuted(true)).unwrap();
        for expected in [
            AudioOperation::SetMuted(true),
            AudioOperation::SetVolumeAndUnmute(VolumeLevel::new(55).unwrap()),
            AudioOperation::SetMuted(true),
        ] {
            assert!(present(&transition).audio().mute().unwrap().proposed());
            let OverviewOperation::WriteAudio(operation) = transition.operations()[0] else {
                panic!("audio recovery operation")
            };
            assert_eq!(operation.operation(), expected);
            let outcome = match expected {
                AudioOperation::SetMuted(muted) => AudioWriteOutcome::Applied {
                    volume: None,
                    muted: Some(muted),
                },
                AudioOperation::SetVolumeAndUnmute(volume) => AudioWriteOutcome::Applied {
                    volume: Some(volume),
                    muted: Some(false),
                },
            };
            transition = app.complete_audio_write(operation, Ok(outcome)).unwrap();
        }
        assert!(transition.operations().is_empty());
        assert!(present(&transition).audio().mute().unwrap().current());
        assert_eq!(
            present(&transition).audio().volume().unwrap().current(),
            CurrentVolume::Level(VolumeLevel::new(55).unwrap())
        );
    }

    #[test]
    fn failed_brightness_rolls_back_display_but_retry_replays_failed_value() {
        let mut app = ready_application();
        let writing = app
            .handle_intent(OverviewIntent::SetBrightness(70))
            .unwrap();
        let OverviewOperation::WriteBrightness(operation) = writing.operations()[0] else {
            panic!("brightness operation")
        };
        let failed = app
            .complete_brightness_write(
                operation,
                Err(BrightnessWriteError::new(
                    crate::brightness::BrightnessWriteFailure::Unreachable,
                    "offline".to_string(),
                )),
            )
            .unwrap();
        let control = present(&failed).brightness().control().unwrap();
        assert_eq!(control.current().as_percent(), 50);
        assert_eq!(control.proposed().as_percent(), 50);
        let retry = app.handle_intent(OverviewIntent::RetryBrightness).unwrap();
        let OverviewOperation::WriteBrightness(retry_operation) = retry.operations()[0] else {
            panic!("brightness retry")
        };
        assert_eq!(retry_operation.brightness().as_percent(), 70);
    }

    #[test]
    fn failed_volume_retry_reconciles_current_and_proposed_values() {
        let mut app = ready_application();
        let writing = app.handle_intent(OverviewIntent::SetVolume(30)).unwrap();
        let OverviewOperation::WriteAudio(operation) = writing.operations()[0] else {
            panic!("volume operation")
        };
        app.complete_audio_write(
            operation,
            Err(AudioWriteError::new(
                AudioWriteFailure::SetVolume,
                "offline".to_string(),
                None,
            )),
        )
        .unwrap();
        let retry = app.handle_intent(OverviewIntent::RetryAudio).unwrap();
        let OverviewOperation::WriteAudio(retry_operation) = retry.operations()[0] else {
            panic!("volume retry")
        };
        assert_eq!(
            retry_operation.operation(),
            AudioOperation::SetVolumeAndUnmute(VolumeLevel::new(30).unwrap())
        );
        let done = app
            .complete_audio_write(
                retry_operation,
                Ok(AudioWriteOutcome::Applied {
                    volume: Some(VolumeLevel::new(30).unwrap()),
                    muted: Some(false),
                }),
            )
            .unwrap();
        let volume = present(&done).audio().volume().unwrap();
        assert_eq!(
            volume.current(),
            CurrentVolume::Level(VolumeLevel::new(30).unwrap())
        );
        assert_eq!(volume.proposed(), VolumeLevel::new(30).unwrap());
    }

    #[test]
    fn write_failure_keeps_other_controls_usable_and_updates_health() {
        let mut app = ready_application();
        let writing = app.handle_intent(OverviewIntent::SetMuted(false)).unwrap();
        let OverviewOperation::WriteAudio(op) = writing.operations()[0] else {
            panic!("mute operation")
        };
        let failed = app
            .complete_audio_write(
                op,
                Err(AudioWriteError::new(
                    AudioWriteFailure::CredentialsUnavailable,
                    "private transport diagnostic".to_string(),
                    None,
                )),
            )
            .unwrap();
        assert_eq!(
            present(&failed).summary().connection_state(),
            TvConnectionState::Connected
        );
        assert!(present(&failed).brightness().control().unwrap().enabled());
        let AudioStatus::Failed(error) = present(&failed).audio().status() else {
            panic!("failed audio")
        };
        assert!(error.detail().contains("pair"));
        assert!(!error.detail().contains("private transport diagnostic"));
        assert!(failed
            .diagnostic()
            .unwrap()
            .contains("private transport diagnostic"));
        assert!(app
            .handle_intent(OverviewIntent::SetBrightness(40))
            .is_some());
    }

    #[test]
    fn opening_starts_three_independent_reads() {
        let (_, transition) = OverviewApplication::open();
        assert_eq!(transition.operations().len(), 3);
    }
    #[test]
    fn stale_read_after_cancel_is_ignored() {
        let (mut app, transition) = OverviewApplication::open();
        let op = match transition.operations()[0] {
            OverviewOperation::ReadSummary(op) => op,
            _ => unreachable!(),
        };
        app.handle_intent(OverviewIntent::Cancel);
        assert!(app
            .complete_summary(
                op,
                Ok(OverviewTvIdentity::new(
                    Ipv4Addr::LOCALHOST,
                    HdmiInput::Hdmi2,
                    TvPlatform::Bscpylgtv
                ))
            )
            .is_none());
    }
    #[test]
    fn unknown_volume_keeps_mute_capability() {
        let (mut app, transition) = OverviewApplication::open();
        let op = match transition.operations()[2] {
            OverviewOperation::ReadAudio(op) => op,
            _ => unreachable!(),
        };
        let status = crate::tv::AudioStatus::new(CurrentVolume::Unknown, true);
        let t = app.complete_audio_read(op, Ok(status)).unwrap();
        assert!(
            matches!(t.update(), OverviewFrontendUpdate::Present(p) if p.audio().volume().is_none() && p.audio().mute().is_some())
        );
    }
    #[test]
    fn audio_read_failure_retries_as_a_new_audio_read() {
        let (mut app, opening) = OverviewApplication::open();
        let read = match opening.operations()[2] {
            OverviewOperation::ReadAudio(operation) => operation,
            _ => unreachable!(),
        };
        app.complete_audio_read(
            read,
            Err(AudioReadError::new(
                AudioReadFailure::Unreachable,
                "offline".to_string(),
            )),
        );
        let retry = app
            .handle_intent(OverviewIntent::RetryAudio)
            .expect("audio retry");
        let next = match retry.operations()[0] {
            OverviewOperation::ReadAudio(operation) => operation,
            _ => unreachable!(),
        };
        assert_ne!(read, next);
        assert!(app
            .complete_audio_read(
                read,
                Ok(crate::tv::AudioStatus::new(CurrentVolume::Unknown, false))
            )
            .is_none());
    }
    #[test]
    fn invalid_values_start_no_operation() {
        let (mut app, opening) = OverviewApplication::open();
        let brightness_read = match opening.operations()[1] {
            OverviewOperation::ReadBrightness(operation) => operation,
            _ => unreachable!(),
        };
        app.complete_brightness_read(brightness_read, Ok(OledBrightness::new(50).unwrap()));
        assert!(app
            .handle_intent(OverviewIntent::SetBrightness(101))
            .is_none());

        let audio_read = match opening.operations()[2] {
            OverviewOperation::ReadAudio(operation) => operation,
            _ => unreachable!(),
        };
        app.complete_audio_read(
            audio_read,
            Ok(crate::tv::AudioStatus::new(
                CurrentVolume::Level(VolumeLevel::new(50).unwrap()),
                false,
            )),
        );
        assert!(app.handle_intent(OverviewIntent::SetVolume(101)).is_none());
    }
    #[test]
    fn brightness_success_stays_open_and_audio_can_start_while_it_is_busy() {
        let (mut app, opening) = OverviewApplication::open();
        let brightness_read = match opening.operations()[1] {
            OverviewOperation::ReadBrightness(operation) => operation,
            _ => unreachable!(),
        };
        app.complete_brightness_read(brightness_read, Ok(OledBrightness::new(50).unwrap()));
        let writing = app
            .handle_intent(OverviewIntent::SetBrightness(60))
            .expect("brightness write");
        let brightness_write = match writing.operations()[0] {
            OverviewOperation::WriteBrightness(operation) => operation,
            _ => unreachable!(),
        };
        let audio_read = match opening.operations()[2] {
            OverviewOperation::ReadAudio(operation) => operation,
            _ => unreachable!(),
        };
        let audio_ready = app
            .complete_audio_read(
                audio_read,
                Ok(crate::tv::AudioStatus::new(
                    CurrentVolume::Level(VolumeLevel::new(20).unwrap()),
                    false,
                )),
            )
            .expect("audio completion while brightness is busy");
        assert!(matches!(
            audio_ready.update(),
            OverviewFrontendUpdate::Present(p)
                if matches!(p.brightness().status(), BrightnessStatus::Applying { .. })
        ));
        let done = app
            .complete_brightness_write(brightness_write, Ok(BrightnessWriteOutcome::Applied))
            .expect("brightness completion");
        assert!(matches!(done.update(), OverviewFrontendUpdate::Present(_)));
    }
    #[test]
    fn partial_unmute_failure_is_actionable_and_stale_completion_ignored() {
        let (mut app, transition) = OverviewApplication::open();
        let op = match transition.operations()[2] {
            OverviewOperation::ReadAudio(op) => op,
            _ => unreachable!(),
        };
        let status =
            crate::tv::AudioStatus::new(CurrentVolume::Level(VolumeLevel::new(20).unwrap()), true);
        app.complete_audio_read(op, Ok(status));
        let t = app.handle_intent(OverviewIntent::SetVolume(30)).unwrap();
        let write = match t.operations()[0] {
            OverviewOperation::WriteAudio(op) => op,
            _ => unreachable!(),
        };
        let e = AudioWriteError::new(
            AudioWriteFailure::UnmuteAfterVolume,
            "failed".to_string(),
            VolumeLevel::new(30).ok(),
        );
        let t = app.complete_audio_write(write, Err(e)).unwrap();
        assert!(
            matches!(t.update(), OverviewFrontendUpdate::Present(p) if matches!(p.audio().status(), AudioStatus::Failed(_)))
        );
        assert!(app
            .complete_audio_write(
                write,
                Ok(AudioWriteOutcome::Applied {
                    volume: Some(VolumeLevel::new(30).unwrap()),
                    muted: Some(false)
                })
            )
            .is_none());
        let retry = app
            .handle_intent(OverviewIntent::RetryAudio)
            .expect("retry unmute only");
        let OverviewOperation::WriteAudio(retry_op) = retry.operations()[0] else {
            panic!("unmute operation")
        };
        assert_eq!(retry_op.operation(), AudioOperation::SetMuted(false));
        let finished = app
            .complete_audio_write(
                retry_op,
                Ok(AudioWriteOutcome::Applied {
                    volume: None,
                    muted: Some(false),
                }),
            )
            .unwrap();
        assert_eq!(
            present(&finished).audio().volume().unwrap().current(),
            CurrentVolume::Level(VolumeLevel::new(30).unwrap())
        );
        assert!(!present(&finished).audio().mute().unwrap().current());
    }
}
