use std::env;
use std::error::Error;
use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{HdmiInput, MacAddress};
use crate::wol::{WakeOnLanError, WakeOnLanSender};

pub const DEFAULT_BSCPYLGTV_COMMAND_PATH: &str = "/usr/bin/LG_Buddy_PIP/bin/bscpylgtvcommand";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    pub fn new(stdout: String, stderr: String) -> Self {
        Self { stdout, stderr }
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    pub fn combined_output(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (false, false) => format!("{}{}", self.stdout, self.stderr),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (true, true) => String::new(),
        }
    }
}

#[derive(Debug)]
pub enum TvError {
    Io {
        command: &'static str,
        source: io::Error,
    },
    CommandFailed {
        command: &'static str,
        status: Option<i32>,
        output: CommandOutput,
    },
    InvalidOutput {
        command: &'static str,
        output: CommandOutput,
        message: &'static str,
    },
}

impl fmt::Display for TvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { command, source } => {
                write!(f, "failed to run `{command}`: {source}")
            }
            Self::CommandFailed {
                command,
                status,
                output,
            } => {
                write!(
                    f,
                    "`{command}` failed with status {}",
                    status
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "terminated by signal".to_string())
                )?;

                let combined = output.combined_output();
                if !combined.trim().is_empty() {
                    write!(f, ": {}", combined.trim_end())?;
                }

                Ok(())
            }
            Self::InvalidOutput {
                command,
                output,
                message,
            } => {
                write!(f, "invalid output from `{command}`: {message}")?;
                let combined = output.combined_output();
                if !combined.trim().is_empty() {
                    write!(f, ": {}", combined.trim_end())?;
                }
                Ok(())
            }
        }
    }
}

impl Error for TvError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::CommandFailed { .. } | Self::InvalidOutput { .. } => None,
        }
    }
}

impl TvError {
    pub fn indicates_active_screen_state(&self) -> bool {
        match self {
            Self::CommandFailed { output, .. } | Self::InvalidOutput { output, .. } => {
                output.stderr().contains("errorCode': '-102'")
                    || output.stdout().contains("errorCode': '-102'")
            }
            Self::Io { .. } => false,
        }
    }
}

