use std::io::Write;

use crate::config::{load_config, resolve_config_path_from_env, Config};
use crate::state::{ScreenOwnershipMarker, StateScope};
use crate::tv::{BscpylgtvCommandClient, CurrentInput, TvClient, TvDevice};
use crate::RunError;

pub fn run_screen_off<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker =
        ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
    let tv_client = BscpylgtvCommandClient::from_env();

    run_screen_off_with(writer, &config, &marker, &tv_client)
}

pub fn run_screen_off_with<W: Write>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &impl TvClient,
) -> Result<(), RunError> {
    let tv = TvDevice::new(tv_client, config.tv_ip);

    match tv.input().current() {
        Ok(current_input) => handle_known_input(writer, config, marker, tv, current_input),
        Err(err) => {
            writeln!(
                writer,
                "LG Buddy Screen Off: Could not query TV input. Falling back to power_off. {err}"
            )?;

            match tv.power().off() {
                Ok(_) => {
                    marker.create()?;
                    writeln!(writer, "LG Buddy Screen Off: Fallback power_off succeeded.")?;
                }
                Err(fallback_err) => {
                    writeln!(
                        writer,
                        "LG Buddy Screen Off: Could not power off the TV (may already be off or unreachable). {fallback_err}"
                    )?;
                }
            }

            Ok(())
        }
    }
}

