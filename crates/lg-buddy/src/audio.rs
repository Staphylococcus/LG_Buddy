use std::error::Error;
use std::fmt;

use crate::config::Config;
use crate::tv::{AudioStatus, TvClient, TvDevice, TvError, TvErrorKind, VolumeLevel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOperation {
    SetVolumeAndUnmute(VolumeLevel),
    SetMuted(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioWriteFailure {
    NotConfigured,
    InvalidConfiguration,
    CredentialsUnavailable,
    SetVolume,
    UnmuteAfterVolume,
    SetMuted,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioWriteError {
    failure: AudioWriteFailure,
    diagnostic: String,
    volume_applied: Option<VolumeLevel>,
}

impl AudioWriteError {
    pub fn new(
        failure: AudioWriteFailure,
        diagnostic: String,
        volume_applied: Option<VolumeLevel>,
    ) -> Self {
        Self {
            failure,
            diagnostic,
            volume_applied,
        }
    }

    pub fn failure(&self) -> AudioWriteFailure {
        self.failure
    }
    pub fn volume_applied(&self) -> Option<VolumeLevel> {
        self.volume_applied
    }
}

impl fmt::Display for AudioWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.diagnostic)
    }
}

impl Error for AudioWriteError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioWriteOutcome {
    Applied {
        volume: Option<VolumeLevel>,
        muted: Option<bool>,
    },
}

pub fn read_audio_status_with<C: TvClient>(
    config: &Config,
    client: &C,
) -> Result<AudioStatus, TvError> {
    TvDevice::new(client, config.tv_ip).audio().status()
}

pub fn apply_audio_operation_with<C: TvClient>(
    config: &Config,
    client: &C,
    operation: AudioOperation,
) -> Result<AudioWriteOutcome, AudioWriteError> {
    let audio = TvDevice::new(client, config.tv_ip).audio();
    match operation {
        AudioOperation::SetVolumeAndUnmute(volume) => {
            audio.set_volume(volume).map_err(|error| {
                AudioWriteError::new(
                    if error.kind() == TvErrorKind::Authentication {
                        AudioWriteFailure::CredentialsUnavailable
                    } else {
                        AudioWriteFailure::SetVolume
                    },
                    format!("failed to set volume: {error}"),
                    None,
                )
            })?;
            audio.set_muted(false).map_err(|error| {
                AudioWriteError::new(
                    AudioWriteFailure::UnmuteAfterVolume,
                    format!("volume was changed, but unmuting failed: {error}"),
                    Some(volume),
                )
            })?;
            Ok(AudioWriteOutcome::Applied {
                volume: Some(volume),
                muted: Some(false),
            })
        }
        AudioOperation::SetMuted(muted) => audio
            .set_muted(muted)
            .map_err(|error| {
                AudioWriteError::new(
                    if error.kind() == TvErrorKind::Authentication {
                        AudioWriteFailure::CredentialsUnavailable
                    } else {
                        AudioWriteFailure::SetMuted
                    },
                    format!("failed to set mute: {error}"),
                    None,
                )
            })
            .map(|_| AudioWriteOutcome::Applied {
                volume: None,
                muted: Some(muted),
            }),
    }
}
