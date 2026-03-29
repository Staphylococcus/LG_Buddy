use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, SystemTime};

use crate::config::{load_config, resolve_config_path_from_env, Config};
use crate::state::{ScreenOwnershipMarker, StateScope};
use crate::tv::{BscpylgtvCommandClient, CurrentInput, TvClient, TvDevice};
use crate::wol::{UdpWakeOnLanSender, WakeOnLanSender};
use crate::{RunError, StartupMode};

const SCREEN_ON_STALE_MARKER_MAX_AGE: Duration = Duration::from_secs(12 * 60 * 60);
const SCREEN_ON_INITIAL_WAKE_DELAY: Duration = Duration::from_secs(6);
const SCREEN_ON_WAKE_ATTEMPTS: u32 = 6;
const STARTUP_INITIAL_WAKE_DELAY: Duration = Duration::from_secs(6);
const STARTUP_WAKE_ATTEMPTS: u32 = 6;
const SYSTEM_SLEEP_GET_INPUT_RETRIES: u32 = 3;
const SYSTEM_SLEEP_POWER_OFF_RETRIES: u32 = 4;

trait Sleeper {
    fn sleep(&self, duration: Duration);
}

struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

trait RebootDetector {
    fn is_reboot_pending(&self) -> io::Result<bool>;
}

trait SleepRequestDetector {
    fn is_sleep_requested(&self) -> io::Result<bool>;
}

struct SystemctlRebootDetector {
    command_path: PathBuf,
}

struct JournalctlSleepDetector {
    command_path: PathBuf,
}

impl Default for SystemctlRebootDetector {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for JournalctlSleepDetector {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SystemctlRebootDetector {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_SYSTEMCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("systemctl")),
        }
    }
}

impl JournalctlSleepDetector {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_JOURNALCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("journalctl")),
        }
    }
}

impl RebootDetector for SystemctlRebootDetector {
    fn is_reboot_pending(&self) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .arg("list-jobs")
            .output()?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .any(|line| line.contains("reboot.target") && line.contains("start")))
    }
}

impl SleepRequestDetector for JournalctlSleepDetector {
    fn is_sleep_requested(&self) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .args(["-u", "NetworkManager", "-n", "30", "--no-pager"])
            .output()?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains("manager: sleep: sleep requested"))
    }
}

pub fn run_screen_off<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker =
        ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
    let tv_client = BscpylgtvCommandClient::from_env();

    run_screen_off_with(writer, &config, &marker, &tv_client)
}

pub fn run_sleep_pre<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = BscpylgtvCommandClient::from_env();
    let sleeper = ThreadSleeper;

    run_sleep_pre_with(writer, &config, &marker, &tv_client, &sleeper)
}

pub fn run_sleep<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = BscpylgtvCommandClient::from_env();
    let detector = JournalctlSleepDetector::default();
    let sleeper = ThreadSleeper;

    run_sleep_with(writer, &config, &marker, &tv_client, &detector, &sleeper)
}

pub fn run_startup<W: Write>(writer: &mut W, mode: StartupMode) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = BscpylgtvCommandClient::from_env();
    let wol_sender = UdpWakeOnLanSender::default();
    let sleeper = ThreadSleeper;

    run_startup_with(
        writer,
        &config,
        &marker,
        &tv_client,
        &wol_sender,
        &sleeper,
        mode,
    )
}

pub fn run_shutdown<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let tv_client = BscpylgtvCommandClient::from_env();
    let reboot_detector = SystemctlRebootDetector::default();

    run_shutdown_with(writer, &config, &tv_client, &reboot_detector)
}

pub fn run_screen_on<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker =
        ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
    let tv_client = BscpylgtvCommandClient::from_env();
    let wol_sender = UdpWakeOnLanSender::default();
    let sleeper = ThreadSleeper;

    run_screen_on_with(
        writer,
        &config,
        &marker,
        &tv_client,
        &wol_sender,
        &sleeper,
        SystemTime::now(),
    )
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