fn handle_known_input<W: Write, C: TvClient>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv: TvDevice<'_, C>,
    current_input: CurrentInput,
) -> Result<(), RunError> {
    if current_input.is_hdmi(config.input) {
        writeln!(
            writer,
            "LG Buddy Screen Off: TV is on {}. Attempting screen blank for idle...",
            config.input.as_str()
        )?;

        match tv.screen().blank() {
            Ok(_) => {
                marker.create()?;
                writeln!(
                    writer,
                    "LG Buddy Screen Off: Screen blank command succeeded."
                )?;
            }
            Err(err) => {
                writeln!(
                    writer,
                    "LG Buddy Screen Off: Screen blank failed. Falling back to power_off. {err}"
                )?;

                match tv.power().off() {
                    Ok(_) => {
                        marker.create()?;
                        writeln!(writer, "LG Buddy Screen Off: Fallback power_off succeeded.")?;
                    }
                    Err(fallback_err) => {
                        writeln!(
                            writer,
                            "LG Buddy Screen Off: Fallback power_off failed. {fallback_err}"
                        )?;
                    }
                }
            }
        }
    } else {
        marker.clear()?;
        writeln!(
            writer,
            "LG Buddy Screen Off: TV is on {current_input} (not {}). Skipping idle action.",
            config.input.as_str()
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run_screen_off_with;
    use crate::config::{Config, HdmiInput, MacAddress, ScreenBackend};
    use crate::state::ScreenOwnershipMarker;
    use crate::tv::{CommandOutput, TvClient, TvError};
    use std::cell::RefCell;
    use std::fs;
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::UNIX_EPOCH;

    #[test]
    fn matching_input_blanks_screen_and_sets_marker() {
        let temp_dir = TestDir::new("screen-off-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let client = FakeTvClient::new()
            .with_current_input(Ok("com.webos.app.hdmi2".to_string()))
            .with_turn_screen_off(Ok(command_output("blanked\n")));

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off should succeed");

        assert!(marker.exists());
        assert_eq!(client.calls(), vec!["get_input", "turn_screen_off"]);
        assert!(rendered(&output).contains("Screen blank command succeeded."));
    }

    #[test]
    fn matching_input_falls_back_to_power_off() {
        let temp_dir = TestDir::new("screen-off-fallback");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let client = FakeTvClient::new()
            .with_current_input(Ok("com.webos.app.hdmi3".to_string()))
            .with_turn_screen_off(Err(command_failed("turn_screen_off", 1, "blank failed\n")))
            .with_power_off(Ok(command_output("powered off\n")));

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
        )
        .expect("screen-off fallback should succeed");

        assert!(marker.exists());
        assert_eq!(
            client.calls(),
            vec!["get_input", "turn_screen_off", "power_off"]
        );
        let rendered = rendered(&output);
        assert!(rendered.contains("Screen blank failed."));
        assert!(rendered.contains("Fallback power_off succeeded."));
    }

    #[test]
    fn get_input_failure_falls_back_to_power_off() {
        let temp_dir = TestDir::new("screen-off-get-input-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let client = FakeTvClient::new()
            .with_current_input(Err(command_failed("get_input", 1, "unreachable\n")))
            .with_power_off(Ok(command_output("powered off\n")));

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            &client,
        )
        .expect("screen-off fallback should succeed");

        assert!(marker.exists());
        assert_eq!(client.calls(), vec!["get_input", "power_off"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Could not query TV input."));
        assert!(rendered.contains("Fallback power_off succeeded."));
    }

    #[test]
    fn different_input_skips_and_clears_marker() {
        let temp_dir = TestDir::new("screen-off-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create stale marker");
        let client = FakeTvClient::new().with_current_input(Ok("com.webos.app.hdmi4".to_string()));

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off skip should succeed");

        assert!(!marker.exists());
        assert_eq!(client.calls(), vec!["get_input"]);
        assert!(rendered(&output).contains("Skipping idle action."));
    }

    #[test]
    fn failed_fallback_does_not_set_marker() {
        let temp_dir = TestDir::new("screen-off-fallback-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let client = FakeTvClient::new()
            .with_current_input(Ok("com.webos.app.hdmi2".to_string()))
            .with_turn_screen_off(Err(command_failed("turn_screen_off", 1, "blank failed\n")))
            .with_power_off(Err(command_failed("power_off", 1, "power failed\n")));

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off should still return ok");

        assert!(!marker.exists());
        assert_eq!(
            client.calls(),
            vec!["get_input", "turn_screen_off", "power_off"]
        );
        assert!(rendered(&output).contains("Fallback power_off failed."));
    }

    fn sample_config(input: HdmiInput) -> Config {
        Config {
            tv_ip: "192.168.1.42".parse::<Ipv4Addr>().expect("parse ipv4"),
            tv_mac: "aa:bb:cc:dd:ee:ff"
                .parse::<MacAddress>()
                .expect("parse mac"),
            input,
            screen_backend: ScreenBackend::Auto,
            screen_idle_timeout: 300,
        }
    }

    fn command_output(stdout: &str) -> CommandOutput {
        CommandOutput::new(stdout.to_string(), String::new())
    }

    fn command_failed(command: &'static str, status: i32, stderr: &str) -> TvError {
        TvError::CommandFailed {
            command,
            status: Some(status),
            output: CommandOutput::new(String::new(), stderr.to_string()),
        }
    }

    fn rendered(output: &[u8]) -> String {
        String::from_utf8(output.to_vec()).expect("utf8 output")
    }

    struct FakeTvClient {
        current_input: RefCell<Option<Result<String, TvError>>>,
        turn_screen_off: RefCell<Option<Result<CommandOutput, TvError>>>,
        power_off: RefCell<Option<Result<CommandOutput, TvError>>>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl FakeTvClient {
        fn new() -> Self {
            Self {
                current_input: RefCell::new(Some(Err(command_failed(
                    "get_input",
                    1,
                    "unconfigured\n",
                )))),
                turn_screen_off: RefCell::new(Some(Err(command_failed(
                    "turn_screen_off",
                    1,
                    "unconfigured\n",
                )))),
                power_off: RefCell::new(Some(Err(command_failed(
                    "power_off",
                    1,
                    "unconfigured\n",
                )))),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with_current_input(self, value: Result<String, TvError>) -> Self {
            *self.current_input.borrow_mut() = Some(value);
            self
        }

        fn with_turn_screen_off(self, value: Result<CommandOutput, TvError>) -> Self {
            *self.turn_screen_off.borrow_mut() = Some(value);
            self
        }

        fn with_power_off(self, value: Result<CommandOutput, TvError>) -> Self {
            *self.power_off.borrow_mut() = Some(value);
            self
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.borrow().clone()
        }
    }

    impl TvClient for FakeTvClient {
        fn get_input(&self, _tv_ip: Ipv4Addr) -> Result<String, TvError> {
            self.calls.borrow_mut().push("get_input");
            self.current_input
                .borrow_mut()
                .take()
                .expect("configured get_input response")
        }

        fn set_input(&self, _tv_ip: Ipv4Addr, _input: HdmiInput) -> Result<CommandOutput, TvError> {
            self.calls.borrow_mut().push("set_input");
            Err(command_failed("set_input", 1, "unused\n"))
        }

        fn power_off(&self, _tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
            self.calls.borrow_mut().push("power_off");
            self.power_off
                .borrow_mut()
                .take()
                .expect("configured power_off response")
        }

        fn turn_screen_off(&self, _tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
            self.calls.borrow_mut().push("turn_screen_off");
            self.turn_screen_off
                .borrow_mut()
                .take()
                .expect("configured turn_screen_off response")
        }

        fn turn_screen_on(&self, _tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
            self.calls.borrow_mut().push("turn_screen_on");
            Err(command_failed("turn_screen_on", 1, "unused\n"))
        }
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);

            let unique = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let timestamp = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-{label}-{}-{timestamp}-{unique}",
                process::id()
            ));

            fs::create_dir_all(&path).expect("create test temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
