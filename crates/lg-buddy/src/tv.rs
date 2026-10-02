use std::error::Error;
use std::fmt;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

use crate::auth::{resolve_config_owner, AuthContextError};
use crate::config::{HdmiInput, MacAddress, TvPlatform};
use crate::platform_access_token::{PlatformAccessTokenStore, PlatformAccessTokenStoreError};
use crate::web_os::{WebOsEndpoint, WebOsPairingPolicy, WebOsTvClient};
use crate::wol::{WakeOnLanError, WakeOnLanSender};

#[cfg(test)]
pub(crate) mod test_support;

pub const OLED_BRIGHTNESS_MIN: u8 = 0;
pub const OLED_BRIGHTNESS_MAX: u8 = 100;
pub const VOLUME_MIN: u8 = 0;
pub const VOLUME_MAX: u8 = 100;
const DEFAULT_WEBOS_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_WEBOS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_WEBOS_UNATTENDED_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvOperation {
    ReadModelName,
    ReadInput,
    SetInput,
    ReadOledBrightness,
    SetOledBrightness,
    ReadAudioStatus,
    SetVolume,
    VolumeUp,
    VolumeDown,
    SetMuted,
    PowerOff,
    BlankScreen,
    UnblankScreen,
}

impl TvOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadModelName => "read model name",
            Self::ReadInput => "read input",
            Self::SetInput => "set input",
            Self::ReadOledBrightness => "read OLED brightness",
            Self::SetOledBrightness => "set OLED brightness",
            Self::ReadAudioStatus => "read audio status",
            Self::SetVolume => "set volume",
            Self::VolumeUp => "increase volume",
            Self::VolumeDown => "decrease volume",
            Self::SetMuted => "set mute",
            Self::PowerOff => "power off",
            Self::BlankScreen => "blank screen",
            Self::UnblankScreen => "unblank screen",
        }
    }
}

impl fmt::Display for TvOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvErrorKind {
    Transport,
    Authentication,
    Rejected,
    InvalidResponse,
    ScreenNotVisible,
    Internal,
}

impl TvErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Authentication => "authentication",
            Self::Rejected => "rejected",
            Self::InvalidResponse => "invalid_response",
            Self::ScreenNotVisible => "screen_not_visible",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvError {
    operation: TvOperation,
    kind: TvErrorKind,
    detail: String,
}

impl fmt::Display for TvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "could not {}: {}", self.operation, self.detail)
    }
}

impl Error for TvError {}

impl TvError {
    pub fn operation(&self) -> TvOperation {
        self.operation
    }

    pub fn kind(&self) -> TvErrorKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub(crate) fn new(
        operation: TvOperation,
        kind: TvErrorKind,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            operation,
            kind,
            detail: detail.into(),
        }
    }

    pub fn indicates_screen_not_visible(&self) -> bool {
        self.kind == TvErrorKind::ScreenNotVisible
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OledBrightnessParseError {
    value: String,
}

impl OledBrightnessParseError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl fmt::Display for OledBrightnessParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid OLED brightness `{}`; expected an integer from {} to {}",
            self.value, OLED_BRIGHTNESS_MIN, OLED_BRIGHTNESS_MAX
        )
    }
}

impl Error for OledBrightnessParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OledBrightness(u8);

impl OledBrightness {
    pub const DEFAULT: Self = Self(50);

    pub fn new(value: u8) -> Result<Self, OledBrightnessParseError> {
        if value <= OLED_BRIGHTNESS_MAX {
            Ok(Self(value))
        } else {
            Err(OledBrightnessParseError::new(value.to_string()))
        }
    }

    pub fn parse(value: &str) -> Result<Self, OledBrightnessParseError> {
        match value.parse::<i64>() {
            Ok(parsed)
                if parsed >= i64::from(OLED_BRIGHTNESS_MIN)
                    && parsed <= i64::from(OLED_BRIGHTNESS_MAX) =>
            {
                Ok(Self(parsed as u8))
            }
            _ => Err(OledBrightnessParseError::new(value)),
        }
    }