fn run_startup_with<W: Write, C: TvClient, S: WakeOnLanSender, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    wol_sender: &S,
    sleeper: &Sl,
    mode: StartupMode,
) -> Result<(), RunError> {
    let tv = TvDevice::new(tv_client, config.tv_ip);

    match mode {
        StartupMode::Wake if !marker.exists() => {
            writeln!(
                writer,
                "LG Buddy Startup: Wake from sleep: TV was not on our input. Skipping."
            )?;
            return Ok(());
        }
        StartupMode::Wake => {
            writeln!(
                writer,
                "LG Buddy Startup: Wake from sleep: LG Buddy turned TV off. Restoring."
            )?;
        }
        StartupMode::Boot => {
            writeln!(
                writer,
                "LG Buddy Startup: Cold boot: Turning TV on and switching to {}.",
                config.input.as_str()
            )?;
        }
        StartupMode::Auto if marker.exists() => {
            writeln!(
                writer,
                "LG Buddy Startup: Wake from sleep: LG Buddy turned TV off. Restoring."
            )?;
        }
        StartupMode::Auto => {
            writeln!(
                writer,
                "LG Buddy Startup: Cold boot: Turning TV on and switching to {}.",
                config.input.as_str()
            )?;
        }
    }

    marker.clear()?;
    send_wake_packet(writer, "LG Buddy Startup", &tv, wol_sender, &config.tv_mac)?;
    sleeper.sleep(startup_initial_wake_delay());

    for attempt in 1..=STARTUP_WAKE_ATTEMPTS {
        if tv.input().set(config.input).is_ok() {
            writeln!(
                writer,
                "LG Buddy Startup: TV turned on and set to {}.",
                config.input.as_str()
            )?;
            return Ok(());
        }

        let retry_delay = startup_retry_delay(attempt);
        writeln!(
            writer,
            "LG Buddy Startup: Attempt {attempt} failed, retrying in {}s...",
            retry_delay.as_secs()
        )?;
        send_wake_packet(writer, "LG Buddy Startup", &tv, wol_sender, &config.tv_mac)?;
        sleeper.sleep(retry_delay);
    }

    writeln!(
        writer,
        "LG Buddy Startup: set_input failed after {STARTUP_WAKE_ATTEMPTS} attempts"
    )?;
    Err(RunError::Policy(format!(
        "startup set_input failed after {STARTUP_WAKE_ATTEMPTS} attempts"
    )))
}

fn run_sleep_pre_with<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    sleeper: &Sl,
) -> Result<(), RunError> {
    let tv = TvDevice::new(tv_client, config.tv_ip);
    let state_was_set = marker.exists();

    match query_current_input_with_retries(&tv, sleeper, SYSTEM_SLEEP_GET_INPUT_RETRIES) {
        Ok(current_input) if current_input.is_hdmi(config.input) => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: TV is on {}. Turning off for sleep.",
                config.input.as_str()
            )?;

            if tv.power().off().is_ok() {
                marker.create()?;
            } else {
                writeln!(
                    writer,
                    "LG Buddy Sleep Pre: power_off failed on known input. State not set."
                )?;
            }
        }
        Ok(current_input) => {
            marker.clear()?;
            writeln!(
                writer,
                "LG Buddy Sleep Pre: TV is on {current_input} (not {}). Skipping.",
                config.input.as_str()
            )?;
        }
        Err(_) => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: Could not query TV input. Attempting power_off fallback."
            )?;

            if retry_power_off(&tv, sleeper, SYSTEM_SLEEP_POWER_OFF_RETRIES) {
                marker.create()?;
            } else if state_was_set {
                writeln!(
                    writer,
                    "LG Buddy Sleep Pre: Fallback power_off failed, but state already set by another hook. Keeping state."
                )?;
            } else {
                marker.clear()?;
                writeln!(
                    writer,
                    "LG Buddy Sleep Pre: Fallback power_off failed after retries. Leaving state unset."
                )?;
            }
        }
    }

    Ok(())
}

