use std::env;
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::Duration;

use crate::config::{load_config, resolve_config_path_from_env, Config, ScreenRestorePolicy};
use crate::state::{ScreenOwnershipMarker, StateScope};
use crate::tv::{BscpylgtvCommandClient, CurrentInput, TvClient, TvDevice};
use crate::wol::{UdpWakeOnLanSender, WakeOnLanSender};
use crate::{RunError, StartupMode};

const SCREEN_ON_INITIAL_WAKE_DELAY: Duration = Duration::from_secs(6);
const SCREEN_ON_WAKE_ATTEMPTS: u32 = 6;
const STARTUP_INITIAL_WAKE_DELAY: Duration = Duration::from_secs(6);
const STARTUP_WAKE_ATTEMPTS: u32 = 6;
const SYSTEM_SLEEP_GET_INPUT_RETRIES: u32 = 3;
const SYSTEM_SLEEP_POWER_OFF_RETRIES: u32 = 4;
const BRIGHTNESS_DEFAULT_VALUE: u8 = 50;

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

trait NetworkWaiter {
    fn wait_for_network(&self) -> io::Result<()>;
}

trait ReachabilityChecker {
    fn is_reachable(&self, tv_ip: Ipv4Addr) -> io::Result<bool>;
}

trait BrightnessUi {
    fn prompt_brightness(&self, initial: u8) -> io::Result<Option<u8>>;
    fn show_error(&self, title: &str, message: &str) -> io::Result<()>;
}

trait Notifier {
    fn notify(&self, title: &str, message: &str) -> io::Result<()>;
}

struct SystemctlRebootDetector {
    command_path: PathBuf,
}

struct JournalctlSleepDetector {
    command_path: PathBuf,
}

struct NmOnlineNetworkWaiter {
    command_path: PathBuf,
}

struct PingReachabilityChecker {
    command_path: PathBuf,
}

struct ZenityBrightnessUi {
    command_path: PathBuf,
}

struct NotifySendNotifier {
    command_path: PathBuf,
}

struct StartupDeps<'a, C, S, Sl, N> {
    tv_client: &'a C,
    wol_sender: &'a S,
    sleeper: &'a Sl,
    network_waiter: &'a N,
}

struct BrightnessDeps<'a, C, R, U, N> {
    tv_client: &'a C,
    reachability: &'a R,
    ui: &'a U,
    notifier: &'a N,
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

impl Default for NmOnlineNetworkWaiter {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for PingReachabilityChecker {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for ZenityBrightnessUi {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for NotifySendNotifier {
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

impl NmOnlineNetworkWaiter {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_NM_ONLINE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("nm-online")),
        }
    }
}

impl PingReachabilityChecker {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_PING")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("ping")),
        }
    }
}

impl ZenityBrightnessUi {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_ZENITY")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("zenity")),
        }
    }
}

impl NotifySendNotifier {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_NOTIFY_SEND")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("notify-send")),
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

impl NetworkWaiter for NmOnlineNetworkWaiter {
    fn wait_for_network(&self) -> io::Result<()> {
        let _ = ProcessCommand::new(&self.command_path)
            .args(["-q", "-t", "60"])
            .output()?;

        Ok(())
    }
}

impl ReachabilityChecker for PingReachabilityChecker {
    fn is_reachable(&self, tv_ip: Ipv4Addr) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .args(["-c", "1", "-W", "2"])
            .arg(tv_ip.to_string())
            .output()?;

        Ok(output.status.success())
    }
}