    pub fn as_percent(self) -> u8 {
        self.0
    }
}

impl fmt::Display for OledBrightness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeLevelParseError {
    value: String,
}

impl VolumeLevelParseError {
    fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

impl fmt::Display for VolumeLevelParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid volume `{}`; expected an integer from {} to {}",
            self.value, VOLUME_MIN, VOLUME_MAX
        )
    }
}

impl Error for VolumeLevelParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeLevel(u8);

impl VolumeLevel {
    pub fn new(value: u8) -> Result<Self, VolumeLevelParseError> {
        if value <= VOLUME_MAX {
            Ok(Self(value))
        } else {
            Err(VolumeLevelParseError::new(value.to_string()))
        }
    }

    pub fn parse(value: &str) -> Result<Self, VolumeLevelParseError> {
        match value.parse::<i64>() {
            Ok(parsed) if parsed >= i64::from(VOLUME_MIN) && parsed <= i64::from(VOLUME_MAX) => {
                Ok(Self(parsed as u8))
            }
            _ => Err(VolumeLevelParseError::new(value)),
        }
    }

    pub fn as_percent(self) -> u8 {
        self.0
    }
}

impl fmt::Display for VolumeLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentVolume {
    Level(VolumeLevel),
    Unknown,
}

impl fmt::Display for CurrentVolume {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Level(volume) => write!(f, "{volume}"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStatus {
    volume: CurrentVolume,
    muted: bool,
}

impl AudioStatus {
    pub fn new(volume: CurrentVolume, muted: bool) -> Self {
        Self { volume, muted }
    }

    pub fn volume(self) -> CurrentVolume {
        self.volume
    }

    pub fn is_muted(self) -> bool {
        self.muted
    }
}

pub trait TvClient {
    fn model_name(&self) -> Result<String, TvError>;
    fn current_input(&self) -> Result<CurrentInput, TvError>;
    fn oled_brightness(&self) -> Result<OledBrightness, TvError>;
    fn audio_status(&self) -> Result<AudioStatus, TvError>;
    fn set_input(&self, input: HdmiInput) -> Result<(), TvError>;
    fn set_oled_brightness(&self, brightness: OledBrightness) -> Result<(), TvError>;
    fn set_volume(&self, volume: VolumeLevel) -> Result<(), TvError>;
    fn volume_up(&self) -> Result<(), TvError>;
    fn volume_down(&self) -> Result<(), TvError>;
    fn set_muted(&self, muted: bool) -> Result<(), TvError>;
    fn power_off(&self) -> Result<(), TvError>;
    fn blank_screen(&self) -> Result<(), TvError>;
    fn unblank_screen(&self) -> Result<(), TvError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TvClientBuildOptions {
    command_timeout: Option<Duration>,
    webos_pairing_policy: WebOsPairingPolicy,
}

impl TvClientBuildOptions {
    pub(crate) fn production() -> Self {
        Self {
            command_timeout: None,
            webos_pairing_policy: WebOsPairingPolicy::PairIfNeeded,
        }
    }

    pub(crate) fn with_command_timeout(mut self, timeout: Duration) -> Self {
        self.command_timeout = Some(timeout);
        self
    }

    pub(crate) fn stored_token_only(mut self) -> Self {
        self.webos_pairing_policy = WebOsPairingPolicy::StoredTokenOnly;
        self
    }
}

#[derive(Debug)]
pub enum TvClientBuildError {
    AuthContext(AuthContextError),
    TokenStore(PlatformAccessTokenStoreError),
    StalePlatform,
}

impl fmt::Display for TvClientBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthContext(error) => write!(f, "{error}"),
            Self::TokenStore(error) => write!(f, "{error}"),
            Self::StalePlatform => write!(
                f,
                "tvs_primary_platform=bscpylgtv is retired; restart LG Buddy to convert the saved profile"
            ),
        }
    }
}