pub trait TvClient {
    fn get_input(&self, tv_ip: Ipv4Addr) -> Result<String, TvError>;
    fn get_oled_brightness(&self, tv_ip: Ipv4Addr) -> Result<u8, TvError>;
    fn set_input(&self, tv_ip: Ipv4Addr, input: HdmiInput) -> Result<CommandOutput, TvError>;
    fn set_oled_brightness(
        &self,
        tv_ip: Ipv4Addr,
        brightness: u8,
    ) -> Result<CommandOutput, TvError>;
    fn power_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
    fn turn_screen_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
    fn turn_screen_on(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
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
            tv_ip: self.tv_ip,
        }
    }

    pub fn screen(&self) -> TvScreen<'a, C> {
        TvScreen {
            client: self.client,
            tv_ip: self.tv_ip,
        }
    }

    pub fn picture(&self) -> TvPicture<'a, C> {
        TvPicture {
            client: self.client,
            tv_ip: self.tv_ip,
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
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvInput<'a, C> {
    pub fn current(&self) -> Result<CurrentInput, TvError> {
        self.client
            .get_input(self.tv_ip)
            .map(CurrentInput::from_raw)
    }

    pub fn set(&self, input: HdmiInput) -> Result<CommandOutput, TvError> {
        self.client.set_input(self.tv_ip, input)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvScreen<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvScreen<'a, C> {
    pub fn blank(&self) -> Result<CommandOutput, TvError> {
        self.client.turn_screen_off(self.tv_ip)
    }

    pub fn unblank(&self) -> Result<CommandOutput, TvError> {
        self.client.turn_screen_on(self.tv_ip)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TvPicture<'a, C> {
    client: &'a C,
    tv_ip: Ipv4Addr,
}

impl<'a, C: TvClient> TvPicture<'a, C> {
    pub fn oled_brightness(&self) -> Result<u8, TvError> {
        self.client.get_oled_brightness(self.tv_ip)
    }

    pub fn set_oled_brightness(&self, brightness: u8) -> Result<CommandOutput, TvError> {
        self.client.set_oled_brightness(self.tv_ip, brightness)
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
        sender.send_magic_packet(tv_mac)
    }

    pub fn off(&self) -> Result<CommandOutput, TvError> {
        self.client.power_off(self.tv_ip)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BscpylgtvCommandClient {
    command_path: PathBuf,
    command_args: Vec<String>,
}

impl Default for BscpylgtvCommandClient {
    fn default() -> Self {
        Self::new(DEFAULT_BSCPYLGTV_COMMAND_PATH)
    }
}

impl BscpylgtvCommandClient {
    pub fn new(command_path: impl Into<PathBuf>) -> Self {
        Self {
            command_path: command_path.into(),
            command_args: Vec::new(),
        }
    }

    pub fn with_args<I, S>(command_path: impl Into<PathBuf>, command_args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            command_path: command_path.into(),
            command_args: command_args.into_iter().map(Into::into).collect(),
        }
    }

    pub fn from_env() -> Self {
        match env::var_os("LG_BUDDY_BSCPYLGTV_COMMAND") {
            Some(path) => Self::new(PathBuf::from(path)),
            None => Self::default(),
        }
    }

    pub fn command_path(&self) -> &Path {
        &self.command_path
    }

    pub fn command_args(&self) -> &[String] {
        &self.command_args
    }

    fn run_command(
        &self,
        tv_ip: Ipv4Addr,
        operation: &'static str,
        extra_args: &[&str],
    ) -> Result<CommandOutput, TvError> {
        let output = Command::new(&self.command_path)
            .args(&self.command_args)
            .arg(tv_ip.to_string())
            .arg(operation)
            .args(extra_args)
            .output()
            .map_err(|source| TvError::Io {
                command: operation,
                source,
            })?;

        let rendered = CommandOutput::new(
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        );

        if output.status.success() {
            Ok(rendered)
        } else {
            Err(TvError::CommandFailed {
                command: operation,
                status: output.status.code(),
                output: rendered,
            })
        }
    }
}

impl TvClient for BscpylgtvCommandClient {
    fn get_input(&self, tv_ip: Ipv4Addr) -> Result<String, TvError> {
        let output = self.run_command(tv_ip, "get_input", &[])?;
        last_non_empty_line(output.stdout()).ok_or(TvError::InvalidOutput {
            command: "get_input",
            output,
            message: "expected a non-empty line in stdout",
        })
    }

    fn get_oled_brightness(&self, tv_ip: Ipv4Addr) -> Result<u8, TvError> {
        let output = self.run_command(tv_ip, "get_picture_settings", &[])?;
        parse_backlight(output.stdout()).ok_or(TvError::InvalidOutput {
            command: "get_picture_settings",
            output,
            message: "expected a backlight value in stdout",
        })
    }

    fn set_input(&self, tv_ip: Ipv4Addr, input: HdmiInput) -> Result<CommandOutput, TvError> {
        self.run_command(tv_ip, "set_input", &[input.as_str()])
    }

    fn set_oled_brightness(
        &self,
        tv_ip: Ipv4Addr,
        brightness: u8,
    ) -> Result<CommandOutput, TvError> {
        let backlight = format!("{{\"backlight\": {brightness}}}");
        self.run_command(tv_ip, "set_settings", &["picture", backlight.as_str()])
    }

    fn power_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        self.run_command(tv_ip, "power_off", &[])
    }

    fn turn_screen_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        self.run_command(tv_ip, "turn_screen_off", &[])
    }

    fn turn_screen_on(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        self.run_command(tv_ip, "turn_screen_on", &[])
    }
}

fn last_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::to_string)
}