fn run_shutdown_with<W: Write, C: TvClient, R: RebootDetector>(
    writer: &mut W,
    config: &Config,
    tv_client: &C,
    reboot_detector: &R,
) -> Result<(), RunError> {
    match reboot_detector.is_reboot_pending() {
        Ok(true) => {
            writeln!(writer, "LG Buddy Shutdown: Reboot; ignoring")?;
            return Ok(());
        }
        Ok(false) => {}
        Err(err) => {
            writeln!(
                writer,
                "LG Buddy Shutdown: Could not determine reboot state. Continuing shutdown. {err}"
            )?;
        }
    }

    let tv = TvDevice::new(tv_client, config.tv_ip);

    match tv.input().current() {
        Ok(current_input) if current_input.is_hdmi(config.input) => {
            writeln!(
                writer,
                "LG Buddy Shutdown: TV is on {}. Turning off for shutdown.",
                config.input.as_str()
            )?;
            log_shutdown_power_off_failure(writer, tv.power().off())?;
        }
        Ok(current_input) => {
            writeln!(
                writer,
                "LG Buddy Shutdown: TV is on {current_input} (not {}). Skipping.",
                config.input.as_str()
            )?;
        }
        Err(_) => {
            writeln!(
                writer,
                "LG Buddy Shutdown: Could not query TV input. Proceeding with power_off."
            )?;
            log_shutdown_power_off_failure(writer, tv.power().off())?;
        }
    }

    Ok(())
}

fn run_sleep_with<W: Write, C: TvClient, D: SleepRequestDetector, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    detector: &D,
    sleeper: &Sl,
) -> Result<(), RunError> {
    match detector.is_sleep_requested() {
        Ok(false) => return Ok(()),
        Ok(true) => {}
        Err(err) => {
            writeln!(
                writer,
                "LG Buddy Sleep: Could not determine NetworkManager sleep state. Skipping. {err}"
            )?;
            return Ok(());
        }
    }

    let tv = TvDevice::new(tv_client, config.tv_ip);

    match query_current_input_with_retries(&tv, sleeper, SYSTEM_SLEEP_GET_INPUT_RETRIES) {
        Ok(current_input) if !current_input.is_hdmi(config.input) => {
            marker.clear()?;
            return Ok(());
        }
        Ok(_) | Err(_) => {}
    }

    let state_was_set = marker.exists();

    if retry_power_off(&tv, sleeper, SYSTEM_SLEEP_POWER_OFF_RETRIES) {
        marker.create()?;
    } else if !state_was_set {
        marker.clear()?;
    }

    Ok(())
}

