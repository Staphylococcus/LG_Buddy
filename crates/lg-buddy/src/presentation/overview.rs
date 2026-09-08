use crate::audio::AudioOperation;
use crate::tv::{CurrentVolume, VolumeLevel, VOLUME_MAX, VOLUME_MIN};

use super::brightness::{BrightnessIntent, BrightnessPresentation, UserFacingError};
use crate::overview::{OverviewIntent, OverviewTvIdentity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewPresentation {
    title: String,
    summary: TvSummaryPresentation,
    brightness: BrightnessPresentation,
    audio: AudioPresentation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvSummaryPresentation {
    identity: Option<OverviewTvIdentity>,
    status: TvSummaryStatus,
    connection_state: TvConnectionState,
    retry_action: Option<OverviewAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TvSummaryStatus {
    Loading { message: String },
    Ready { message: String },
    Failed(UserFacingError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvConnectionState {
    Connecting,
    Connected,
    Disconnected,
}

impl TvConnectionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::Connecting => "Connecting",
            Self::Disconnected => "Disconnected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPresentation {
    status: AudioStatus,
    volume: Option<AudioVolumeControl>,
    mute: Option<MuteControl>,
    retry_action: Option<OverviewAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioStatus {
    Loading { message: String },
    Ready { message: String },
    Applying { message: String },
    Failed(UserFacingError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioVolumeControl {
    label: String,
    current: CurrentVolume,
    proposed: VolumeLevel,
    minimum: u8,
    maximum: u8,
    step: u8,
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuteControl {
    label: String,
    current: bool,
    proposed: bool,
    enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewAction {
    label: String,
    enabled: bool,
    intent: OverviewIntent,
}

impl OverviewPresentation {
    pub(crate) fn new(
        summary: TvSummaryPresentation,
        brightness: BrightnessPresentation,
        audio: AudioPresentation,
    ) -> Self {
        Self {
            title: "LG Buddy".to_string(),
            summary,
            brightness,
            audio,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn summary(&self) -> &TvSummaryPresentation {
        &self.summary
    }
    pub fn brightness(&self) -> &BrightnessPresentation {
        &self.brightness
    }
    pub fn brightness_retry_action(&self) -> Option<OverviewAction> {
        self.brightness
            .primary_action()
            .filter(|action| action.intent() == BrightnessIntent::Retry)
            .map(|action| {
                OverviewAction::new(
                    &format!("{} {}", action.label(), self.brightness.heading()),
                    action.enabled(),
                    OverviewIntent::RetryBrightness,
                )
            })
    }
    pub fn audio(&self) -> &AudioPresentation {
        &self.audio
    }
}

impl TvSummaryPresentation {
    pub(crate) fn loading() -> Self {
        Self {
            identity: None,
            status: TvSummaryStatus::Loading {
                message: "Loading primary TV configuration…".to_string(),
            },
            connection_state: TvConnectionState::Connecting,
            retry_action: None,
        }
    }

    pub(crate) fn ready(identity: OverviewTvIdentity, connection_state: TvConnectionState) -> Self {
        Self {
            identity: Some(identity.clone()),
            status: TvSummaryStatus::Ready {
                message: format!("{} · {}", identity.address(), identity.input().as_str()),
            },
            connection_state,
            retry_action: None,
        }
    }

    pub(crate) fn failed(error: UserFacingError) -> Self {
        Self {
            identity: None,
            status: TvSummaryStatus::Failed(error),
            connection_state: TvConnectionState::Disconnected,
            retry_action: Some(OverviewAction::new(
                "Retry TV configuration",
                true,
                OverviewIntent::RetrySummary,
            )),
        }
    }

    pub fn identity(&self) -> Option<&OverviewTvIdentity> {
        self.identity.as_ref()
    }
    pub fn status(&self) -> &TvSummaryStatus {
        &self.status
    }
    pub fn connection_state(&self) -> TvConnectionState {
        self.connection_state
    }
    pub fn retry_action(&self) -> Option<&OverviewAction> {
        self.retry_action.as_ref()
    }
}

impl AudioPresentation {
    pub(crate) fn loading() -> Self {
        Self::new(
            AudioStatus::Loading {
                message: "Loading current audio state…".to_string(),
            },
            None,
            None,
        )
    }

    pub(crate) fn ready(
        current: CurrentVolume,
        proposed: Option<VolumeLevel>,
        muted: bool,
    ) -> Self {
        Self::new(
            AudioStatus::Ready {
                message: audio_message(current, muted),
            },
            proposed.map(|proposed| AudioVolumeControl::new(current, proposed, true)),
            Some(MuteControl::new(muted, true)),
        )
    }

    pub(crate) fn applying(
        current: CurrentVolume,
        proposed: Option<VolumeLevel>,
        muted: bool,
        operation: AudioOperation,
        requested_muted: bool,
    ) -> Self {
        let message = match operation {
            AudioOperation::SetVolumeAndUnmute(volume) => {
                format!("Setting volume to {volume} and unmuting…")
            }
            AudioOperation::SetMuted(true) => "Muting the TV…".to_string(),
            AudioOperation::SetMuted(false) => "Unmuting the TV…".to_string(),
        };
        let mut mute = MuteControl::new(muted, true);
        mute.proposed = requested_muted;
        Self::new(
            AudioStatus::Applying { message },
            proposed.map(|proposed| AudioVolumeControl::new(current, proposed, true)),
            Some(mute),
        )
    }

    pub(crate) fn failed(
        current: Option<CurrentVolume>,
        proposed: Option<VolumeLevel>,
        muted: Option<bool>,
        requested_muted: Option<bool>,
        error: UserFacingError,
        retry: OverviewIntent,
    ) -> Self {
        let volume = current
            .zip(proposed)
            .map(|(current, proposed)| AudioVolumeControl::new(current, proposed, true));
        let mute = muted.map(|muted| {
            let mut control = MuteControl::new(muted, true);
            if let Some(requested) = requested_muted {
                control.proposed = requested;
            }
            control
        });
        Self::new(AudioStatus::Failed(error), volume, mute)
            .with_retry_action(Some(OverviewAction::new("Retry Audio", true, retry)))
    }

    fn new(
        status: AudioStatus,
        volume: Option<AudioVolumeControl>,
        mute: Option<MuteControl>,
    ) -> Self {
        Self {
            status,
            volume,
            mute,
            retry_action: None,
        }
    }

    fn with_retry_action(mut self, action: Option<OverviewAction>) -> Self {
        self.retry_action = action;
        self
    }

    pub fn status(&self) -> &AudioStatus {
        &self.status
    }
    pub fn volume(&self) -> Option<&AudioVolumeControl> {
        self.volume.as_ref()
    }
    pub fn mute(&self) -> Option<&MuteControl> {
        self.mute.as_ref()
    }
    pub fn retry_action(&self) -> Option<&OverviewAction> {
        self.retry_action.as_ref()
    }
}

impl AudioVolumeControl {
    fn new(current: CurrentVolume, proposed: VolumeLevel, enabled: bool) -> Self {
        Self {
            label: "TV Volume".to_string(),
            current,
            proposed,
            minimum: VOLUME_MIN,
            maximum: VOLUME_MAX,
            step: 1,
            enabled,
        }
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn current(&self) -> CurrentVolume {
        self.current
    }
    pub fn proposed(&self) -> VolumeLevel {
        self.proposed
    }
    pub fn minimum(&self) -> u8 {
        self.minimum
    }
    pub fn maximum(&self) -> u8 {
        self.maximum
    }
    pub fn step(&self) -> u8 {
        self.step
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

impl MuteControl {
    fn new(current: bool, enabled: bool) -> Self {
        Self {
            label: "Mute TV".to_string(),
            current,
            proposed: current,
            enabled,
        }
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn current(&self) -> bool {
        self.current
    }
    pub fn proposed(&self) -> bool {
        self.proposed
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}

impl OverviewAction {
    pub fn new(label: &str, enabled: bool, intent: OverviewIntent) -> Self {
        Self {
            label: label.to_string(),
            enabled,
            intent,
        }
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn intent(&self) -> OverviewIntent {
        self.intent
    }
}

fn audio_message(volume: CurrentVolume, muted: bool) -> String {
    if muted {
        format!("Muted · volume {volume}")
    } else {
        format!("Volume {volume}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tv::OledBrightness;

    #[test]
    fn brightness_retry_uses_the_declared_action_and_availability() {
        let current = OledBrightness::new(50).unwrap();
        let overview = |brightness| {
            OverviewPresentation::new(
                TvSummaryPresentation::loading(),
                brightness,
                AudioPresentation::loading(),
            )
        };
        for brightness in [
            BrightnessPresentation::loading(),
            BrightnessPresentation::overview_ready(current, current),
            BrightnessPresentation::overview_applying(current, current),
            BrightnessPresentation::ready(current, current),
        ] {
            assert!(overview(brightness).brightness_retry_action().is_none());
        }

        let error = UserFacingError::new("Could not change brightness.", "Retry the operation.");
        for (brightness, enabled) in [
            (BrightnessPresentation::read_failed(error.clone()), true),
            (
                BrightnessPresentation::write_failed(current, current, error.clone()),
                false,
            ),
            (
                BrightnessPresentation::overview_write_failed(current, current, error),
                true,
            ),
        ] {
            let action = overview(brightness).brightness_retry_action().unwrap();
            assert_eq!(action.label(), "Retry OLED Pixel Brightness");
            assert_eq!(action.enabled(), enabled);
            assert_eq!(action.intent(), OverviewIntent::RetryBrightness);
        }
    }
}