impl BrightnessUi for ZenityBrightnessUi {
    fn prompt_brightness(&self, initial: u8) -> io::Result<Option<u8>> {
        let output = ProcessCommand::new(&self.command_path)
            .args([
                "--scale",
                "--title=LG TV Brightness",
                "--text=Set OLED Pixel Brightness:",
                "--min-value=0",
                "--max-value=100",
                &format!("--value={initial}"),
                "--step=5",
            ])
            .output()?;

        if !output.status.success() {
            return Ok(None);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let value = stdout.trim().parse::<u8>().map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid zenity brightness value `{}`: {err}", stdout.trim()),
            )
        })?;

        if value > 100 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("zenity brightness value out of range: {value}"),
            ));
        }

        Ok(Some(value))
    }

    fn show_error(&self, title: &str, message: &str) -> io::Result<()> {
        let _ = ProcessCommand::new(&self.command_path)
            .arg("--error")
            .arg(format!("--title={title}"))
            .arg(format!("--text={message}"))
            .output()?;

        Ok(())
    }
}

impl Notifier for NotifySendNotifier {
    fn notify(&self, title: &str, message: &str) -> io::Result<()> {
        let _ = ProcessCommand::new(&self.command_path)
            .arg(title)
            .arg(message)
            .output()?;

        Ok(())
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

pub fn run_brightness<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let tv_client = BscpylgtvCommandClient::from_env();
    let reachability = PingReachabilityChecker::default();
    let ui = ZenityBrightnessUi::default();
    let notifier = NotifySendNotifier::default();
    let deps = BrightnessDeps {
        tv_client: &tv_client,
        reachability: &reachability,
        ui: &ui,
        notifier: &notifier,
    };

    run_brightness_with(writer, &config, deps)
}

pub fn run_startup<W: Write>(writer: &mut W, mode: StartupMode) -> Result<(), RunError> {
    let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
    let config = load_config(&config_path).map_err(RunError::Config)?;
    let marker = ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
    let tv_client = BscpylgtvCommandClient::from_env();
    let wol_sender = UdpWakeOnLanSender::default();
    let sleeper = ThreadSleeper;
    let network_waiter = NmOnlineNetworkWaiter::default();
    let deps = StartupDeps {
        tv_client: &tv_client,
        wol_sender: &wol_sender,
        sleeper: &sleeper,
        network_waiter: &network_waiter,
    };

    run_startup_with(writer, &config, &marker, deps, mode)
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

    run_screen_on_with(writer, &config, &marker, &tv_client, &wol_sender, &sleeper)
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

fn run_startup_with<W: Write, C: TvClient, S: WakeOnLanSender, Sl: Sleeper, N: NetworkWaiter>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    deps: StartupDeps<'_, C, S, Sl, N>,
    mode: StartupMode,
) -> Result<(), RunError> {
    let tv = TvDevice::new(deps.tv_client, config.tv_ip);
    let _ = deps.network_waiter.wait_for_network();

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
    send_wake_packet(
        writer,
        "LG Buddy Startup",
        &tv,
        deps.wol_sender,
        &config.tv_mac,
    )?;
    deps.sleeper.sleep(startup_initial_wake_delay());

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
        send_wake_packet(
            writer,
            "LG Buddy Startup",
            &tv,
            deps.wol_sender,
            &config.tv_mac,
        )?;
        deps.sleeper.sleep(retry_delay);
    }

    writeln!(
        writer,
        "LG Buddy Startup: set_input failed after {STARTUP_WAKE_ATTEMPTS} attempts"
    )?;
    Ok(())
}

fn run_brightness_with<
    W: Write,
    C: TvClient,
    R: ReachabilityChecker,
    U: BrightnessUi,
    N: Notifier,