fn run_screen_on_with<W: Write, C: TvClient, S: WakeOnLanSender, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    wol_sender: &S,
    sleeper: &Sl,
    now: SystemTime,
) -> Result<(), RunError> {
    if !marker.exists() {
        writeln!(
            writer,
            "LG Buddy Screen On: State file not found. TV was not turned off by LG Buddy. Skipping wake."
        )?;
        return Ok(());
    }

    if marker
        .is_stale(SCREEN_ON_STALE_MARKER_MAX_AGE, now)
        .map_err(RunError::Io)?
    {
        writeln!(
            writer,
            "LG Buddy Screen On: State file is stale (>12h old). Removing and skipping wake."
        )?;
        marker.clear()?;
        return Ok(());
    }

    let tv = TvDevice::new(tv_client, config.tv_ip);

    writeln!(
        writer,
        "LG Buddy Screen On: Turning TV on (screen wake) using input {}...",
        config.input.as_str()
    )?;
    writeln!(writer, "LG Buddy Screen On: Attempting screen unblank...")?;

    match tv.screen().unblank() {
        Ok(_) => {
            writeln!(
                writer,
                "LG Buddy Screen On: Screen unblank succeeded. Clearing wake state."
            )?;
            marker.clear()?;
            return Ok(());
        }
        Err(err) if err.indicates_active_screen_state() => {
            writeln!(
                writer,
                "LG Buddy Screen On: TV reported an active screen state. Trying immediate input restore before full wake."
            )?;

            if tv.input().set(config.input).is_ok() {
                writeln!(
                    writer,
                    "LG Buddy Screen On: Immediate input restore succeeded. Clearing wake state."
                )?;
                marker.clear()?;
                return Ok(());
            }
        }
        Err(_) => {}
    }

    writeln!(
        writer,
        "LG Buddy Screen On: Screen unblank failed. Falling back to full wake."
    )?;
    writeln!(
        writer,
        "LG Buddy Screen On: Sending initial Wake-on-LAN packet..."
    )?;
    send_wake_packet(
        writer,
        "LG Buddy Screen On",
        &tv,
        wol_sender,
        &config.tv_mac,
    )?;
    sleeper.sleep(screen_on_initial_wake_delay());

    for attempt in 1..=SCREEN_ON_WAKE_ATTEMPTS {
        writeln!(
            writer,
            "LG Buddy Screen On: Wake attempt {attempt}: setting input to {}...",
            config.input.as_str()
        )?;

        if tv.input().set(config.input).is_ok() {
            writeln!(
                writer,
                "LG Buddy Screen On: Wake attempt {attempt} succeeded. Clearing wake state."
            )?;
            marker.clear()?;
            return Ok(());
        }

        let retry_delay = screen_on_retry_delay(attempt);
        writeln!(
            writer,
            "LG Buddy Screen On: Wake attempt {attempt} failed. Resending WoL and retrying in {}s...",
            retry_delay.as_secs()
        )?;
        send_wake_packet(
            writer,
            "LG Buddy Screen On",
            &tv,
            wol_sender,
            &config.tv_mac,
        )?;
        sleeper.sleep(retry_delay);
    }

    writeln!(
        writer,
        "LG Buddy Screen On: Wake failed after {SCREEN_ON_WAKE_ATTEMPTS} attempts. Leaving state file in place for another resume event."
    )?;
    Err(RunError::Policy(format!(
        "screen-on wake sequence failed after {SCREEN_ON_WAKE_ATTEMPTS} attempts"
    )))
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

fn log_shutdown_power_off_failure<W: Write>(
    writer: &mut W,
    result: Result<crate::tv::CommandOutput, crate::tv::TvError>,
) -> Result<(), RunError> {
    if let Err(err) = result {
        writeln!(
            writer,
            "LG Buddy Shutdown: power_off failed, continuing shutdown. {err}"
        )?;
    }

    Ok(())
}

fn send_wake_packet<W: Write, C: TvClient, S: WakeOnLanSender>(
    writer: &mut W,
    prefix: &str,
    tv: &TvDevice<'_, C>,
    wol_sender: &S,
    tv_mac: &crate::config::MacAddress,
) -> Result<(), RunError> {
    if let Err(err) = tv.power().wake(wol_sender, tv_mac) {
        writeln!(
            writer,
            "{prefix}: Wake-on-LAN send failed. Continuing anyway. {err}"
        )?;
    }

    Ok(())
}

fn query_current_input_with_retries<C: TvClient, Sl: Sleeper>(
    tv: &TvDevice<'_, C>,
    sleeper: &Sl,
    retries: u32,
) -> Result<CurrentInput, crate::tv::TvError> {
    let mut last_err = None;

    for attempt in 0..=retries {
        match tv.input().current() {
            Ok(current_input) => return Ok(current_input),
            Err(err) => {
                last_err = Some(err);
                if attempt < retries {
                    sleeper.sleep(system_sleep_retry_delay());
                }
            }
        }
    }

    Err(last_err.expect("retry loop should capture a tv error"))
}

fn retry_power_off<C: TvClient, Sl: Sleeper>(
    tv: &TvDevice<'_, C>,
    sleeper: &Sl,
    attempts: u32,
) -> bool {
    for attempt in 1..=attempts {
        if tv.power().off().is_ok() {
            return true;
        }

        if attempt < attempts {
            sleeper.sleep(system_sleep_retry_delay());
        }
    }

    false
}

fn duration_override_secs(env_key: &str, default: Duration) -> Duration {
    env::var(env_key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
}

fn screen_on_initial_wake_delay() -> Duration {
    duration_override_secs(
        "LG_BUDDY_SCREEN_ON_INITIAL_WAKE_DELAY_SECS",
        SCREEN_ON_INITIAL_WAKE_DELAY,
    )
}

fn startup_initial_wake_delay() -> Duration {
    duration_override_secs(
        "LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS",
        STARTUP_INITIAL_WAKE_DELAY,
    )
}

fn screen_on_retry_delay(attempt: u32) -> Duration {
    duration_override_secs(
        "LG_BUDDY_SCREEN_ON_RETRY_DELAY_SECS",
        Duration::from_secs(u64::from((attempt * 2).min(30))),
    )
}

fn startup_retry_delay(attempt: u32) -> Duration {
    duration_override_secs(
        "LG_BUDDY_STARTUP_RETRY_DELAY_SECS",
        Duration::from_secs(u64::from((attempt * 2).min(30))),
    )
}

fn system_sleep_retry_delay() -> Duration {
    duration_override_secs("LG_BUDDY_SLEEP_RETRY_DELAY_SECS", Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{
        run_screen_off_with, run_screen_on_with, run_shutdown_with, run_sleep_pre_with,
        run_sleep_with, run_startup_with, RebootDetector, SleepRequestDetector, Sleeper,
    };
    use crate::config::{Config, HdmiInput, MacAddress, ScreenBackend};
    use crate::state::ScreenOwnershipMarker;
    use crate::tv::BscpylgtvCommandClient;
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use crate::StartupMode;
    use std::cell::RefCell;
    use std::fs;
    use std::io;
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use support::MockBscpylgtv;

    #[test]
    fn matching_input_blanks_screen_and_sets_marker() {
        let temp_dir = TestDir::new("screen-off-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-success-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off"]);
        assert!(rendered(&output).contains("Screen blank command succeeded."));
    }

    #[test]
    fn matching_input_falls_back_to_power_off() {
        let temp_dir = TestDir::new("screen-off-fallback");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-fallback-tv");
        mock.set_input("HDMI_3");
        mock.queue_error("turn_screen_off", 1, "blank failed\n");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
        )
        .expect("screen-off fallback should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off", "power_off"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Screen blank failed."));
        assert!(rendered.contains("Fallback power_off succeeded."));
    }

    #[test]
    fn get_input_failure_falls_back_to_power_off() {
        let temp_dir = TestDir::new("screen-off-get-input-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-get-input-failure-tv");
        mock.queue_error("get_input", 1, "unreachable\n");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            &client,
        )
        .expect("screen-off fallback should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Could not query TV input."));
        assert!(rendered.contains("Fallback power_off succeeded."));
    }

    #[test]
    fn different_input_skips_and_clears_marker() {
        let temp_dir = TestDir::new("screen-off-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create stale marker");
        let mock = MockBscpylgtv::new("screen-off-skip-tv");
        mock.set_input("HDMI_4");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off skip should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).contains("Skipping idle action."));
    }

    #[test]
    fn failed_fallback_does_not_set_marker() {
        let temp_dir = TestDir::new("screen-off-fallback-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-off-fallback-failure-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("turn_screen_off", 1, "blank failed\n");
        mock.queue_error("power_off", 1, "power failed\n");
        let client = client_for_mock(&mock);

        let mut output = Vec::new();
        run_screen_off_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
        )
        .expect("screen-off should still return ok");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input", "turn_screen_off", "power_off"]);
        assert!(rendered(&output).contains("Fallback power_off failed."));
    }

    #[test]
    fn screen_on_skips_when_marker_is_missing() {
        let temp_dir = TestDir::new("screen-on-no-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-on-no-marker-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
            SystemTime::now(),
        )
        .expect("missing marker should skip");

        assert_call_commands(&mock, &[]);
        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("State file not found."));
    }

    #[test]
    fn screen_on_removes_stale_marker_and_skips() {
        let temp_dir = TestDir::new("screen-on-stale-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-stale-marker-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
            SystemTime::now() + Duration::from_secs((12 * 60 * 60) + 1),
        )
        .expect("stale marker should skip");

        assert!(!marker.exists());
        assert_call_commands(&mock, &[]);
        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("State file is stale (>12h old)."));
    }

    #[test]
    fn screen_on_unblanks_and_clears_marker() {
        let temp_dir = TestDir::new("screen-on-unblank");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-unblank-tv");
        mock.set_screen_on(false);
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            &client,
            &wol,
            &sleeper,
            SystemTime::now(),
        )
        .expect("turn_screen_on should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["turn_screen_on"]);
        assert!(wol.calls().is_empty());
        assert!(rendered(&output).contains("Screen unblank succeeded."));
    }

    #[test]
    fn screen_on_restores_input_when_screen_is_already_active() {
        let temp_dir = TestDir::new("screen-on-already-active");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-already-active-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
            &wol,
            &sleeper,
            SystemTime::now(),
        )
        .expect("already-active path should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["turn_screen_on", "set_input"]);
        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        let rendered = rendered(&output);
        assert!(rendered.contains("TV reported an active screen state."));
        assert!(rendered.contains("Immediate input restore succeeded."));
    }

    #[test]
    fn screen_on_falls_back_to_wake_and_retries_until_success() {
        let temp_dir = TestDir::new("screen-on-wake-retry-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-wake-retry-success-tv");
        mock.queue_error("turn_screen_on", 1, "offline\n");
        mock.queue_error("set_input", 1, "not ready\n");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &wol,
            &sleeper,
            SystemTime::now(),
        )
        .expect("wake retry should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["turn_screen_on", "set_input", "set_input"]);
        assert_eq!(wol.calls().len(), 2);
        assert_eq!(
            sleeper.durations(),
            vec![Duration::from_secs(6), Duration::from_secs(2)]
        );
        let rendered = rendered(&output);
        assert!(rendered.contains("Sending initial Wake-on-LAN packet"));
        assert!(rendered.contains("Wake attempt 1 failed."));
        assert!(rendered.contains("Wake attempt 2 succeeded."));
    }

    #[test]
    fn screen_on_returns_error_and_preserves_marker_after_exhausting_retries() {
        let temp_dir = TestDir::new("screen-on-wake-retry-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("screen-on-wake-retry-failure-tv");
        mock.queue_error("turn_screen_on", 1, "offline\n");
        for _ in 0..6 {
            mock.queue_error("set_input", 1, "not ready\n");
        }
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let err = run_screen_on_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
            SystemTime::now(),
        )
        .expect_err("exhausted retries should fail");

        assert!(marker.exists());
        assert_eq!(mock.calls().len(), 7);
        assert_eq!(wol.calls().len(), 7);
        assert_eq!(sleeper.durations().len(), 7);
        assert!(matches!(err, crate::RunError::Policy(_)));
        assert!(rendered(&output).contains("Wake failed after 6 attempts."));
    }

    #[test]
    fn startup_wake_mode_skips_without_system_marker() {
        let temp_dir = TestDir::new("startup-wake-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-wake-skip-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
            StartupMode::Wake,
        )
        .expect("wake mode without marker should skip");

        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("TV was not on our input. Skipping."));
    }

    #[test]
    fn startup_auto_mode_treats_missing_marker_as_boot() {
        let temp_dir = TestDir::new("startup-auto-boot");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-auto-boot-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &wol,
            &sleeper,
            StartupMode::Auto,
        )
        .expect("auto boot should succeed");

        assert!(!marker.exists());
        assert_eq!(wol.calls().len(), 1);
        assert_eq!(sleeper.durations(), vec![Duration::from_secs(6)]);
        assert_call_commands(&mock, &["set_input"]);
        let rendered = rendered(&output);
        assert!(rendered.contains("Cold boot: Turning TV on and switching to HDMI_4."));
        assert!(rendered.contains("TV turned on and set to HDMI_4."));
    }

    #[test]
    fn startup_auto_mode_restores_when_system_marker_exists() {
        let temp_dir = TestDir::new("startup-auto-wake");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("startup-auto-wake-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            &client,
            &wol,
            &sleeper,
            StartupMode::Auto,
        )
        .expect("auto wake should succeed");

        assert!(!marker.exists());
        assert_eq!(wol.calls().len(), 1);
        assert_call_commands(&mock, &["set_input"]);
        assert!(rendered(&output).contains("Wake from sleep: LG Buddy turned TV off. Restoring."));
    }

    #[test]
    fn startup_boot_mode_clears_existing_marker_before_restore() {
        let temp_dir = TestDir::new("startup-boot-clears-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create stale system marker");
        let mock = MockBscpylgtv::new("startup-boot-clears-marker-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
            &wol,
            &sleeper,
            StartupMode::Boot,
        )
        .expect("boot mode should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["set_input"]);
        assert!(rendered(&output).contains("Cold boot: Turning TV on and switching to HDMI_3."));
    }

    #[test]
    fn startup_retries_until_set_input_succeeds() {
        let temp_dir = TestDir::new("startup-retry-success");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-retry-success-tv");
        mock.queue_error("set_input", 1, "not ready\n");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &wol,
            &sleeper,
            StartupMode::Boot,
        )
        .expect("startup retry should succeed");

        assert_call_commands(&mock, &["set_input", "set_input"]);
        assert_eq!(wol.calls().len(), 2);
        assert_eq!(
            sleeper.durations(),
            vec![Duration::from_secs(6), Duration::from_secs(2)]
        );
        let rendered = rendered(&output);
        assert!(rendered.contains("Attempt 1 failed, retrying in 2s..."));
        assert!(rendered.contains("TV turned on and set to HDMI_2."));
    }

    #[test]
    fn startup_returns_error_after_exhausting_retries_and_leaves_marker_cleared() {
        let temp_dir = TestDir::new("startup-retry-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("startup-retry-failure-tv");
        for _ in 0..6 {
            mock.queue_error("set_input", 1, "not ready\n");
        }
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let err = run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            &client,
            &wol,
            &sleeper,
            StartupMode::Auto,
        )
        .expect_err("startup should fail after exhausting retries");

        assert!(!marker.exists());
        assert_eq!(mock.calls().len(), 6);
        assert_eq!(wol.calls().len(), 7);
        assert_eq!(sleeper.durations().len(), 7);
        assert!(matches!(err, crate::RunError::Policy(_)));
        assert!(rendered(&output).contains("set_input failed after 6 attempts"));
    }

    #[test]
    fn shutdown_ignores_reboot() {
        let mock = MockBscpylgtv::new("shutdown-ignores-reboot-tv");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::pending();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &client,
            &detector,
        )
        .expect("reboot should be ignored");

        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("Reboot; ignoring"));
    }

    #[test]
    fn shutdown_powers_off_when_configured_input_is_active() {
        let mock = MockBscpylgtv::new("shutdown-match-tv");
        mock.set_input("HDMI_3");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::clear();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &client,
            &detector,
        )
        .expect("matching input should power off");

        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("TV is on HDMI_3. Turning off for shutdown."));
    }

    #[test]
    fn shutdown_skips_when_tv_is_on_different_input() {
        let mock = MockBscpylgtv::new("shutdown-skip-tv");
        mock.set_input("HDMI_1");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::clear();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &client,
            &detector,
        )
        .expect("nonmatching input should skip");

        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).contains("TV is on HDMI_1 (not HDMI_4). Skipping."));
    }

    #[test]
    fn shutdown_falls_back_to_power_off_when_input_query_fails() {
        let mock = MockBscpylgtv::new("shutdown-fallback-tv");
        mock.queue_error("get_input", 1, "offline\n");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::clear();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &client,
            &detector,
        )
        .expect("query failure should still power off");

        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("Could not query TV input. Proceeding with power_off."));
    }

    #[test]
    fn shutdown_logs_power_off_failure_but_does_not_error() {
        let mock = MockBscpylgtv::new("shutdown-power-off-failure-tv");
        mock.set_input("HDMI_2");
        mock.queue_error("power_off", 1, "already off\n");
        let client = client_for_mock(&mock);
        let detector = FakeRebootDetector::clear();

        let mut output = Vec::new();
        run_shutdown_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &client,
            &detector,
        )
        .expect("power_off failure should not abort shutdown");

        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("power_off failed, continuing shutdown."));
    }

    #[test]
    fn sleep_pre_powers_off_and_sets_system_marker_on_matching_input() {
        let temp_dir = TestDir::new("sleep-pre-match");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("sleep-pre-match-tv");
        mock.set_input("HDMI_3");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_sleep_pre_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
            &sleeper,
        )
        .expect("sleep-pre should succeed");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("Turning off for sleep."));
    }

    #[test]
    fn sleep_pre_skips_and_clears_marker_on_different_input() {
        let temp_dir = TestDir::new("sleep-pre-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("sleep-pre-skip-tv");
        mock.set_input("HDMI_1");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_sleep_pre_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &sleeper,
        )
        .expect("sleep-pre skip should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).contains("Skipping."));
    }

    #[test]
    fn sleep_pre_falls_back_to_power_off_when_input_query_keeps_failing() {
        let temp_dir = TestDir::new("sleep-pre-fallback");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("sleep-pre-fallback-tv");
        for _ in 0..4 {
            mock.queue_error("get_input", 1, "offline\n");
        }
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_sleep_pre_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &sleeper,
        )
        .expect("sleep-pre fallback should succeed");

        assert!(marker.exists());
        assert_call_commands(
            &mock,
            &[
                "get_input",
                "get_input",
                "get_input",
                "get_input",
                "power_off",
            ],
        );
        assert_eq!(
            sleeper.durations(),
            vec![
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1)
            ]
        );
        assert!(rendered(&output).contains("Attempting power_off fallback."));
    }

    #[test]
    fn sleep_skips_when_networkmanager_is_not_entering_sleep() {
        let temp_dir = TestDir::new("sleep-noop");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("sleep-noop-tv");
        let client = client_for_mock(&mock);
        let detector = FakeSleepRequestDetector::clear();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_sleep_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &detector,
            &sleeper,
        )
        .expect("sleep noop should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).is_empty());
    }

    #[test]
    fn sleep_powers_off_and_sets_marker_when_sleep_is_requested() {
        let temp_dir = TestDir::new("sleep-match");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("sleep-match-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let detector = FakeSleepRequestDetector::pending();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_sleep_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &client,
            &detector,
            &sleeper,
        )
        .expect("sleep should power off");

        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    #[test]
    fn sleep_skips_when_tv_is_on_different_input() {
        let temp_dir = TestDir::new("sleep-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        let mock = MockBscpylgtv::new("sleep-skip-tv");
        mock.set_input("HDMI_1");
        let client = client_for_mock(&mock);
        let detector = FakeSleepRequestDetector::pending();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_sleep_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &detector,
            &sleeper,
        )
        .expect("sleep skip should succeed");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["get_input"]);
        assert!(rendered(&output).is_empty());
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

    fn rendered(output: &[u8]) -> String {
        String::from_utf8(output.to_vec()).expect("utf8 output")
    }

    fn client_for_mock(mock: &MockBscpylgtv) -> BscpylgtvCommandClient {
        BscpylgtvCommandClient::with_args(mock.command_path(), mock.command_args())
    }

    fn assert_call_commands(mock: &MockBscpylgtv, expected: &[&str]) {
        let actual = mock
            .calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>();
        let expected = expected
            .iter()
            .map(|command| command.to_string())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
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

    #[derive(Default)]
    struct RecordingSleeper {
        durations: RefCell<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn durations(&self) -> Vec<Duration> {
            self.durations.borrow().clone()
        }
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.durations.borrow_mut().push(duration);
        }
    }

    struct FakeRebootDetector {
        pending: io::Result<bool>,
    }

    impl FakeRebootDetector {
        fn clear() -> Self {
            Self { pending: Ok(false) }
        }

        fn pending() -> Self {
            Self { pending: Ok(true) }
        }
    }

    impl RebootDetector for FakeRebootDetector {
        fn is_reboot_pending(&self) -> io::Result<bool> {
            match &self.pending {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    struct FakeSleepRequestDetector {
        requested: io::Result<bool>,
    }

    impl FakeSleepRequestDetector {
        fn clear() -> Self {
            Self {
                requested: Ok(false),
            }
        }

        fn pending() -> Self {
            Self {
                requested: Ok(true),
            }
        }
    }

    impl SleepRequestDetector for FakeSleepRequestDetector {
        fn is_sleep_requested(&self) -> io::Result<bool> {
            match &self.requested {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
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