impl Error for TvClientBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::AuthContext(error) => Some(error),
            Self::TokenStore(error) => Some(error),
            Self::StalePlatform => None,
        }
    }
}

pub(crate) fn build_tv_client(
    config_path: &Path,
    tv_ip: Ipv4Addr,
    platform: TvPlatform,
    options: TvClientBuildOptions,
) -> Result<WebOsTvClient, TvClientBuildError> {
    match platform {
        TvPlatform::Bscpylgtv => Err(TvClientBuildError::StalePlatform),
        TvPlatform::LgWebOs => {
            let owner =
                resolve_config_owner(config_path).map_err(TvClientBuildError::AuthContext)?;
            let token_store = PlatformAccessTokenStore::for_primary_profile(config_path, owner)
                .map_err(TvClientBuildError::TokenStore)?;
            let response_timeout =
                options
                    .command_timeout
                    .unwrap_or(match options.webos_pairing_policy {
                        WebOsPairingPolicy::PairIfNeeded => DEFAULT_WEBOS_RESPONSE_TIMEOUT,
                        WebOsPairingPolicy::StoredTokenOnly => {
                            DEFAULT_WEBOS_UNATTENDED_RESPONSE_TIMEOUT
                        }
                    });
            Ok(WebOsTvClient::new(
                WebOsEndpoint::wss(tv_ip),
                DEFAULT_WEBOS_CONNECT_TIMEOUT,
                response_timeout,
                token_store,
                options.webos_pairing_policy,
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentInput {
    Hdmi(HdmiInput),
    Other(String),
}

impl CurrentInput {
    pub fn from_raw(value: String) -> Self {
        match HdmiInput::from_app_id(&value) {
            Some(input) => Self::Hdmi(input),
            None => Self::Other(value),
        }
    }

    pub fn is_hdmi(&self, input: HdmiInput) -> bool {
        matches!(self, Self::Hdmi(current) if *current == input)
    }
}

impl fmt::Display for CurrentInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hdmi(input) => write!(f, "{}", input.as_str()),
            Self::Other(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvDevice<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C> TvDevice<'a, C> {
    pub fn new(client: &'a C, tv_ip: Ipv4Addr) -> Self {
        Self { client, tv_ip }
    }
}

impl<'a, C: TvClient> TvDevice<'a, C> {
    pub fn input(&self) -> TvInput<'a, C> {
        TvInput {
            client: self.client,
        }
    }

    pub fn screen(&self) -> TvScreen<'a, C> {
        TvScreen {
            client: self.client,
        }
    }

    pub fn picture(&self) -> TvPicture<'a, C> {
        TvPicture {
            client: self.client,
        }
    }

    pub fn audio(&self) -> TvAudio<'a, C> {
        TvAudio {
            client: self.client,
        }
    }

    pub fn power(&self) -> TvPower<'a, C> {
        TvPower {
            client: self.client,
            tv_ip: self.tv_ip,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvInput<'a, C> {
    client: &'a C,
}

impl<'a, C: TvClient> TvInput<'a, C> {
    pub fn current(&self) -> Result<CurrentInput, TvError> {
        self.client.current_input()
    }

    pub fn set(&self, input: HdmiInput) -> Result<(), TvError> {
        self.client.set_input(input)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvScreen<'a, C> {
    client: &'a C,
}

impl<'a, C: TvClient> TvScreen<'a, C> {
    pub fn blank(&self) -> Result<(), TvError> {
        self.client.blank_screen()
    }

    pub fn unblank(&self) -> Result<(), TvError> {
        self.client.unblank_screen()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvPicture<'a, C> {
    client: &'a C,
}

#[derive(Debug, Clone, Copy)]
pub struct TvAudio<'a, C> {
    client: &'a C,
}

impl<'a, C: TvClient> TvAudio<'a, C> {
    pub fn status(&self) -> Result<AudioStatus, TvError> {
        self.client.audio_status()
    }

    pub fn set_volume(&self, volume: VolumeLevel) -> Result<(), TvError> {
        self.client.set_volume(volume)
    }

    pub fn volume_up(&self) -> Result<(), TvError> {
        self.client.volume_up()
    }

    pub fn volume_down(&self) -> Result<(), TvError> {
        self.client.volume_down()
    }

    pub fn set_muted(&self, muted: bool) -> Result<(), TvError> {
        self.client.set_muted(muted)
    }
}

impl<'a, C: TvClient> TvPicture<'a, C> {
    pub fn oled_brightness(&self) -> Result<OledBrightness, TvError> {
        self.client.oled_brightness()
    }

    pub fn set_oled_brightness(&self, brightness: OledBrightness) -> Result<(), TvError> {
        self.client.set_oled_brightness(brightness)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvPower<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvPower<'a, C> {
    pub fn wake<W: WakeOnLanSender>(
        &self,
        sender: &W,
        tv_mac: &MacAddress,
    ) -> Result<(), WakeOnLanError> {
        sender.send_magic_packet_to(tv_mac, self.tv_ip)
    }

    pub fn off(&self) -> Result<(), TvError> {
        self.client.power_off()
    }
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::test_support::FakeTvClient;
    use super::{
        build_tv_client, CurrentInput, OledBrightness, TvClientBuildError, TvClientBuildOptions,
        TvDevice, VolumeLevel,
    };
    use crate::config::{HdmiInput, MacAddress, TvPlatform};
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use std::cell::RefCell;
    use std::net::Ipv4Addr;
    use std::time::Duration;
    use support::TestConfigFile;

    #[test]
    fn oled_brightness_accepts_boundary_values() {
        let minimum = OledBrightness::parse("0").expect("minimum brightness should parse");
        let maximum = OledBrightness::parse("100").expect("maximum brightness should parse");

        assert_eq!(minimum.as_percent(), 0);
        assert_eq!(maximum.as_percent(), 100);
        assert_eq!(minimum.to_string(), "0");
        assert_eq!(OledBrightness::DEFAULT.as_percent(), 50);
    }

    #[test]
    fn oled_brightness_rejects_invalid_values() {
        for value in ["-1", "101", "abc", "50.5"] {
            let err = OledBrightness::parse(value).expect_err("invalid brightness should fail");
            assert!(err
                .to_string()
                .contains("expected an integer from 0 to 100"));
        }

        assert!(OledBrightness::new(101).is_err());
    }

    #[test]
    fn volume_level_accepts_boundaries_and_rejects_other_values() {
        assert_eq!(VolumeLevel::parse("0").unwrap().as_percent(), 0);
        assert_eq!(VolumeLevel::parse("100").unwrap().as_percent(), 100);

        for value in ["-1", "101", "abc", "50.5"] {
            let err = VolumeLevel::parse(value).expect_err("invalid volume should fail");
            assert!(err
                .to_string()
                .contains("expected an integer from 0 to 100"));
        }
    }

    #[test]
    fn centralized_factory_rejects_stale_bscpylgtv_platform() {
        let config = TestConfigFile::new("tv-client-stale-platform");
        config.write_sample("HDMI_2");
        let tv_ip = ip("192.0.2.42");

        let stale = build_tv_client(
            config.path(),
            tv_ip,
            TvPlatform::Bscpylgtv,
            TvClientBuildOptions::production(),
        )
        .expect_err("bscpylgtv should be rejected as a stale platform");
        assert!(
            matches!(stale, TvClientBuildError::StalePlatform),
            "expected a typed stale-platform error, got: {stale}"
        );

        let webos = build_tv_client(
            config.path(),
            tv_ip,
            TvPlatform::LgWebOs,
            TvClientBuildOptions::production(),
        )
        .expect("build native webOS TV client");
        let _ = webos;
    }

    #[test]
    fn centralized_factory_applies_command_timeout_to_native_responses() {
        let config = TestConfigFile::new("tv-client-native-timeout");
        config.write_sample("HDMI_2");
        let command_timeout = Duration::from_secs(3);

        let client = build_tv_client(
            config.path(),
            ip("192.0.2.42"),
            TvPlatform::LgWebOs,
            TvClientBuildOptions::production().with_command_timeout(command_timeout),
        )
        .expect("build native webOS TV client");

        assert_eq!(client.response_timeout(), command_timeout);
    }

    #[test]
    fn tv_device_maps_hdmi_inputs_to_typed_values() {
        let client = FakeTvClient::new("hdmi");
        client.set_input("HDMI_4");
        let tv = TvDevice::new(&client, ip("10.0.0.7"));

        assert_eq!(
            tv.input().current().unwrap(),
            CurrentInput::Hdmi(HdmiInput::Hdmi4)
        );
        assert_eq!(client.calls()[0].command, "current_input");
    }

    #[test]
    fn tv_device_preserves_non_hdmi_inputs() {
        let client = FakeTvClient::new("other-input");
        client.set_input("com.webos.app.youtube");
        let tv = TvDevice::new(&client, ip("10.0.0.9"));

        assert_eq!(
            tv.input().current().unwrap(),
            CurrentInput::Other("com.webos.app.youtube".into())
        );
    }

    #[test]
    fn tv_screen_blank_uses_domain_facade() {
        let client = FakeTvClient::new("blank");
        let tv = TvDevice::new(&client, ip("10.0.0.11"));
        tv.screen().blank().unwrap();

        assert!(!client.state_snapshot().screen_on);
        assert_eq!(client.calls()[0].command, "blank_screen");
    }

    #[test]
    fn tv_picture_set_oled_brightness_uses_domain_facade() {
        let client = FakeTvClient::new("brightness-write");
        let tv = TvDevice::new(&client, ip("10.0.0.12"));
        tv.picture()
            .set_oled_brightness(OledBrightness::new(40).unwrap())
            .unwrap();

        assert_eq!(client.state_snapshot().backlight, 40);
        assert_eq!(client.calls()[0].command, "set_oled_brightness");
    }

    #[test]
    fn tv_picture_reads_oled_brightness_via_domain_facade() {
        let client = FakeTvClient::new("brightness-read");
        client.set_backlight(33);
        let tv = TvDevice::new(&client, ip("10.0.0.13"));

        assert_eq!(tv.picture().oled_brightness().unwrap().as_percent(), 33);
        assert_eq!(client.calls()[0].command, "oled_brightness");
    }

    #[test]
    fn tv_power_wake_uses_wake_on_lan_sender() {
        let client = FakeTvClient::new("wake");
        let tv = TvDevice::new(&client, ip("10.0.0.15"));
        let sender = RecordingWakeOnLanSender::default();
        let mac: MacAddress = "01:23:45:67:89:ab".parse().unwrap();

        tv.power().wake(&sender, &mac).unwrap();

        assert_eq!(sender.0.borrow().as_slice(), &[(mac, ip("10.0.0.15"))]);
        assert!(client.calls().is_empty());
    }

    #[derive(Default)]
    struct RecordingWakeOnLanSender(RefCell<Vec<(MacAddress, Ipv4Addr)>>);

    impl WakeOnLanSender for RecordingWakeOnLanSender {
        fn send_magic_packet(&self, _mac: &MacAddress) -> Result<(), WakeOnLanError> {
            unreachable!("TvPower must pass its target IP")
        }

        fn send_magic_packet_to(
            &self,
            mac: &MacAddress,
            target_ip: Ipv4Addr,
        ) -> Result<(), WakeOnLanError> {
            self.0.borrow_mut().push((*mac, target_ip));
            Ok(())
        }
    }

    fn ip(value: &str) -> Ipv4Addr {
        value.parse().expect("parse IPv4 address")
    }
}