>(
    writer: &mut W,
    config: &Config,
    deps: BrightnessDeps<'_, C, R, U, N>,
) -> Result<(), RunError> {
    let tv = TvDevice::new(deps.tv_client, config.tv_ip);

    match deps.reachability.is_reachable(config.tv_ip) {
        Ok(true) => {}
        Ok(false) => {
            let message = format!("TV is not reachable at {}.", config.tv_ip);
            let _ = deps.ui.show_error("LG Buddy", &message);
            return Err(RunError::Policy(message));
        }
        Err(err) => {
            let message = format!("Could not check TV reachability at {}. {err}", config.tv_ip);
            let _ = deps.ui.show_error("LG Buddy", &message);
            return Err(RunError::Policy(message));
        }
    }

    let initial_brightness = tv
        .picture()
        .oled_brightness()
        .unwrap_or(BRIGHTNESS_DEFAULT_VALUE);

    let Some(brightness) = deps.ui.prompt_brightness(initial_brightness)? else {
        return Ok(());
    };

    match tv.picture().set_oled_brightness(brightness) {
        Ok(_) => {
            let _ = deps
                .notifier
                .notify("LG TV", &format!("Brightness set to {brightness}%"));
            writeln!(
                writer,
                "LG Buddy Brightness: Set OLED pixel brightness to {brightness}%."
            )?;
            Ok(())
        }
        Err(err) => {
            let _ = deps.notifier.notify("LG TV", "Failed to set brightness");
            Err(RunError::Policy(format!("failed to set brightness: {err}")))
        }
    }
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
) -> Result<(), RunError> {
    if !marker.exists() {
        if config.screen_restore_policy == ScreenRestorePolicy::Aggressive {
            writeln!(
                writer,
                "LG Buddy Screen On: State file not found. Aggressive restore policy is enabled, so LG Buddy will attempt wake anyway."
            )?;
        } else {
            writeln!(
                writer,
                "LG Buddy Screen On: State file not found. TV was not turned off by LG Buddy. Skipping wake."
            )?;
            return Ok(());
        }
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
        run_brightness_with, run_screen_off_with, run_screen_on_with, run_shutdown_with,
        run_sleep_pre_with, run_sleep_with, run_startup_with, BrightnessDeps, BrightnessUi,
        NetworkWaiter, Notifier, ReachabilityChecker, RebootDetector, SleepRequestDetector,
        Sleeper, StartupDeps, BRIGHTNESS_DEFAULT_VALUE,
    };
    use crate::config::{Config, HdmiInput, MacAddress, ScreenBackend, ScreenRestorePolicy};
    use crate::state::ScreenOwnershipMarker;
    use crate::tv::BscpylgtvCommandClient;
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use crate::StartupMode;
    use std::cell::RefCell;
    use std::ffi::CString;
    use std::fs;
    use std::io;
    use std::net::Ipv4Addr;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use support::MockBscpylgtv;

    #[cfg(unix)]
    fn set_modified_time(path: &Path, modified: SystemTime) {
        let duration = modified
            .duration_since(UNIX_EPOCH)
            .expect("modified time should be after the unix epoch");
        let path =
            CString::new(path.as_os_str().as_bytes()).expect("path should not contain nul bytes");
        let times = [
            libc::timespec {
                tv_sec: duration.as_secs() as libc::time_t,
                tv_nsec: duration.subsec_nanos() as libc::c_long,
            },
            libc::timespec {
                tv_sec: duration.as_secs() as libc::time_t,
                tv_nsec: duration.subsec_nanos() as libc::c_long,
            },
        ];

        let result = unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(
            result,
            0,
            "failed to set file timestamps: {}",
            io::Error::last_os_error()
        );
    }

    #[cfg(not(unix))]
    fn set_modified_time(_path: &Path, _modified: SystemTime) {
        panic!("set_modified_time is only implemented for unix test targets");
    }

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
        )
        .expect("missing marker should skip");

        assert_call_commands(&mock, &[]);
        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("State file not found."));
    }

    #[test]
    fn screen_on_aggressive_mode_restores_without_marker() {
        let temp_dir = TestDir::new("screen-on-aggressive-no-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-on-aggressive-no-marker-tv");
        mock.queue_error("turn_screen_on", 1, "offline\n");
        mock.queue_error("set_input", 1, "not ready\n");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        run_screen_on_with(
            &mut output,
            &sample_config_with_restore_policy(HdmiInput::Hdmi2, ScreenRestorePolicy::Aggressive),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect("aggressive mode should restore without a marker");

        assert!(!marker.exists());
        assert_call_commands(&mock, &["turn_screen_on", "set_input", "set_input"]);
        assert_eq!(wol.calls().len(), 2);
        assert_eq!(
            sleeper.durations(),
            vec![Duration::from_secs(6), Duration::from_secs(2)]
        );
        let rendered = rendered(&output);
        assert!(rendered.contains("Aggressive restore policy is enabled"));
        assert!(rendered.contains("Wake attempt 2 succeeded."));
        assert!(rendered.contains("Clearing wake state."));
    }

    #[test]
    fn screen_on_restores_even_when_marker_is_old() {
        let temp_dir = TestDir::new("screen-on-old-marker");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create marker");
        set_modified_time(
            marker.path(),
            SystemTime::now() - Duration::from_secs((12 * 60 * 60) + 1),
        );
        let mock = MockBscpylgtv::new("screen-on-old-marker-tv");
        mock.set_screen_on(false);
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
        )
        .expect("old marker should still restore");

        assert!(
            !marker.exists(),
            "successful restore should clear the marker"
        );
        assert_call_commands(&mock, &["turn_screen_on"]);
        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert!(rendered(&output).contains("Screen unblank succeeded."));
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
    fn screen_on_aggressive_mode_returns_error_without_creating_marker_after_exhausting_retries() {
        let temp_dir = TestDir::new("screen-on-aggressive-wake-retry-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("screen-on-aggressive-wake-retry-failure-tv");
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
            &sample_config_with_restore_policy(HdmiInput::Hdmi2, ScreenRestorePolicy::Aggressive),
            &marker,
            &client,
            &wol,
            &sleeper,
        )
        .expect_err("aggressive mode should still fail after exhausting retries");

        assert!(!marker.exists());
        assert_eq!(mock.calls().len(), 7);
        assert_eq!(wol.calls().len(), 7);
        assert_eq!(sleeper.durations().len(), 7);
        assert!(matches!(err, crate::RunError::Policy(_)));
        let rendered = rendered(&output);
        assert!(rendered.contains("Aggressive restore policy is enabled"));
        assert!(rendered.contains("Wake failed after 6 attempts."));
    }

    #[test]
    fn startup_wake_mode_skips_without_system_marker() {
        let temp_dir = TestDir::new("startup-wake-skip");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-wake-skip-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            deps,
            StartupMode::Wake,
        )
        .expect("wake mode without marker should skip");

        assert!(wol.calls().is_empty());
        assert!(sleeper.durations().is_empty());
        assert_eq!(network.calls(), 1);
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
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            deps,
            StartupMode::Auto,
        )
        .expect("auto boot should succeed");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
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
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            deps,
            StartupMode::Auto,
        )
        .expect("auto wake should succeed");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
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
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            deps,
            StartupMode::Boot,
        )
        .expect("boot mode should succeed");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_call_commands(&mock, &["set_input"]);
        assert!(rendered(&output).contains("Cold boot: Turning TV on and switching to HDMI_3."));
    }

    #[test]
    fn startup_ignores_network_wait_failures() {
        let temp_dir = TestDir::new("startup-network-wait-failure");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("startup-network-wait-failure-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::failing(io::ErrorKind::TimedOut, "network offline");
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            deps,
            StartupMode::Boot,
        )
        .expect("startup should continue even if network wait fails");

        assert_eq!(network.calls(), 1);
        assert_eq!(wol.calls().len(), 1);
        assert_call_commands(&mock, &["set_input"]);
        assert!(rendered(&output).contains("TV turned on and set to HDMI_2."));
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
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            deps,
            StartupMode::Boot,
        )
        .expect("startup retry should succeed");

        assert_eq!(network.calls(), 1);
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
    fn startup_logs_failure_after_exhausting_retries_and_leaves_marker_cleared() {
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
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        run_startup_with(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &marker,
            deps,
            StartupMode::Auto,
        )
        .expect("startup should still succeed after exhausting retries");

        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_eq!(mock.calls().len(), 6);
        assert_eq!(wol.calls().len(), 7);
        assert_eq!(sleeper.durations().len(), 7);
        assert!(rendered(&output).contains("set_input failed after 6 attempts"));
    }

    #[test]
    fn brightness_sets_oled_brightness_and_notifies() {
        let mock = MockBscpylgtv::new("brightness-success-tv");
        mock.set_backlight(72);
        let client = client_for_mock(&mock);
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::selected(65);
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDeps {
            tv_client: &client,
            reachability: &reachability,
            ui: &ui,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        run_brightness_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect("brightness should succeed");

        assert_eq!(ui.initial_values(), vec![72]);
        assert!(ui.error_messages().is_empty());
        assert_eq!(
            notifier.messages(),
            vec![("LG TV".to_string(), "Brightness set to 65%".to_string())]
        );
        assert_eq!(mock.state_snapshot().backlight, 65);
        assert_call_commands(&mock, &["get_picture_settings", "set_settings"]);
        assert!(rendered(&output).contains("Set OLED pixel brightness to 65%."));
    }

    #[test]
    fn brightness_returns_ok_when_dialog_is_cancelled() {
        let mock = MockBscpylgtv::new("brightness-cancel-tv");
        let client = client_for_mock(&mock);
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::cancelled();
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDeps {
            tv_client: &client,
            reachability: &reachability,
            ui: &ui,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        run_brightness_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect("cancel should exit cleanly");

        assert_eq!(ui.initial_values(), vec![50]);
        assert_call_commands(&mock, &["get_picture_settings"]);
        assert!(notifier.messages().is_empty());
        assert!(rendered(&output).is_empty());
    }

    #[test]
    fn brightness_shows_error_and_fails_when_tv_is_unreachable() {
        let mock = MockBscpylgtv::new("brightness-unreachable-tv");
        let client = client_for_mock(&mock);
        let reachability = FakeReachabilityChecker::unreachable();
        let ui = FakeBrightnessUi::selected(50);
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDeps {
            tv_client: &client,
            reachability: &reachability,
            ui: &ui,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        let err = run_brightness_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect_err("unreachable tv should fail");

        assert!(matches!(err, crate::RunError::Policy(_)));
        assert!(mock.calls().is_empty());
        assert!(notifier.messages().is_empty());
        assert_eq!(
            ui.error_messages(),
            vec![(
                "LG Buddy".to_string(),
                "TV is not reachable at 192.0.2.42.".to_string(),
            )]
        );
    }

    #[test]
    fn brightness_defaults_to_fifty_when_current_brightness_query_fails() {
        let mock = MockBscpylgtv::new("brightness-query-failure-tv");
        mock.queue_error("get_picture_settings", 1, "offline\n");
        let client = client_for_mock(&mock);
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::cancelled();
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDeps {
            tv_client: &client,
            reachability: &reachability,
            ui: &ui,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        run_brightness_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect("fallback to default brightness should still allow cancellation");

        assert_eq!(ui.initial_values(), vec![BRIGHTNESS_DEFAULT_VALUE]);
        assert_call_commands(&mock, &["get_picture_settings"]);
        assert!(notifier.messages().is_empty());
        assert!(rendered(&output).is_empty());
    }

    #[test]
    fn brightness_notifies_and_fails_when_tv_update_fails() {
        let mock = MockBscpylgtv::new("brightness-tv-failure");
        mock.queue_error("set_settings", 1, "offline\n");
        let client = client_for_mock(&mock);
        let reachability = FakeReachabilityChecker::reachable();
        let ui = FakeBrightnessUi::selected(30);
        let notifier = RecordingNotifier::default();
        let deps = BrightnessDeps {
            tv_client: &client,
            reachability: &reachability,
            ui: &ui,
            notifier: &notifier,
        };

        let mut output = Vec::new();
        let err = run_brightness_with(&mut output, &sample_config(HdmiInput::Hdmi2), deps)
            .expect_err("tv command failure should fail");

        assert!(matches!(err, crate::RunError::Policy(_)));
        assert_eq!(ui.initial_values(), vec![50]);
        assert_eq!(
            notifier.messages(),
            vec![("LG TV".to_string(), "Failed to set brightness".to_string())]
        );
        assert_call_commands(&mock, &["get_picture_settings", "set_settings"]);
        assert!(rendered(&output).is_empty());
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
        sample_config_with_restore_policy(input, ScreenRestorePolicy::MarkerOnly)
    }

    fn sample_config_with_restore_policy(
        input: HdmiInput,
        screen_restore_policy: ScreenRestorePolicy,
    ) -> Config {
        Config {
            tv_ip: "192.0.2.42".parse::<Ipv4Addr>().expect("parse ipv4"),
            tv_mac: "aa:bb:cc:dd:ee:ff"
                .parse::<MacAddress>()
                .expect("parse mac"),
            input,
            screen_backend: ScreenBackend::Auto,
            screen_idle_timeout: 300,
            screen_restore_policy,
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

    struct FakeNetworkWaiter {
        result: io::Result<()>,
        calls: RefCell<u32>,
    }

    impl FakeNetworkWaiter {
        fn clear() -> Self {
            Self {
                result: Ok(()),
                calls: RefCell::new(0),
            }
        }

        fn failing(kind: io::ErrorKind, message: &str) -> Self {
            Self {
                result: Err(io::Error::new(kind, message.to_string())),
                calls: RefCell::new(0),
            }
        }

        fn calls(&self) -> u32 {
            *self.calls.borrow()
        }
    }

    impl NetworkWaiter for FakeNetworkWaiter {
        fn wait_for_network(&self) -> io::Result<()> {
            *self.calls.borrow_mut() += 1;

            match &self.result {
                Ok(()) => Ok(()),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    struct FakeReachabilityChecker {
        reachable: io::Result<bool>,
    }

    impl FakeReachabilityChecker {
        fn reachable() -> Self {
            Self {
                reachable: Ok(true),
            }
        }

        fn unreachable() -> Self {
            Self {
                reachable: Ok(false),
            }
        }
    }

    impl ReachabilityChecker for FakeReachabilityChecker {
        fn is_reachable(&self, _tv_ip: Ipv4Addr) -> io::Result<bool> {
            match &self.reachable {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    struct FakeBrightnessUi {
        selection: io::Result<Option<u8>>,
        initial_values: RefCell<Vec<u8>>,
        error_messages: RefCell<Vec<(String, String)>>,
    }

    impl FakeBrightnessUi {
        fn selected(value: u8) -> Self {
            Self {
                selection: Ok(Some(value)),
                initial_values: RefCell::new(Vec::new()),
                error_messages: RefCell::new(Vec::new()),
            }
        }

        fn cancelled() -> Self {
            Self {
                selection: Ok(None),
                initial_values: RefCell::new(Vec::new()),
                error_messages: RefCell::new(Vec::new()),
            }
        }

        fn initial_values(&self) -> Vec<u8> {
            self.initial_values.borrow().clone()
        }

        fn error_messages(&self) -> Vec<(String, String)> {
            self.error_messages.borrow().clone()
        }
    }

    impl BrightnessUi for FakeBrightnessUi {
        fn prompt_brightness(&self, initial: u8) -> io::Result<Option<u8>> {
            self.initial_values.borrow_mut().push(initial);
            match &self.selection {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }

        fn show_error(&self, title: &str, message: &str) -> io::Result<()> {
            self.error_messages
                .borrow_mut()
                .push((title.to_string(), message.to_string()));
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingNotifier {
        messages: RefCell<Vec<(String, String)>>,
    }

    impl RecordingNotifier {
        fn messages(&self) -> Vec<(String, String)> {
            self.messages.borrow().clone()
        }
    }

    impl Notifier for RecordingNotifier {
        fn notify(&self, title: &str, message: &str) -> io::Result<()> {
            self.messages
                .borrow_mut()
                .push((title.to_string(), message.to_string()));
            Ok(())
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