fn parse_backlight(output: &str) -> Option<u8> {
    for token in output.split([',', '{', '}', '\n']) {
        let token = token.trim();
        if !(token.starts_with("'backlight'") || token.starts_with("\"backlight\"")) {
            continue;
        }

        let (_, value) = token.split_once(':')?;
        let parsed = value
            .trim()
            .trim_matches('\'')
            .trim_matches('"')
            .parse::<u16>()
            .ok()?;
        if parsed <= 100 {
            return Some(parsed as u8);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{
        BscpylgtvCommandClient, CommandOutput, CurrentInput, TvClient, TvDevice, TvError,
        DEFAULT_BSCPYLGTV_COMMAND_PATH,
    };
    use crate::config::{HdmiInput, MacAddress};
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use std::cell::RefCell;
    use std::net::Ipv4Addr;
    use std::path::Path;
    use support::MockBscpylgtv;

    #[test]
    fn default_client_uses_expected_command_path() {
        let client = BscpylgtvCommandClient::default();
        assert_eq!(
            client.command_path(),
            Path::new(DEFAULT_BSCPYLGTV_COMMAND_PATH)
        );
        assert!(client.command_args().is_empty());
    }

    #[test]
    fn combined_output_preserves_stdout_and_stderr() {
        let output = CommandOutput::new("hello\n".to_string(), "world\n".to_string());
        assert_eq!(output.combined_output(), "hello\nworld\n");
    }

    #[test]
    fn get_input_uses_last_non_empty_stdout_line() {
        let mock = MockBscpylgtv::new("tv-get-input");
        mock.queue_success("get_input", "\nignored\ncom.webos.app.hdmi2\n");

        let client = client_for_mock(&mock);
        let input = client
            .get_input(ip("192.168.1.42"))
            .expect("get_input should succeed");

        assert_eq!(input, "com.webos.app.hdmi2");
        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "192.168.1.42".to_string(),
                "get_input".to_string(),
                Vec::<String>::new(),
            )]
        );
    }

    #[test]
    fn get_input_rejects_empty_output() {
        let mock = MockBscpylgtv::new("tv-get-input-empty");
        mock.queue_success("get_input", "");

        let client = client_for_mock(&mock);
        let err = client
            .get_input(ip("192.168.1.42"))
            .expect_err("empty output should fail");

        match err {
            TvError::InvalidOutput {
                command, message, ..
            } => {
                assert_eq!(command, "get_input");
                assert_eq!(message, "expected a non-empty line in stdout");
            }
            other => panic!("expected invalid output error, got {other:?}"),
        }
    }

    #[test]
    fn set_input_passes_expected_arguments() {
        let mock = MockBscpylgtv::new("tv-set-input");
        let client = client_for_mock(&mock);
        client
            .set_input(ip("10.0.0.5"), HdmiInput::Hdmi3)
            .expect("set_input should succeed");

        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.5".to_string(),
                "set_input".to_string(),
                vec!["HDMI_3".to_string()],
            )]
        );
    }

    #[test]
    fn set_oled_brightness_passes_expected_arguments() {
        let mock = MockBscpylgtv::new("tv-set-brightness");
        let client = client_for_mock(&mock);
        client
            .set_oled_brightness(ip("10.0.0.5"), 65)
            .expect("set_oled_brightness should succeed");

        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.5".to_string(),
                "set_settings".to_string(),
                vec!["picture".to_string(), "{\"backlight\": 65}".to_string()],
            )]
        );
        assert_eq!(mock.state_snapshot().backlight, 65);
    }

    #[test]
    fn get_oled_brightness_reads_backlight_from_picture_settings() {
        let mock = MockBscpylgtv::new("tv-get-brightness");
        mock.set_backlight(72);
        let client = client_for_mock(&mock);

        let brightness = client
            .get_oled_brightness(ip("10.0.0.5"))
            .expect("get_oled_brightness should succeed");

        assert_eq!(brightness, 72);
        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.5".to_string(),
                "get_picture_settings".to_string(),
                Vec::<String>::new(),
            )]
        );
    }

    #[test]
    fn get_oled_brightness_rejects_missing_backlight_value() {
        let mock = MockBscpylgtv::new("tv-get-brightness-invalid");
        mock.queue_success("get_picture_settings", "{'contrast': 85}\n");
        let client = client_for_mock(&mock);

        let err = client
            .get_oled_brightness(ip("10.0.0.5"))
            .expect_err("missing backlight should fail");

        match err {
            TvError::InvalidOutput {
                command, message, ..
            } => {
                assert_eq!(command, "get_picture_settings");
                assert_eq!(message, "expected a backlight value in stdout");
            }
            other => panic!("expected invalid output error, got {other:?}"),
        }
    }

    #[test]
    fn tv_device_maps_hdmi_inputs_to_typed_values() {
        let mock = MockBscpylgtv::new("tv-device-current-hdmi");
        mock.set_input("HDMI_4");

        let client = client_for_mock(&mock);
        let tv = TvDevice::new(&client, ip("10.0.0.7"));
        let current = tv.input().current().expect("current input should parse");

        assert_eq!(current, CurrentInput::Hdmi(HdmiInput::Hdmi4));
    }

    #[test]
    fn tv_device_preserves_non_hdmi_inputs() {
        let mock = MockBscpylgtv::new("tv-device-current-other");
        mock.queue_success("get_input", "com.webos.app.youtube\n");

        let client = client_for_mock(&mock);
        let tv = TvDevice::new(&client, ip("10.0.0.9"));
        let current = tv.input().current().expect("current input should parse");

        assert_eq!(
            current,
            CurrentInput::Other("com.webos.app.youtube".to_string())
        );
    }

    #[test]
    fn tv_screen_blank_uses_domain_facade() {
        let mock = MockBscpylgtv::new("tv-device-screen-blank");
        let client = client_for_mock(&mock);
        let tv = TvDevice::new(&client, ip("10.0.0.11"));
        tv.screen().blank().expect("screen blank should succeed");

        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.11".to_string(),
                "turn_screen_off".to_string(),
                Vec::<String>::new(),
            )]
        );
    }

    #[test]
    fn tv_picture_set_oled_brightness_uses_domain_facade() {
        let mock = MockBscpylgtv::new("tv-device-picture-brightness");
        let client = client_for_mock(&mock);
        let tv = TvDevice::new(&client, ip("10.0.0.12"));
        tv.picture()
            .set_oled_brightness(40)
            .expect("brightness set should succeed");

        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.12".to_string(),
                "set_settings".to_string(),
                vec!["picture".to_string(), "{\"backlight\": 40}".to_string()],
            )]
        );
    }

    #[test]
    fn tv_picture_reads_oled_brightness_via_domain_facade() {
        let mock = MockBscpylgtv::new("tv-device-picture-read-brightness");
        mock.set_backlight(33);
        let client = client_for_mock(&mock);
        let tv = TvDevice::new(&client, ip("10.0.0.13"));

        let brightness = tv
            .picture()
            .oled_brightness()
            .expect("brightness read should succeed");

        assert_eq!(brightness, 33);
        assert_eq!(
            mock.calls()
                .into_iter()
                .map(|call| (call.tv_ip, call.command, call.args))
                .collect::<Vec<_>>(),
            vec![(
                "10.0.0.13".to_string(),
                "get_picture_settings".to_string(),
                Vec::<String>::new(),
            )]
        );
    }

    #[test]
    fn tv_power_wake_uses_wake_on_lan_sender() {
        let client = BscpylgtvCommandClient::default();
        let tv = TvDevice::new(&client, ip("10.0.0.15"));
        let sender = RecordingWakeOnLanSender::default();
        let mac = parse_mac("01:23:45:67:89:ab");

        tv.power()
            .wake(&sender, &mac)
            .expect("wake on lan should succeed");

        assert_eq!(sender.calls(), vec![mac]);
    }

    #[test]
    fn command_failures_preserve_status_and_output() {
        let mock = MockBscpylgtv::new("tv-command-failure");
        mock.queue_error("turn_screen_on", 7, "failure stderr\n");
        let client = client_for_mock(&mock);
        let err = client
            .turn_screen_on(ip("10.0.0.8"))
            .expect_err("turn_screen_on should fail");

        match err {
            TvError::CommandFailed {
                command,
                status,
                output,
            } => {
                assert_eq!(command, "turn_screen_on");
                assert_eq!(status, Some(7));
                assert_eq!(output.stdout(), "");
                assert_eq!(output.stderr(), "failure stderr\n");
            }
            other => panic!("expected command failure, got {other:?}"),
        }
    }

    fn ip(value: &str) -> Ipv4Addr {
        value.parse().expect("parse IPv4 address")
    }

    fn parse_mac(value: &str) -> MacAddress {
        value.parse().expect("parse mac address")
    }

    fn client_for_mock(mock: &MockBscpylgtv) -> BscpylgtvCommandClient {
        BscpylgtvCommandClient::with_args(mock.command_path(), mock.command_args())
    }

    #[derive(Default)]
    struct RecordingWakeOnLanSender {
        calls: RefCell<Vec<MacAddress>>,
    }

    impl RecordingWakeOnLanSender {
        fn calls(&self) -> Vec<MacAddress> {
            self.calls.borrow().clone()
        }
    }

    impl WakeOnLanSender for RecordingWakeOnLanSender {
        fn send_magic_packet(&self, mac: &MacAddress) -> Result<(), WakeOnLanError> {
            self.calls.borrow_mut().push(mac.clone());
            Ok(())
        }
    }
}
