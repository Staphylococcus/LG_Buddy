mod dev;

#[cfg(all(feature = "gui-test-fixtures", not(debug_assertions)))]
compile_error!("gui-test-fixtures must never be enabled in a release build");
#[cfg(feature = "gui-test-fixtures")]
mod gui_test_fixtures;

pub mod application;
pub mod audio;
pub mod auth;
pub mod backend;
pub mod brightness;
mod command;
pub mod commands;
pub mod config;
pub mod diagnostics;
pub mod diagnostics_view;
pub mod events;
pub mod inhibition;
pub mod kwin_bridge;
pub mod lifecycle;
pub mod migration;
pub mod navigation;
pub mod notifications;
pub mod overview;
pub mod pairing;
mod pairing_store;
pub mod platform_access_token;
pub mod policy;
pub mod presentation;
pub mod release_bundle;
pub mod runtime_phase;
pub mod screen;
pub mod session;
pub mod session_bus;
pub mod session_notifications;
pub mod settings;
pub mod settings_view;
pub mod setup;
pub mod sources;
pub mod state;
pub mod tv;
pub mod tvs;
pub mod update_flow;
pub mod update_install;
pub mod updates;
pub mod upgrade_preflight;
pub mod version;
pub mod web_os;
pub mod wol;

pub use dev::{DevCommand, DevError, DevParseError, WebOsControlProbeCommand};
pub use sources::desktop::{gnome, swayidle, wayland};
pub use sources::linux::{logind, network_manager};

use crate::backend::{
    configured_backend_from_env_or_config, detect_backend_from_system, BackendDetectionError,
    BackendSelectionError,
};
use crate::commands::{
    run_brightness, run_nm_pre_down, run_screen_off, run_screen_on, run_shutdown, run_sleep,
    run_sleep_pre, run_volume,
};
use crate::config::{
    load_current_config, resolve_config_path_from_env, ConfigError, ConfigLoadError,
    ConfigPathError,
};
use crate::dev::run_dev_command;
use crate::notifications::NotificationError;
use crate::session::runner::{run_lifecycle_monitor, run_monitor};
use crate::settings::{run_settings_command, SettingsCommand, SettingsError, SettingsParseError};
use crate::state::StateDirError;
use crate::tv::{
    OledBrightness, OledBrightnessParseError, TvClientBuildError, VolumeLevel,
    VolumeLevelParseError,
};
use crate::update_install::{run_update_install, UpdateInstallError};
use crate::updates::{run_updates_command, UpdatesCommand, UpdatesError, UpdatesParseError};
use crate::upgrade_preflight::CompatibilityReport;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Overview,
    Startup(StartupMode),
    Shutdown,
    Power(PowerCommand),
    SleepPre,
    Sleep,
    NetworkManagerPreDown,
    Brightness(BrightnessCommand),
    Volume(VolumeCommand),
    Screen(ScreenCommand),
    ScreenOff,
    ScreenOn,
    Monitor,
    Lifecycle,
    DetectBackend,
    Setup(setup::cli::SetupOptions),
    KWinBridge(kwin_bridge::KWinBridgeCommand),
    Dev(DevCommand),
    Settings(SettingsCommand),
    Updates(UpdatesCommand),
    UpgradePreflight {
        candidate_root: PathBuf,
        remove_legacy_env: bool,
        json: bool,
    },
    /// Internal GNOME readiness probe (migration 257, Slice A). A parent
    /// process runs this as a bounded child: it checks GNOME readiness against
    /// an EXPLICIT bus address only — no env read, no autolaunch, no platform
    /// lookup. `bus_address` is the `--bus <address>` value; a missing or
    /// empty address is a fast `Unavailable` (exit 1).
    ///
    /// Typed exit codes (the parent parses the process exit status):
    /// `0` ready; `1` unavailable (missing/invalid `--bus`, or a
    /// setup/transport error); `2` cancelled; `3` readiness failed
    /// (services / subscriptions / watch / read / owner). Resource release is
    /// the daemon reaping the owned client's matches on disconnect (verified
    /// by the dedicated `disconnected_client_match_cycles` test — no extra
    /// RemoveWatch/RemoveMatch required).
    GnomeReadinessProbe {
        bus_address: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrightnessCommand {
    Prompt,
    Get,
    Set(OledBrightness),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeCommand {
    Get,
    Set(VolumeLevel),
    Up,
    Down,
    Mute(MuteCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuteCommand {
    Toggle,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerCommand {
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenCommand {
    Off,
    On,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsHelpTopic {
    Root,
    List,
    Describe,
    Get,
    Set,
    Unset,
}

impl SettingsHelpTopic {
    fn from_subcommand(subcommand: &str) -> Option<Self> {
        match subcommand {
            "list" => Some(Self::List),
            "describe" => Some(Self::Describe),
            "get" => Some(Self::Get),
            "set" => Some(Self::Set),
            "unset" => Some(Self::Unset),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatesHelpTopic {
    Root,
    Check,
    Install,
}

impl UpdatesHelpTopic {
    fn from_subcommand(subcommand: &str) -> Option<Self> {
        match subcommand {
            "check" => Some(Self::Check),
            "install" => Some(Self::Install),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpTopic {
    Global,
    Setup,
    Brightness,
    Volume,
    Power,
    Screen,
    Settings(SettingsHelpTopic),
    Updates(UpdatesHelpTopic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupMode {
    Auto,
    Boot,
    Wake,
}

impl StartupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Boot => "boot",
            Self::Wake => "wake",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "boot" => Some(Self::Boot),
            "wake" => Some(Self::Wake),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Help(HelpTopic),
    Version,
    Command(Command),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnknownCommand(String),
    MissingPowerCommand,
    UnknownPowerCommand(String),
    MissingScreenCommand,
    UnknownScreenCommand(String),
    UnknownStartupMode(String),
    UnknownBrightnessCommand(String),
    MissingBrightnessValue,
    InvalidBrightnessValue(OledBrightnessParseError),
    UnknownVolumeCommand(String),
    UnknownMuteCommand(String),
    InvalidVolumeValue(VolumeLevelParseError),
    Dev(DevParseError),
    Settings(SettingsParseError),
    Updates(UpdatesParseError),
    MissingUpgradePreflightRoot,
    MissingGnomeReadinessProbeBus,
    Setup(String),
    UnexpectedArguments {
        command: Command,
        arguments: Vec<String>,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(command) => {
                write!(f, "unknown command `{command}`")
            }
            Self::MissingPowerCommand => {
                write!(
                    f,
                    "missing power command; expected `power on` or `power off`"
                )
            }
            Self::UnknownPowerCommand(command) => {
                write!(f, "unknown power command `{command}`")
            }
            Self::MissingScreenCommand => {
                write!(
                    f,
                    "missing screen command; expected `screen off` or `screen on`"
                )
            }
            Self::UnknownScreenCommand(command) => {
                write!(f, "unknown screen command `{command}`")
            }
            Self::UnknownStartupMode(mode) => {
                write!(f, "unknown startup mode `{mode}`")
            }
            Self::UnknownBrightnessCommand(command) => {
                write!(f, "unknown brightness command `{command}`")
            }
            Self::MissingBrightnessValue => {
                write!(f, "missing brightness value for `brightness set`")
            }
            Self::InvalidBrightnessValue(err) => write!(f, "{err}"),
            Self::UnknownVolumeCommand(command) => {
                write!(f, "unknown volume command `{command}`")
            }
            Self::UnknownMuteCommand(command) => {
                write!(f, "unknown mute command `{command}`; expected `volume mute`, `volume mute on`, or `volume mute off`")
            }
            Self::InvalidVolumeValue(err) => write!(f, "{err}"),
            Self::Dev(err) => write!(f, "{err}"),
            Self::Settings(err) => write!(f, "{err}"),
            Self::Updates(err) => write!(f, "{err}"),
            Self::Setup(error) => write!(f, "{error}"),
            Self::MissingUpgradePreflightRoot => {
                write!(f, "missing candidate root for `upgrade-preflight`")
            }
            Self::MissingGnomeReadinessProbeBus => {
                write!(f, "missing bus address for `gnome-readiness-probe --bus`")
            }
            Self::UnexpectedArguments { command, arguments } => {
                write!(
                    f,
                    "unexpected arguments for `{}`: {}",
                    command.as_str(),
                    arguments.join(" ")
                )
            }
        }
    }
}

#[derive(Debug)]
pub enum RunError {
    Setup(setup::cli::SetupError),
    Io(io::Error),
    Policy(String),
    MigrationRequired(String),
    TvClientBuild(TvClientBuildError),
    ConfigPath(ConfigPathError),
    Config(ConfigError),
    ConfigLoad(ConfigLoadError),
    StateDir(StateDirError),
    BackendSelection(BackendSelectionError),
    BackendDetection(BackendDetectionError),
    Dev(DevError),
    Settings(SettingsError),
    Updates(UpdatesError),
    UpdateInstall(UpdateInstallError),
    UpgradePreflight(CompatibilityReport),
    GnomeReadinessProbe(GnomeReadinessProbeError),
    NotificationAfterPrimary {
        primary: Box<RunError>,
        notification: NotificationError,
    },
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(err) => write!(f, "{err}"),
            Self::Io(err) => write!(f, "{err}"),
            Self::Policy(err) => write!(f, "{err}"),
            Self::MigrationRequired(err) => write!(f, "{err}"),
            Self::TvClientBuild(err) => write!(f, "{err}"),
            Self::ConfigPath(err) => write!(f, "{err}"),
            Self::Config(err) => write!(f, "{err}"),
            Self::ConfigLoad(err) => write!(f, "{err}"),
            Self::StateDir(err) => write!(f, "{err}"),
            Self::BackendSelection(err) => write!(f, "{err}"),
            Self::BackendDetection(err) => write!(f, "{err}"),
            Self::Dev(err) => write!(f, "{err}"),
            Self::Settings(err) => write!(f, "{err}"),
            Self::Updates(err) => write!(f, "{err}"),
            Self::UpdateInstall(err) => write!(f, "{err}"),
            Self::UpgradePreflight(report) => write!(f, "{report}"),
            Self::GnomeReadinessProbe(err) => write!(f, "{err}"),
            Self::NotificationAfterPrimary {
                primary,
                notification,
            } => write!(
                f,
                "{primary}; additionally, desktop notification failed: {notification}"
            ),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Setup(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Policy(_) => None,
            Self::MigrationRequired(_) => None,
            Self::TvClientBuild(err) => Some(err),
            Self::ConfigPath(err) => Some(err),
            Self::Config(err) => Some(err),
            Self::ConfigLoad(err) => Some(err),
            Self::StateDir(err) => Some(err),
            Self::BackendSelection(err) => Some(err),
            Self::BackendDetection(err) => Some(err),
            Self::Dev(err) => Some(err),
            Self::Settings(err) => Some(err),
            Self::Updates(err) => Some(err),
            Self::UpdateInstall(err) => Some(err),
            Self::UpgradePreflight(_) => None,
            Self::GnomeReadinessProbe(err) => Some(err),
            Self::NotificationAfterPrimary { primary, .. } => Some(primary.as_ref()),
        }
    }
}

/// Fixed-stage error for the internal `gnome-readiness-probe` command. Each
/// variant maps to a stable process exit code the parent parses. No variant
/// embeds transport strings, owner names, or reply values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GnomeReadinessProbeError {
    /// Missing/empty/invalid `--bus`, or a bus setup/transport error (exit 1).
    Unavailable,
    /// Cancellation observed at a readiness checkpoint (exit 2).
    Cancelled,
    /// Readiness failed: services / subscriptions / watch / read / owner (exit 3).
    NotReady,
}

impl GnomeReadinessProbeError {
    /// The stable process exit code for this failure stage.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Unavailable => 1,
            Self::Cancelled => 2,
            Self::NotReady => 3,
        }
    }
}

impl fmt::Display for GnomeReadinessProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Unavailable => "gnome readiness probe unavailable",
            Self::Cancelled => "gnome readiness probe cancelled",
            Self::NotReady => "gnome readiness probe not ready",
        };
        f.write_str(message)
    }
}

impl std::error::Error for GnomeReadinessProbeError {}

/// Boundary mapping from the config module's typed loader error to the top-level
/// application error. Stale configs surface as the distinct `MigrationRequired`
/// failure (the pre-migration read-only signal); every other loader failure is
/// carried as a config-load error. Kept here, not in the config module, so the
/// config module stays free of top-level `RunError` construction.
impl From<ConfigLoadError> for RunError {
    fn from(err: ConfigLoadError) -> Self {
        match err {
            ConfigLoadError::Stale(_) => RunError::MigrationRequired(err.to_string()),
            other => RunError::ConfigLoad(other),
        }
    }
}

impl ParseError {
    pub fn help_topic(&self) -> HelpTopic {
        match self {
            Self::Setup(_) => HelpTopic::Setup,
            Self::UnknownBrightnessCommand(_)
            | Self::MissingBrightnessValue
            | Self::InvalidBrightnessValue(_) => HelpTopic::Brightness,
            Self::UnexpectedArguments {
                command: Command::Brightness(_),
                ..
            } => HelpTopic::Brightness,
            Self::UnknownVolumeCommand(_)
            | Self::UnknownMuteCommand(_)
            | Self::InvalidVolumeValue(_) => HelpTopic::Volume,
            Self::UnexpectedArguments {
                command: Command::Volume(_),
                ..
            } => HelpTopic::Volume,
            Self::MissingPowerCommand | Self::UnknownPowerCommand(_) => HelpTopic::Power,
            Self::UnexpectedArguments {
                command: Command::Power(_),
                ..
            } => HelpTopic::Power,
            Self::MissingScreenCommand | Self::UnknownScreenCommand(_) => HelpTopic::Screen,
            Self::UnexpectedArguments {
                command: Command::Screen(_),
                ..
            } => HelpTopic::Screen,
            Self::Settings(error) => HelpTopic::Settings(match error {
                SettingsParseError::MissingKey { subcommand }
                | SettingsParseError::MissingValue { subcommand }
                | SettingsParseError::UnexpectedArguments { subcommand, .. } => {
                    SettingsHelpTopic::from_subcommand(subcommand)
                        .unwrap_or(SettingsHelpTopic::Root)
                }
                SettingsParseError::MissingSubcommand
                | SettingsParseError::UnknownSubcommand(_) => SettingsHelpTopic::Root,
            }),
            Self::Updates(error) => HelpTopic::Updates(match error {
                UpdatesParseError::DuplicateNotify => UpdatesHelpTopic::Check,
                UpdatesParseError::UnexpectedArguments { subcommand, .. } => {
                    UpdatesHelpTopic::from_subcommand(subcommand).unwrap_or(UpdatesHelpTopic::Root)
                }
                UpdatesParseError::MissingSubcommand | UpdatesParseError::UnknownSubcommand(_) => {
                    UpdatesHelpTopic::Root
                }
            }),
            _ => HelpTopic::Global,
        }
    }
}

impl From<io::Error> for RunError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<TvClientBuildError> for RunError {
    fn from(value: TvClientBuildError) -> Self {
        Self::TvClientBuild(value)
    }
}

impl Command {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Startup(_) => "startup",
            Self::Shutdown => "shutdown",
            Self::Power(_) => "power",
            Self::SleepPre => "sleep-pre",
            Self::Sleep => "sleep",
            Self::NetworkManagerPreDown => "nm-pre-down",
            Self::Brightness(_) => "brightness",
            Self::Volume(_) => "volume",
            Self::Screen(_) => "screen",
            Self::ScreenOff => "screen-off",
            Self::ScreenOn => "screen-on",
            Self::Monitor => "monitor",
            Self::Lifecycle => "lifecycle",
            Self::DetectBackend => "detect-backend",
            Self::Setup(_) => "setup",
            Self::KWinBridge(_) => "kwin-bridge",
            Self::Dev(command) => command.as_str(),
            Self::Settings(_) => "settings",
            Self::Updates(_) => "updates",
            Self::UpgradePreflight { .. } => "upgrade-preflight",
            Self::GnomeReadinessProbe { .. } => "gnome-readiness-probe",
        }
    }

    pub fn placeholder_message(&self) -> &'static str {
        match self {
            Self::Overview => "TODO: implemented via command handler",
            Self::Startup(_) => "TODO: implemented via command handler",
            Self::Shutdown => "TODO: implemented via command handler",
            Self::Power(_) => "TODO: implemented via command handler",
            Self::SleepPre => "TODO: implemented via command handler",
            Self::Sleep => "TODO: implemented via command handler",
            Self::NetworkManagerPreDown => "TODO: implemented via command handler",
            Self::Brightness(_) => "TODO: implemented via command handler",
            Self::Volume(_) => "TODO: implemented via command handler",
            Self::Screen(_) => "TODO: implemented via command handler",
            Self::ScreenOff => "TODO: implemented via command handler",
            Self::ScreenOn => "TODO: implemented via command handler",
            Self::Monitor => "TODO: implemented via command handler",
            Self::Lifecycle => "TODO: implemented via command handler",
            Self::DetectBackend => "TODO: implement detect-backend command",
            Self::Setup(_) => "TODO: implemented via command handler",
            Self::KWinBridge(_) => "TODO: implemented via command handler",
            Self::Dev(_) => "TODO: implemented via temporary dev command handler",
            Self::Settings(_) => "TODO: implemented via command handler",
            Self::Updates(_) => "TODO: implemented via command handler",
            Self::UpgradePreflight { .. } => "TODO: implemented via command handler",
            Self::GnomeReadinessProbe { .. } => "TODO: implemented via command handler",
        }
    }
}

pub fn usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV control

Usage:
  {program}
  {program} <command>
  {program} help [COMMAND...]
  {program} --help, -h
  {program} --version, -V

With no command, open Overview in the installed graphical application.

Commands:
  setup           Complete or repair TV, services and desktop integration setup
  brightness      Open Overview focused on brightness
  brightness get  Print the current TV OLED brightness
  brightness set <0-100>
                  Set the TV OLED brightness
  volume          Print the current TV volume or mute state
  volume <0-100>  Set the TV volume and unmute it
  volume up       Increase the TV volume and unmute it
  volume down     Decrease the TV volume and unmute it
  volume mute     Toggle TV mute
  volume mute on  Mute the TV
  volume mute off Unmute the TV
  power on        Start or restore the TV output
  power off       Power off the TV when LG Buddy owns the active input
  screen off      Blank the configured TV output if active
  screen on       Restore the TV output after an LG Buddy screen blank
  settings        Inspect and edit structured LG Buddy settings
  updates         Check for and install LG Buddy releases
  help [COMMAND...]
                  Show global or scoped command help

Settings:
  settings list
  settings describe [KEY]
  settings get <KEY>
  settings set <KEY> <VALUE>
  settings unset <KEY>

Updates:
  updates check [--notify]
  updates install
"
    )
}

pub fn power_usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV power control

Usage:
  {program} power on
  {program} power off
  {program} power --help

Commands:
  on              Start or restore the TV output
  off             Power off the TV when LG Buddy owns the active input
"
    )
}

pub fn brightness_usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV brightness control

Usage:
  {program} brightness
  {program} brightness get
  {program} brightness set <0-100>
  {program} brightness --help

Commands:
  get             Print the current TV OLED brightness
  set <0-100>     Set the TV OLED brightness
"
    )
}

pub fn volume_usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV volume control

Usage:
  {program} volume
  {program} volume <0-100>
  {program} volume up
  {program} volume down
  {program} volume mute
  {program} volume mute on
  {program} volume mute off
  {program} volume --help

Commands:
  <0-100>         Set the TV volume and unmute it
  up              Increase the TV volume and unmute it
  down            Decrease the TV volume and unmute it
  mute            Toggle TV mute
  mute on         Mute the TV
  mute off        Unmute the TV
"
    )
}

pub fn screen_usage(program: &str) -> String {
    format!(
        "\
LG Buddy TV screen control

Usage:
  {program} screen off
  {program} screen on
  {program} screen --help

Commands:
  off             Blank the configured TV output if active
  on              Restore the TV output after an LG Buddy screen blank
"
    )
}

pub fn settings_usage(program: &str, topic: SettingsHelpTopic) -> String {
    match topic {
        SettingsHelpTopic::Root => format!(
            "\
LG Buddy settings

Usage:
  {program} settings list
  {program} settings describe [KEY]
  {program} settings get <KEY>
  {program} settings set <KEY> <VALUE>
  {program} settings unset <KEY>
  {program} settings --help

Commands:
  list                    List settings and their effective values
  describe [KEY]          Describe one setting or all public settings
  get <KEY>               Print one raw effective value
  set <KEY> <VALUE>       Save a setting value
  unset <KEY>             Remove a saved override
"
        ),
        SettingsHelpTopic::List => format!(
            "\
Usage:
  {program} settings list
"
        ),
        SettingsHelpTopic::Describe => format!(
            "\
Usage:
  {program} settings describe [KEY]
"
        ),
        SettingsHelpTopic::Get => format!(
            "\
Usage:
  {program} settings get <KEY>
"
        ),
        SettingsHelpTopic::Set => format!(
            "\
Usage:
  {program} settings set <KEY> <VALUE>
"
        ),
        SettingsHelpTopic::Unset => format!(
            "\
Usage:
  {program} settings unset <KEY>
"
        ),
    }
}

pub fn updates_usage(program: &str, topic: UpdatesHelpTopic) -> String {
    match topic {
        UpdatesHelpTopic::Root => format!(
            "\
LG Buddy updates

Usage:
  {program} updates check [--notify]
  {program} updates install
  {program} updates --help

Commands:
  check           Check GitHub releases for an available update
  install         Interactively verify and install an available update
"
        ),
        UpdatesHelpTopic::Check => format!(
            "\
LG Buddy update check

Usage:
  {program} updates check [--notify]

Options:
  --notify        Request a desktop notification when an update is available
"
        ),
        UpdatesHelpTopic::Install => format!(
            "\
LG Buddy update installation

Usage:
  {program} updates install

Installs the next release from the saved updates.channel after host checks and
explicit confirmation. Channel and version arguments are not accepted.
"
        ),
    }
}

pub fn help(program: &str, topic: HelpTopic) -> String {
    match topic {
        HelpTopic::Global => usage(program),
        HelpTopic::Setup => setup::cli::usage(program),
        HelpTopic::Brightness => brightness_usage(program),
        HelpTopic::Volume => volume_usage(program),
        HelpTopic::Power => power_usage(program),
        HelpTopic::Screen => screen_usage(program),
        HelpTopic::Settings(topic) => settings_usage(program, topic),
        HelpTopic::Updates(topic) => updates_usage(program, topic),
    }
}

pub fn parse_args<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(ParseOutcome::Command(Command::Overview));
    };

    let first = first.as_ref();
    if matches!(first, "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Global));
    }
    if first == "help" {
        return parse_help_command(args);
    }
    if matches!(first, "-V" | "--version") {
        return Ok(ParseOutcome::Version);
    }

    let command = match first {
        "startup" => {
            let startup_mode = match args.next() {
                Some(mode) => {
                    let mode = mode.as_ref();
                    StartupMode::parse(mode)
                        .ok_or_else(|| ParseError::UnknownStartupMode(mode.to_string()))?
                }
                None => StartupMode::Auto,
            };

            let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            if !extra_args.is_empty() {
                return Err(ParseError::UnexpectedArguments {
                    command: Command::Startup(startup_mode),
                    arguments: extra_args,
                });
            }

            return Ok(ParseOutcome::Command(Command::Startup(startup_mode)));
        }
        "setup" => return setup::cli::parse(args.map(|arg| arg.as_ref().to_string())),
        "settings" => return parse_settings_command(args),
        "updates" => return parse_updates_command(args),
        "kwin-bridge" => {
            let arguments: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            return kwin_bridge::KWinBridgeCommand::parse(&arguments)
                .map(|command| ParseOutcome::Command(Command::KWinBridge(command)))
                .ok_or_else(|| ParseError::UnknownCommand("kwin-bridge: expected info, check, load <plugin-id>, or unload <plugin-id>".into()));
        }
        "upgrade-preflight" => {
            let candidate_root = PathBuf::from(
                args.next()
                    .ok_or(ParseError::MissingUpgradePreflightRoot)?
                    .as_ref(),
            );
            let mut remove_legacy_env = false;
            let mut json = false;
            let mut unexpected = Vec::new();
            for argument in args {
                if argument.as_ref() == "--remove-legacy-env" && !remove_legacy_env {
                    remove_legacy_env = true;
                } else if argument.as_ref() == "--json" && !json {
                    json = true;
                } else {
                    unexpected.push(argument.as_ref().to_string());
                }
            }
            let command = Command::UpgradePreflight {
                candidate_root,
                remove_legacy_env,
                json,
            };
            if !unexpected.is_empty() {
                return Err(ParseError::UnexpectedArguments {
                    command,
                    arguments: unexpected,
                });
            }
            return Ok(ParseOutcome::Command(command));
        }
        "dev" => {
            return DevCommand::parse(args)
                .map(|command| ParseOutcome::Command(Command::Dev(command)))
                .map_err(ParseError::Dev);
        }
        "brightness" => return parse_brightness_command(args),
        "volume" => return parse_volume_command(args),
        "power" => return parse_power_command(args),
        "screen" => return parse_screen_command(args),
        "shutdown" => Command::Shutdown,
        "sleep-pre" => Command::SleepPre,
        "sleep" => Command::Sleep,
        "nm-pre-down" => Command::NetworkManagerPreDown,
        "screen-off" => Command::ScreenOff,
        "screen-on" => Command::ScreenOn,
        "monitor" => Command::Monitor,
        "lifecycle" => Command::Lifecycle,
        "detect-backend" => Command::DetectBackend,
        "gnome-readiness-probe" => {
            let mut bus_address: Option<String> = None;
            let mut unexpected = Vec::new();
            while let Some(argument) = args.next() {
                match argument.as_ref() {
                    "--bus" => {
                        let Some(address) = args.next() else {
                            return Err(ParseError::MissingGnomeReadinessProbeBus);
                        };
                        bus_address = Some(address.as_ref().to_string());
                    }
                    other => unexpected.push(other.to_string()),
                }
            }
            let command = Command::GnomeReadinessProbe { bus_address };
            if !unexpected.is_empty() {
                return Err(ParseError::UnexpectedArguments {
                    command,
                    arguments: unexpected,
                });
            }
            return Ok(ParseOutcome::Command(command));
        }
        other => return Err(ParseError::UnknownCommand(other.to_string())),
    };

    let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
    if !extra_args.is_empty() {
        return Err(ParseError::UnexpectedArguments {
            command,
            arguments: extra_args,
        });
    }

    Ok(ParseOutcome::Command(command))
}

/// Map a readiness check outcome to the probe's typed exit code. Pure:
/// consumes the caller-owned client by value so the exit mapping is testable
/// without a real bus. Actual readiness maps to `0`; `Cancelled` to `2`; any
/// other readiness error to `3`. (Connection failures are mapped separately
/// to `Unavailable` by the caller, not here.)
fn gnome_readiness_probe_exit_code(
    bus: impl session_bus::SessionBusClient,
    stop: &std::sync::atomic::AtomicBool,
) -> u8 {
    match crate::sources::desktop::gnome::readiness::check_gnome_readiness_on(bus, stop) {
        Ok(()) => 0,
        Err(crate::sources::desktop::gnome::readiness::GnomeReadinessError::Cancelled) => 2,
        Err(_) => 3,
    }
}

/// Run the internal `gnome-readiness-probe` command against an explicit bus
/// address. Validation is pure and happens before any connector is invoked:
/// a missing, empty, malformed, or unsupported address is a fast `Unavailable`
/// (exit 1) with no env read, autolaunch, platform lookup, or fallback list.
/// On a valid address the connector is invoked exactly once and, after
/// connection, the readiness check performs D-Bus RPCs on the owned client.
/// (A SIGKILL'd child runs neither `Drop` nor `RemoveWatch`, so the child
/// does not itself clean up — the daemon reaps its fds and Mutter tracks the
/// caller's unique name.)
pub fn run_gnome_readiness_probe(bus_address: Option<&str>) -> u8 {
    // Production path: validate the address, then connect through the real
    // constructor. The address is validated BEFORE the connector runs, so a
    // rejected/autolaunch/exec address never reaches libdbus.
    run_gnome_readiness_probe_with_connector(
        bus_address,
        session_bus::DbusSessionBusClient::new_address,
    )
}

/// Connector-injection variant of `run_gnome_readiness_probe` for tests.
/// Validates the address first (exactly as the production wrapper), then hands
/// the *unchanged* address string to a caller-supplied connector `F`. A valid
/// address reaches the connector exactly once, byte-for-byte; a rejected
/// address never calls `F`. Production passes `DbusSessionBusClient::new_address`;
/// tests pass a fake connector so dangerous samples never touch a real bus.
/// (The validation step is pure: no env read, autolaunch/exec, filesystem, or
/// network operation. After a successful connection the readiness check
/// performs RPCs on the owned client.)
fn run_gnome_readiness_probe_with_connector<F, B>(bus_address: Option<&str>, connect: F) -> u8
where
    F: FnOnce(&str) -> Result<B, session_bus::SessionBusError>,
    B: session_bus::SessionBusClient,
{
    let Some(address) = bus_address else {
        return GnomeReadinessProbeError::Unavailable.exit_code();
    };
    if !is_valid_probe_address(address) {
        return GnomeReadinessProbeError::Unavailable.exit_code();
    }
    let client = match connect(address) {
        Ok(client) => client,
        Err(_) => return GnomeReadinessProbeError::Unavailable.exit_code(),
    };
    let stop = std::sync::atomic::AtomicBool::new(false);
    gnome_readiness_probe_exit_code(client, &stop)
}

/// Pure validator for the child probe's explicit D-Bus address. Accepts
/// exactly one local UNIX endpoint: the exact `unix:` prefix followed by a
/// comma-separated key=value list with exactly one endpoint key (`path` or
/// `abstract`, nonempty) and an optional `guid`. Rejects raw semicolons
/// (fallback lists), NUL, duplicate or unknown keys (including the
/// runtime/tmpdir/dir lookup keys), both endpoint keys, an absent endpoint,
/// empty fields or values, and every non-UNIX transport (tcp/nonce-tcp,
/// unixexec, autolaunch, ...).
///
/// Values are percent-decoded for validation only: every `%` requires two hex
/// digits and a decoded NUL is rejected; every other byte must be a literal
/// address character (ASCII alphanumeric plus `- _ / \ * .`) — all other
/// bytes must be escaped. The decoded `path` must start with `/`; the
/// decoded `abstract` may be any nonempty byte string; the decoded `guid`
/// must be exactly 32 ASCII hex digits. No env read, no
/// fallback/canonicalization, no filesystem or network operations.
fn is_valid_probe_address(address: &str) -> bool {
    let rest = match address.strip_prefix("unix:") {
        Some(rest) if !rest.is_empty() => rest,
        _ => return false,
    };
    if rest.contains(';') || rest.contains('\0') {
        return false;
    }

    let mut has_endpoint = false;
    let mut has_guid = false;
    for field in rest.split(',') {
        let (key, value) = match field.split_once('=') {
            Some((key, value)) => (key, value),
            None => return false,
        };
        let Some(decoded) = decode_probe_address_field(value) else {
            return false;
        };
        match key {
            "path" => {
                if has_endpoint || decoded.first() != Some(&b'/') {
                    return false;
                }
                has_endpoint = true;
            }
            "abstract" => {
                if has_endpoint || decoded.is_empty() {
                    return false;
                }
                has_endpoint = true;
            }
            "guid" => {
                if has_guid
                    || decoded.len() != 32
                    || !decoded.iter().all(|byte| byte.is_ascii_hexdigit())
                {
                    return false;
                }
                has_guid = true;
            }
            _ => return false,
        }
    }
    has_endpoint
}

/// Percent-decode one D-Bus address field for validation. Every `%` must
/// start two hex digits (a decoded NUL is rejected); every other byte must be
/// a literal address character — ASCII alphanumeric plus `- _ / \ * .` — all
/// other bytes must be escaped. Returns the decoded bytes, preserving escaped
/// non-UTF-8 values.
fn decode_probe_address_field(value: &str) -> Option<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            let high = hex_digit_value(*bytes.get(index + 1)?)?;
            let low = hex_digit_value(*bytes.get(index + 2)?)?;
            let decoded_byte = high * 16 + low;
            if decoded_byte == 0 {
                return None;
            }
            decoded.push(decoded_byte);
            index += 3;
        } else if is_probe_address_literal(byte) {
            decoded.push(byte);
            index += 1;
        } else {
            return None;
        }
    }
    Some(decoded)
}

fn is_probe_address_literal(byte: u8) -> bool {
    matches!(
        byte,
        b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'_' | b'/' | b'\\' | b'*' | b'.'
    )
}

fn hex_digit_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn run_command<W: Write>(command: Command, writer: &mut W) -> Result<(), RunError> {
    // #256: TV-operating commands require a current (v2) config. A stale
    // bscpylgtv/1.x config is a read-only failure (MigrationRequired) — no
    // migration is attempted here; setup/overview/detect-backend/settings/dev
    // stay available so the migration host is still reachable.
    if requires_current_config(&command) {
        let config_path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
        // Early-feedback stale gate: a stale 1.x/bscpylgtv config is a
        // read-only MigrationRequired failure before any runtime work. The
        // command handler re-reads via the loader to obtain the snapshot.
        load_current_config(&config_path)?;
    }
    match command {
        Command::Setup(options) => setup::cli::run(options, writer).map_err(RunError::Setup),
        Command::Overview => crate::commands::run_overview(),
        Command::Startup(mode) => crate::commands::run_startup(writer, mode),
        Command::Shutdown => run_shutdown(writer),
        Command::Power(PowerCommand::On) => crate::commands::run_startup(writer, StartupMode::Boot),
        Command::Power(PowerCommand::Off) => run_shutdown(writer),
        Command::SleepPre => run_sleep_pre(writer),
        Command::Sleep => run_sleep(writer),
        Command::NetworkManagerPreDown => run_nm_pre_down(writer),
        Command::Brightness(command) => run_brightness(writer, command),
        Command::Volume(command) => run_volume(writer, command),
        Command::DetectBackend => run_detect_backend(writer),
        Command::KWinBridge(command) => kwin_bridge::run(command, writer).map_err(RunError::Io),
        Command::Screen(ScreenCommand::Off) => run_screen_off(writer),
        Command::Screen(ScreenCommand::On) => run_screen_on(writer),
        Command::ScreenOff => run_screen_off(writer),
        Command::ScreenOn => run_screen_on(writer),
        Command::Monitor => run_monitor(writer),
        Command::Lifecycle => run_lifecycle_monitor(writer),
        Command::Dev(command) => run_dev_command(command, writer).map_err(RunError::Dev),
        Command::Settings(command) => {
            run_settings_command(command, writer).map_err(RunError::Settings)
        }
        Command::Updates(UpdatesCommand::Install) => {
            run_update_install(writer).map_err(RunError::UpdateInstall)
        }
        Command::Updates(command) => {
            run_updates_command(command, writer).map_err(RunError::Updates)
        }
        Command::UpgradePreflight {
            candidate_root,
            remove_legacy_env,
            json,
        } => {
            let report = crate::upgrade_preflight::candidate_host_preflight(
                &candidate_root,
                remove_legacy_env,
            );
            if json {
                writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&report.advice())
                        .expect("preflight advice contains only serializable strings")
                )?;
            }
            if report.compatible() {
                if !json {
                    write!(writer, "{report}")?;
                }
                Ok(())
            } else {
                Err(RunError::UpgradePreflight(report))
            }
        }
        Command::GnomeReadinessProbe { bus_address } => {
            let code = run_gnome_readiness_probe(bus_address.as_deref());
            match code {
                0 => Ok(()),
                1 => Err(RunError::GnomeReadinessProbe(
                    GnomeReadinessProbeError::Unavailable,
                )),
                2 => Err(RunError::GnomeReadinessProbe(
                    GnomeReadinessProbeError::Cancelled,
                )),
                _ => Err(RunError::GnomeReadinessProbe(
                    GnomeReadinessProbeError::NotReady,
                )),
            }
        }
    }
}

/// Commands that operate a TV require a current (v2) config; a stale 1.x /
/// bscpylgtv config is a read-only `MigrationRequired` failure. The migration
/// host (`setup` / `overview` / `detect-backend` / `settings` / `dev`) and the
/// GUI-forwarding `brightness --prompt` stay available on a stale config so
/// the migration flow is still reachable.
///
/// The match is exhaustive by design: adding a `Command` or `BrightnessCommand`
/// variant is a compile error until its config requirement is decided here.
fn requires_current_config(command: &Command) -> bool {
    match command {
        Command::Startup(_)
        | Command::Shutdown
        | Command::Power(_)
        | Command::SleepPre
        | Command::Sleep
        | Command::Screen(ScreenCommand::Off)
        | Command::Screen(ScreenCommand::On)
        | Command::ScreenOff
        | Command::ScreenOn
        | Command::Monitor
        | Command::Lifecycle
        | Command::NetworkManagerPreDown
        | Command::Volume(_)
        // The TV-operating brightness variants are gated.
        | Command::Brightness(BrightnessCommand::Get)
        | Command::Brightness(BrightnessCommand::Set(_)) => true,
        // Migration host / diagnostics / maintenance: no TV operation.
        Command::Overview
        | Command::DetectBackend
        | Command::Setup(_)
        | Command::KWinBridge(_)
        | Command::Dev(_)
        | Command::Settings(_)
        | Command::Updates(_)
        | Command::UpgradePreflight { .. }
        | Command::GnomeReadinessProbe { .. }
        // `prompt` opens the GUI migration host and must stay reachable.
        | Command::Brightness(BrightnessCommand::Prompt) => false,
    }
}

fn parse_help_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(topic) = args.next() else {
        return Ok(ParseOutcome::Help(HelpTopic::Global));
    };

    let remaining = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();
    match topic.as_ref() {
        "setup" if remaining.is_empty() => Ok(ParseOutcome::Help(HelpTopic::Setup)),
        "brightness" => {
            if remaining.is_empty()
                || (remaining.len() == 1 && matches!(remaining[0].as_str(), "get" | "set"))
            {
                Ok(ParseOutcome::Help(HelpTopic::Brightness))
            } else {
                Err(ParseError::UnknownBrightnessCommand(remaining.join(" ")))
            }
        }
        "volume" => {
            let valid_topic = remaining.is_empty()
                || (remaining.len() == 1
                    && matches!(remaining[0].as_str(), "up" | "down" | "mute"))
                || (remaining.len() == 2
                    && remaining[0] == "mute"
                    && matches!(remaining[1].as_str(), "on" | "off"));
            if valid_topic {
                Ok(ParseOutcome::Help(HelpTopic::Volume))
            } else {
                Err(ParseError::UnknownVolumeCommand(remaining.join(" ")))
            }
        }
        "power" => {
            if remaining.is_empty()
                || (remaining.len() == 1 && matches!(remaining[0].as_str(), "on" | "off"))
            {
                Ok(ParseOutcome::Help(HelpTopic::Power))
            } else {
                Err(ParseError::UnknownPowerCommand(remaining.join(" ")))
            }
        }
        "screen" => {
            if remaining.is_empty()
                || (remaining.len() == 1 && matches!(remaining[0].as_str(), "off" | "on"))
            {
                Ok(ParseOutcome::Help(HelpTopic::Screen))
            } else {
                Err(ParseError::UnknownScreenCommand(remaining.join(" ")))
            }
        }
        "settings" => match remaining.as_slice() {
            [] => Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Root,
            ))),
            [subcommand] => SettingsHelpTopic::from_subcommand(subcommand)
                .map(|topic| ParseOutcome::Help(HelpTopic::Settings(topic)))
                .ok_or_else(|| {
                    ParseError::Settings(SettingsParseError::UnknownSubcommand(
                        subcommand.to_string(),
                    ))
                }),
            [subcommand, arguments @ ..] => {
                if let Some(topic) = SettingsHelpTopic::from_subcommand(subcommand) {
                    Err(ParseError::Settings(
                        SettingsParseError::UnexpectedArguments {
                            subcommand: settings_help_subcommand(topic),
                            arguments: arguments.to_vec(),
                        },
                    ))
                } else {
                    Err(ParseError::Settings(SettingsParseError::UnknownSubcommand(
                        subcommand.to_string(),
                    )))
                }
            }
        },
        "updates" => match remaining.as_slice() {
            [] => Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Root,
            ))),
            [subcommand] => UpdatesHelpTopic::from_subcommand(subcommand)
                .map(|topic| ParseOutcome::Help(HelpTopic::Updates(topic)))
                .ok_or_else(|| {
                    ParseError::Updates(UpdatesParseError::UnknownSubcommand(
                        subcommand.to_string(),
                    ))
                }),
            [subcommand, arguments @ ..] => {
                if let Some(topic) = UpdatesHelpTopic::from_subcommand(subcommand) {
                    Err(ParseError::Updates(
                        UpdatesParseError::UnexpectedArguments {
                            subcommand: updates_help_subcommand(topic),
                            arguments: arguments.to_vec(),
                        },
                    ))
                } else {
                    Err(ParseError::Updates(UpdatesParseError::UnknownSubcommand(
                        subcommand.to_string(),
                    )))
                }
            }
        },
        other => Err(ParseError::UnknownCommand(other.to_string())),
    }
}

fn updates_help_subcommand(topic: UpdatesHelpTopic) -> &'static str {
    match topic {
        UpdatesHelpTopic::Root => "updates",
        UpdatesHelpTopic::Check => "check",
        UpdatesHelpTopic::Install => "install",
    }
}

fn settings_help_subcommand(topic: SettingsHelpTopic) -> &'static str {
    match topic {
        SettingsHelpTopic::Root => "settings",
        SettingsHelpTopic::List => "list",
        SettingsHelpTopic::Describe => "describe",
        SettingsHelpTopic::Get => "get",
        SettingsHelpTopic::Set => "set",
        SettingsHelpTopic::Unset => "unset",
    }
}

fn parse_settings_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let arguments = args
        .into_iter()
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    let Some(subcommand) = arguments.first() else {
        return Err(ParseError::Settings(SettingsParseError::MissingSubcommand));
    };

    if matches!(subcommand.as_str(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Settings(
            SettingsHelpTopic::Root,
        )));
    }

    if let Some(topic) = SettingsHelpTopic::from_subcommand(subcommand) {
        if arguments[1..]
            .iter()
            .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
        {
            return Ok(ParseOutcome::Help(HelpTopic::Settings(topic)));
        }
    }

    SettingsCommand::parse(arguments)
        .map(|command| ParseOutcome::Command(Command::Settings(command)))
        .map_err(ParseError::Settings)
}

fn parse_updates_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let arguments = args
        .into_iter()
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    let Some(subcommand) = arguments.first() else {
        return Err(ParseError::Updates(UpdatesParseError::MissingSubcommand));
    };

    if matches!(subcommand.as_str(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Updates(
            UpdatesHelpTopic::Root,
        )));
    }

    if let Some(topic) = UpdatesHelpTopic::from_subcommand(subcommand) {
        if arguments[1..]
            .iter()
            .any(|argument| matches!(argument.as_str(), "-h" | "--help"))
        {
            return Ok(ParseOutcome::Help(HelpTopic::Updates(topic)));
        }
    }

    UpdatesCommand::parse(arguments)
        .map(|command| ParseOutcome::Command(Command::Updates(command)))
        .map_err(ParseError::Updates)
}

fn parse_power_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Err(ParseError::MissingPowerCommand);
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Power));
    }

    let command = match subcommand.as_ref() {
        "on" => PowerCommand::On,
        "off" => PowerCommand::Off,
        other => return Err(ParseError::UnknownPowerCommand(other.to_string())),
    };
    let arguments = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    if arguments.is_empty() {
        Ok(ParseOutcome::Command(Command::Power(command)))
    } else if arguments.len() == 1 && matches!(arguments[0].as_str(), "-h" | "--help") {
        Ok(ParseOutcome::Help(HelpTopic::Power))
    } else {
        Err(ParseError::UnexpectedArguments {
            command: Command::Power(command),
            arguments,
        })
    }
}

fn parse_screen_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Err(ParseError::MissingScreenCommand);
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Screen));
    }

    let command = match subcommand.as_ref() {
        "off" => ScreenCommand::Off,
        "on" => ScreenCommand::On,
        other => return Err(ParseError::UnknownScreenCommand(other.to_string())),
    };
    let arguments = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    if arguments.is_empty() {
        Ok(ParseOutcome::Command(Command::Screen(command)))
    } else if arguments.len() == 1 && matches!(arguments[0].as_str(), "-h" | "--help") {
        Ok(ParseOutcome::Help(HelpTopic::Screen))
    } else {
        Err(ParseError::UnexpectedArguments {
            command: Command::Screen(command),
            arguments,
        })
    }
}

fn parse_brightness_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Ok(ParseOutcome::Command(Command::Brightness(
            BrightnessCommand::Prompt,
        )));
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Brightness));
    }

    match subcommand.as_ref() {
        "get" => {
            let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            if extra_args.is_empty() {
                Ok(ParseOutcome::Command(Command::Brightness(
                    BrightnessCommand::Get,
                )))
            } else if extra_args.len() == 1 && matches!(extra_args[0].as_str(), "-h" | "--help") {
                Ok(ParseOutcome::Help(HelpTopic::Brightness))
            } else {
                Err(ParseError::UnexpectedArguments {
                    command: Command::Brightness(BrightnessCommand::Get),
                    arguments: extra_args,
                })
            }
        }
        "set" => {
            let value = args.next().ok_or(ParseError::MissingBrightnessValue)?;
            if matches!(value.as_ref(), "-h" | "--help") {
                return Ok(ParseOutcome::Help(HelpTopic::Brightness));
            }
            let brightness = OledBrightness::parse(value.as_ref())
                .map_err(ParseError::InvalidBrightnessValue)?;
            let extra_args: Vec<String> = args.map(|arg| arg.as_ref().to_string()).collect();
            let command = BrightnessCommand::Set(brightness);
            if extra_args.is_empty() {
                Ok(ParseOutcome::Command(Command::Brightness(command)))
            } else if extra_args.len() == 1 && matches!(extra_args[0].as_str(), "-h" | "--help") {
                Ok(ParseOutcome::Help(HelpTopic::Brightness))
            } else {
                Err(ParseError::UnexpectedArguments {
                    command: Command::Brightness(command),
                    arguments: extra_args,
                })
            }
        }
        other => Err(ParseError::UnknownBrightnessCommand(other.to_string())),
    }
}

fn parse_volume_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Get)));
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Volume));
    }

    let command = match subcommand.as_ref() {
        "up" => VolumeCommand::Up,
        "down" => VolumeCommand::Down,
        "mute" => return parse_mute_command(args),
        value => {
            VolumeCommand::Set(VolumeLevel::parse(value).map_err(ParseError::InvalidVolumeValue)?)
        }
    };
    let arguments = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    if arguments.is_empty() {
        Ok(ParseOutcome::Command(Command::Volume(command)))
    } else if arguments.len() == 1 && matches!(arguments[0].as_str(), "-h" | "--help") {
        Ok(ParseOutcome::Help(HelpTopic::Volume))
    } else {
        Err(ParseError::UnexpectedArguments {
            command: Command::Volume(command),
            arguments,
        })
    }
}

fn parse_mute_command<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(subcommand) = args.next() else {
        return Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
            MuteCommand::Toggle,
        ))));
    };

    if matches!(subcommand.as_ref(), "-h" | "--help") {
        return Ok(ParseOutcome::Help(HelpTopic::Volume));
    }

    let mute = match subcommand.as_ref() {
        "on" => MuteCommand::On,
        "off" => MuteCommand::Off,
        other => return Err(ParseError::UnknownMuteCommand(other.to_string())),
    };
    let arguments = args
        .map(|argument| argument.as_ref().to_string())
        .collect::<Vec<_>>();

    if arguments.is_empty() {
        Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
            mute,
        ))))
    } else if arguments.len() == 1 && matches!(arguments[0].as_str(), "-h" | "--help") {
        Ok(ParseOutcome::Help(HelpTopic::Volume))
    } else {
        Err(ParseError::UnexpectedArguments {
            command: Command::Volume(VolumeCommand::Mute(mute)),
            arguments,
        })
    }
}

fn run_detect_backend<W: Write>(writer: &mut W) -> Result<(), RunError> {
    let configured = configured_backend_from_env_or_config().map_err(RunError::BackendSelection)?;
    let backend = detect_backend_from_system(configured).map_err(RunError::BackendDetection)?;

    writeln!(writer, "{}", backend.as_str())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        brightness_usage, parse_args, power_usage, screen_usage, settings_usage, updates_usage,
        usage, volume_usage, BrightnessCommand, Command, DevCommand, DevParseError, HelpTopic,
        MuteCommand, ParseError, ParseOutcome, PowerCommand, ScreenCommand, SettingsHelpTopic,
        StartupMode, UpdatesHelpTopic, VolumeCommand, WebOsControlProbeCommand,
    };
    use crate::session_bus::{
        BusMethodCall, BusReply, BusSignal, BusSignalMatch, BusValue, SessionBusClient,
        SessionBusError,
    };
    use crate::settings::{SettingsCommand, SettingsParseError};
    use crate::sources::desktop::gnome::{
        GNOME_IDLE_MONITOR_NAME, GNOME_SCREEN_SAVER_NAME, GNOME_SHELL_NAME,
    };
    use crate::tv::{OledBrightness, VolumeLevel};
    use crate::updates::{UpdatesCommand, UpdatesParseError};
    use crate::{
        gnome_readiness_probe_exit_code, is_valid_probe_address, run_gnome_readiness_probe,
        run_gnome_readiness_probe_with_connector, GnomeReadinessProbeError,
    };
    use crate::{notifications::NotificationError, RunError};
    use std::error::Error;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A self-contained fake observation bus mirroring the GNOME readiness
    /// success trace (service check, unique-owner resolution, activity watch,
    /// screen-saver/idletime reads, and RemoveWatch). Used to prove the probe
    /// handler actually exercises a bus rather than trivially passing.
    struct ProbeFakeBus {
        services: bool,
        screen_saver_owner: Option<String>,
        idle_monitor_owner: Option<String>,
        watch_id: u32,
        observation: Arc<Mutex<ProbeObservation>>,
        dropped: Arc<Mutex<bool>>,
    }

    /// Shared observation state so a test can inspect the bus's call trace
    /// *after* the probe consumed it by value. (The drop flag lives separately
    /// on the bus as `Arc<Mutex<bool>>`, so it needs no field here.)
    #[derive(Default)]
    struct ProbeObservation {
        calls: Vec<(String, String)>,
    }

    impl Default for ProbeFakeBus {
        fn default() -> Self {
            Self {
                services: false,
                screen_saver_owner: None,
                idle_monitor_owner: None,
                watch_id: 0,
                observation: Arc::new(Mutex::new(ProbeObservation::default())),
                dropped: Arc::new(Mutex::new(false)),
            }
        }
    }

    impl Drop for ProbeFakeBus {
        fn drop(&mut self) {
            *self.dropped.lock().unwrap() = true;
        }
    }

    /// A fake observation bus with the happy-path configuration: all three
    /// required service names present, unique owners resolvable, and a valid
    /// activity watch. Returns the bus (owned, to be consumed by value)
    /// alongside the shared state and drop flag so the caller can inspect the
    /// bus after the probe has consumed it.
    fn ready_probe_bus() -> (ProbeFakeBus, Arc<Mutex<ProbeObservation>>, Arc<Mutex<bool>>) {
        let observation = Arc::new(Mutex::new(ProbeObservation::default()));
        let dropped = Arc::new(Mutex::new(false));
        let bus = ProbeFakeBus {
            services: true,
            screen_saver_owner: Some(":1.41".to_string()),
            idle_monitor_owner: Some(":1.42".to_string()),
            watch_id: 7,
            observation: Arc::clone(&observation),
            dropped: Arc::clone(&dropped),
        };
        (bus, observation, dropped)
    }

    impl SessionBusClient for ProbeFakeBus {
        fn name_has_owner(&mut self, name: &str) -> Result<bool, SessionBusError> {
            Ok(self.services
                && matches!(
                    name,
                    GNOME_SHELL_NAME | GNOME_SCREEN_SAVER_NAME | GNOME_IDLE_MONITOR_NAME
                ))
        }

        fn call_method(&mut self, call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            self.observation
                .lock()
                .unwrap()
                .calls
                .push((call.member.to_string(), call.destination.to_string()));
            match call.member {
                "GetNameOwner" => {
                    let [BusValue::String(name)] = call.body.as_slice() else {
                        return Err(SessionBusError::Transport("no name".into()));
                    };
                    let owner = match name.as_str() {
                        GNOME_SCREEN_SAVER_NAME => self.screen_saver_owner.clone(),
                        GNOME_IDLE_MONITOR_NAME => self.idle_monitor_owner.clone(),
                        _ => None,
                    };
                    owner
                        .map(|owner| BusReply::new(vec![BusValue::String(owner)]))
                        .ok_or_else(|| SessionBusError::Transport("no owner".into()))
                }
                "AddUserActiveWatch" => {
                    if Some(call.destination) != self.idle_monitor_owner.as_deref() {
                        return Err(SessionBusError::Transport("wrong owner".into()));
                    }
                    Ok(BusReply::new(vec![BusValue::U32(self.watch_id)]))
                }
                "GetActive" => Ok(BusReply::new(vec![BusValue::Bool(true)])),
                "GetIdletime" => Ok(BusReply::new(vec![BusValue::U64(0)])),
                "RemoveWatch" => Ok(BusReply::new(Vec::new())),
                other => Err(SessionBusError::Transport(format!("unexpected {other}"))),
            }
        }

        fn add_signal_match(&mut self, _rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            Ok(())
        }

        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            panic!("readiness must not enter a monitor loop");
        }
    }

    /// A stub that always returns success and never reads the address: the
    /// ready-case expectation must FAIL against it, proving the handler does
    /// real bus work (a non-trivial client cannot be passed off).
    struct TrivialOkBus;

    impl SessionBusClient for TrivialOkBus {
        fn name_has_owner(&mut self, _name: &str) -> Result<bool, SessionBusError> {
            Ok(true)
        }
        fn call_method(&mut self, _call: BusMethodCall<'_>) -> Result<BusReply, SessionBusError> {
            Ok(BusReply::new(Vec::new()))
        }
        fn add_signal_match(&mut self, _rule: BusSignalMatch<'_>) -> Result<(), SessionBusError> {
            Ok(())
        }
        fn process(&mut self, _timeout: Duration) -> Result<Option<BusSignal>, SessionBusError> {
            Ok(None)
        }
    }

    #[test]
    fn no_args_opens_overview() {
        assert_eq!(
            parse_args(Vec::<String>::new()),
            Ok(ParseOutcome::Command(Command::Overview))
        );
    }

    #[test]
    fn explicit_help_prints_help() {
        assert_eq!(
            parse_args(["--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Global))
        );
        assert_eq!(
            parse_args(["-h"]),
            Ok(ParseOutcome::Help(HelpTopic::Global))
        );
        assert_eq!(
            parse_args(["help"]),
            Ok(ParseOutcome::Help(HelpTopic::Global))
        );
        assert_eq!(
            parse_args(["help", "brightness"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["help", "brightness", "set"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["brightness", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["brightness", "get", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["brightness", "set", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Brightness))
        );
        assert_eq!(
            parse_args(["help", "volume"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["help", "volume", "mute"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["help", "volume", "mute", "on"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["help", "volume", "set"]),
            Err(ParseError::UnknownVolumeCommand("set".to_string()))
        );
        assert_eq!(
            parse_args(["volume", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["volume", "up", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["volume", "mute", "on", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Volume))
        );
        assert_eq!(
            parse_args(["help", "power"]),
            Ok(ParseOutcome::Help(HelpTopic::Power))
        );
        assert_eq!(
            parse_args(["power", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Power))
        );
        assert_eq!(
            parse_args(["power", "on", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Power))
        );
        assert_eq!(
            parse_args(["help", "screen"]),
            Ok(ParseOutcome::Help(HelpTopic::Screen))
        );
        assert_eq!(
            parse_args(["screen", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Screen))
        );
        assert_eq!(
            parse_args(["screen", "off", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Screen))
        );
        assert_eq!(
            parse_args(["help", "settings"]),
            Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Root
            )))
        );
        assert_eq!(
            parse_args(["help", "settings", "set"]),
            Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Set
            )))
        );
        assert_eq!(
            parse_args(["settings", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Root
            )))
        );
        assert_eq!(
            parse_args(["settings", "set", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Settings(
                SettingsHelpTopic::Set
            )))
        );
        assert_eq!(
            parse_args(["help", "updates"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Root
            )))
        );
        assert_eq!(
            parse_args(["help", "updates", "check"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Check
            )))
        );
        assert_eq!(
            parse_args(["help", "updates", "install"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Install
            )))
        );
        assert_eq!(
            parse_args(["updates", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Root
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Check
            )))
        );
        assert_eq!(
            parse_args(["updates", "install", "--help"]),
            Ok(ParseOutcome::Help(HelpTopic::Updates(
                UpdatesHelpTopic::Install
            )))
        );
    }

    #[test]
    fn explicit_version_prints_version() {
        assert_eq!(parse_args(["--version"]), Ok(ParseOutcome::Version));
        assert_eq!(parse_args(["-V"]), Ok(ParseOutcome::Version));
    }

    #[test]
    fn supported_commands_parse() {
        assert_eq!(
            parse_args(["startup"]),
            Ok(ParseOutcome::Command(Command::Startup(StartupMode::Auto)))
        );
        assert_eq!(
            parse_args(["startup", "boot"]),
            Ok(ParseOutcome::Command(Command::Startup(StartupMode::Boot)))
        );
        assert_eq!(
            parse_args(["startup", "wake"]),
            Ok(ParseOutcome::Command(Command::Startup(StartupMode::Wake)))
        );
        assert_eq!(
            parse_args(["shutdown"]),
            Ok(ParseOutcome::Command(Command::Shutdown))
        );
        assert_eq!(
            parse_args(["power", "on"]),
            Ok(ParseOutcome::Command(Command::Power(PowerCommand::On)))
        );
        assert_eq!(
            parse_args(["power", "off"]),
            Ok(ParseOutcome::Command(Command::Power(PowerCommand::Off)))
        );
        assert_eq!(
            parse_args(["sleep-pre"]),
            Ok(ParseOutcome::Command(Command::SleepPre))
        );
        assert_eq!(
            parse_args(["sleep"]),
            Ok(ParseOutcome::Command(Command::Sleep))
        );
        assert_eq!(
            parse_args(["nm-pre-down"]),
            Ok(ParseOutcome::Command(Command::NetworkManagerPreDown))
        );
        assert_eq!(
            parse_args(["brightness"]),
            Ok(ParseOutcome::Command(Command::Brightness(
                BrightnessCommand::Prompt
            )))
        );
        assert_eq!(
            parse_args(["brightness", "get"]),
            Ok(ParseOutcome::Command(Command::Brightness(
                BrightnessCommand::Get
            )))
        );
        assert_eq!(
            parse_args(["brightness", "set", "65"]),
            Ok(ParseOutcome::Command(Command::Brightness(
                BrightnessCommand::Set(brightness(65))
            )))
        );
        assert_eq!(
            parse_args(["volume"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Get)))
        );
        assert_eq!(
            parse_args(["volume", "65"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Set(
                volume(65),
            ))))
        );
        assert_eq!(
            parse_args(["volume", "0"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Set(
                volume(0),
            ))))
        );
        assert_eq!(
            parse_args(["volume", "100"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Set(
                volume(100),
            ))))
        );
        assert_eq!(
            parse_args(["volume", "up"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Up)))
        );
        assert_eq!(
            parse_args(["volume", "down"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Down)))
        );
        assert_eq!(
            parse_args(["volume", "mute"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
                MuteCommand::Toggle,
            ))))
        );
        assert_eq!(
            parse_args(["volume", "mute", "on"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
                MuteCommand::On,
            ))))
        );
        assert_eq!(
            parse_args(["volume", "mute", "off"]),
            Ok(ParseOutcome::Command(Command::Volume(VolumeCommand::Mute(
                MuteCommand::Off,
            ))))
        );
        assert_eq!(
            parse_args(["screen-off"]),
            Ok(ParseOutcome::Command(Command::ScreenOff))
        );
        assert_eq!(
            parse_args(["screen-on"]),
            Ok(ParseOutcome::Command(Command::ScreenOn))
        );
        assert_eq!(
            parse_args(["screen", "off"]),
            Ok(ParseOutcome::Command(Command::Screen(ScreenCommand::Off)))
        );
        assert_eq!(
            parse_args(["screen", "on"]),
            Ok(ParseOutcome::Command(Command::Screen(ScreenCommand::On)))
        );
        assert_eq!(
            parse_args(["monitor"]),
            Ok(ParseOutcome::Command(Command::Monitor))
        );
        assert_eq!(
            parse_args(["lifecycle"]),
            Ok(ParseOutcome::Command(Command::Lifecycle))
        );
        assert_eq!(
            parse_args(["detect-backend"]),
            Ok(ParseOutcome::Command(Command::DetectBackend))
        );
        assert_eq!(
            parse_args(["dev", "webos-auth-probe"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsAuthProbe
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-read-probe"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsReadProbe
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "set-input"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsControlProbe(WebOsControlProbeCommand::SetInput)
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "screen-off"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsControlProbe(WebOsControlProbeCommand::ScreenOff)
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "screen-on"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsControlProbe(WebOsControlProbeCommand::ScreenOn)
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "power-off"]),
            Ok(ParseOutcome::Command(Command::Dev(
                DevCommand::WebOsControlProbe(WebOsControlProbeCommand::PowerOff)
            )))
        );
        assert_eq!(
            parse_args(["settings", "list"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::List
            )))
        );
        assert_eq!(
            parse_args(["settings", "describe"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Describe(None)
            )))
        );
        assert_eq!(
            parse_args(["settings", "describe", "screen.backend"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Describe(Some("screen.backend".to_string()))
            )))
        );
        assert_eq!(
            parse_args(["settings", "get", "screen.backend"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Get("screen.backend".to_string())
            )))
        );
        assert_eq!(
            parse_args(["settings", "set", "screen.backend", "gnome"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Set {
                    key: "screen.backend".to_string(),
                    value: "gnome".to_string(),
                }
            )))
        );
        assert_eq!(
            parse_args(["settings", "unset", "screen.backend"]),
            Ok(ParseOutcome::Command(Command::Settings(
                SettingsCommand::Unset("screen.backend".to_string())
            )))
        );
        assert_eq!(
            parse_args(["updates", "check"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check { notify: false }
            )))
        );
        assert_eq!(
            parse_args(["updates", "check", "--notify"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Check { notify: true }
            )))
        );
        assert_eq!(
            parse_args(["updates", "install"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::Install
            )))
        );
        assert_eq!(
            parse_args(["updates", "background-check"]),
            Ok(ParseOutcome::Command(Command::Updates(
                UpdatesCommand::BackgroundCheck
            )))
        );
        assert_eq!(
            parse_args(["upgrade-preflight", "/tmp/lg-buddy-candidate"]),
            Ok(ParseOutcome::Command(Command::UpgradePreflight {
                candidate_root: PathBuf::from("/tmp/lg-buddy-candidate"),
                remove_legacy_env: false,
                json: false,
            }))
        );
        assert_eq!(
            parse_args([
                "upgrade-preflight",
                "/tmp/lg-buddy-candidate",
                "--remove-legacy-env"
            ]),
            Ok(ParseOutcome::Command(Command::UpgradePreflight {
                candidate_root: PathBuf::from("/tmp/lg-buddy-candidate"),
                remove_legacy_env: true,
                json: false,
            }))
        );
    }

    #[test]
    fn unknown_command_is_rejected() {
        assert_eq!(
            parse_args(["launch"]),
            Err(ParseError::UnknownCommand("launch".to_string()))
        );
    }

    #[test]
    fn extra_arguments_are_rejected() {
        assert_eq!(
            parse_args(["startup", "boot", "extra"]),
            Err(ParseError::UnexpectedArguments {
                command: Command::Startup(StartupMode::Boot),
                arguments: vec!["extra".to_string()],
            })
        );
    }

    #[test]
    fn invalid_startup_mode_is_rejected() {
        assert_eq!(
            parse_args(["startup", "resume"]),
            Err(ParseError::UnknownStartupMode("resume".to_string()))
        );
    }

    #[test]
    fn invalid_power_command_is_rejected_with_power_help() {
        assert_eq!(parse_args(["power"]), Err(ParseError::MissingPowerCommand));
        assert_eq!(
            parse_args(["power", "standby"]),
            Err(ParseError::UnknownPowerCommand("standby".to_string()))
        );
        let error = parse_args(["power", "on", "extra"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::UnexpectedArguments {
                command: Command::Power(PowerCommand::On),
                arguments: vec!["extra".to_string()],
            }
        );
        assert_eq!(error.help_topic(), HelpTopic::Power);
    }

    #[test]
    fn invalid_screen_command_is_rejected_with_screen_help() {
        assert_eq!(
            parse_args(["screen"]),
            Err(ParseError::MissingScreenCommand)
        );
        assert_eq!(
            parse_args(["screen", "toggle"]),
            Err(ParseError::UnknownScreenCommand("toggle".to_string()))
        );
        let error = parse_args(["screen", "off", "extra"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::UnexpectedArguments {
                command: Command::Screen(ScreenCommand::Off),
                arguments: vec!["extra".to_string()],
            }
        );
        assert_eq!(error.help_topic(), HelpTopic::Screen);
    }

    #[test]
    fn invalid_brightness_command_is_rejected() {
        assert_eq!(
            parse_args(["brightness", "show"]),
            Err(ParseError::UnknownBrightnessCommand("show".to_string()))
        );
        assert_eq!(
            parse_args(["brightness", "set"]),
            Err(ParseError::MissingBrightnessValue)
        );
        let error = parse_args(["brightness", "set", "101"]).unwrap_err();
        assert!(matches!(&error, ParseError::InvalidBrightnessValue(_)));
        assert_eq!(error.help_topic(), HelpTopic::Brightness);
        assert!(matches!(
            parse_args(["brightness", "set", "abc"]),
            Err(ParseError::InvalidBrightnessValue(_))
        ));
        assert_eq!(
            parse_args(["brightness", "get", "extra"]),
            Err(ParseError::UnexpectedArguments {
                command: Command::Brightness(BrightnessCommand::Get),
                arguments: vec!["extra".to_string()],
            })
        );
        assert_eq!(
            ParseError::MissingBrightnessValue.help_topic(),
            HelpTopic::Brightness
        );
    }

    #[test]
    fn invalid_volume_command_is_rejected_with_volume_help() {
        let error = parse_args(["volume", "101"]).unwrap_err();
        assert!(matches!(&error, ParseError::InvalidVolumeValue(_)));
        assert_eq!(error.help_topic(), HelpTopic::Volume);
        assert!(matches!(
            parse_args(["volume", "abc"]),
            Err(ParseError::InvalidVolumeValue(_))
        ));
        assert_eq!(
            parse_args(["volume", "set", "65"]),
            Err(ParseError::InvalidVolumeValue(
                VolumeLevel::parse("set").unwrap_err()
            ))
        );
        assert_eq!(
            parse_args(["volume", "mute", "toggle"]),
            Err(ParseError::UnknownMuteCommand("toggle".to_string()))
        );
        let error = parse_args(["volume", "up", "extra"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::UnexpectedArguments {
                command: Command::Volume(VolumeCommand::Up),
                arguments: vec!["extra".to_string()],
            }
        );
        assert_eq!(error.help_topic(), HelpTopic::Volume);
        let error = parse_args(["volume", "mute", "on", "extra"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::UnexpectedArguments {
                command: Command::Volume(VolumeCommand::Mute(MuteCommand::On)),
                arguments: vec!["extra".to_string()],
            }
        );
        assert_eq!(error.help_topic(), HelpTopic::Volume);
        assert_eq!(
            ParseError::UnknownVolumeCommand("set".to_string()).help_topic(),
            HelpTopic::Volume
        );
    }

    #[test]
    fn invalid_dev_command_is_rejected() {
        assert_eq!(
            parse_args(["dev"]),
            Err(ParseError::Dev(DevParseError::MissingSubcommand))
        );
        assert_eq!(
            parse_args(["dev", "other"]),
            Err(ParseError::Dev(DevParseError::UnknownSubcommand(
                "other".to_string()
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-auth-probe", "extra"]),
            Err(ParseError::Dev(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsAuthProbe,
                arguments: vec!["extra".to_string()],
            }))
        );
        assert_eq!(
            parse_args(["dev", "webos-read-probe", "extra"]),
            Err(ParseError::Dev(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsReadProbe,
                arguments: vec!["extra".to_string()],
            }))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe"]),
            Err(ParseError::Dev(DevParseError::MissingControlOperation))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "other"]),
            Err(ParseError::Dev(DevParseError::UnknownControlOperation(
                "other".to_string()
            )))
        );
        assert_eq!(
            parse_args(["dev", "webos-control-probe", "set-input", "extra"]),
            Err(ParseError::Dev(DevParseError::UnexpectedArguments {
                command: DevCommand::WebOsControlProbe(WebOsControlProbeCommand::SetInput),
                arguments: vec!["extra".to_string()],
            }))
        );
    }

    #[test]
    fn invalid_settings_command_is_rejected() {
        assert_eq!(
            parse_args(["settings"]),
            Err(ParseError::Settings(SettingsParseError::MissingSubcommand))
        );
        assert_eq!(
            parse_args(["settings", "get"]),
            Err(ParseError::Settings(SettingsParseError::MissingKey {
                subcommand: "get",
            }))
        );
        assert_eq!(
            parse_args(["settings", "list", "extra"]),
            Err(ParseError::Settings(
                SettingsParseError::UnexpectedArguments {
                    subcommand: "list",
                    arguments: vec!["extra".to_string()],
                }
            ))
        );
        let error = parse_args(["settings", "set", "screen.backend"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::Settings(SettingsParseError::MissingValue { subcommand: "set" })
        );
        assert_eq!(
            error.help_topic(),
            HelpTopic::Settings(SettingsHelpTopic::Set)
        );
    }

    #[test]
    fn invalid_updates_command_is_rejected() {
        assert_eq!(
            parse_args(["updates"]),
            Err(ParseError::Updates(UpdatesParseError::MissingSubcommand))
        );
        assert_eq!(
            parse_args(["updates", "latest"]),
            Err(ParseError::Updates(UpdatesParseError::UnknownSubcommand(
                "latest".to_string()
            )))
        );
        let error = parse_args(["updates", "check", "--channel"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::Updates(UpdatesParseError::UnexpectedArguments {
                subcommand: "check",
                arguments: vec!["--channel".to_string()]
            })
        );
        assert_eq!(
            error.help_topic(),
            HelpTopic::Updates(UpdatesHelpTopic::Check)
        );
        assert_eq!(
            parse_args(["updates", "check", "--channel", "stable"]),
            Err(ParseError::Updates(
                UpdatesParseError::UnexpectedArguments {
                    subcommand: "check",
                    arguments: vec!["--channel".to_string(), "stable".to_string()]
                }
            ))
        );
        assert_eq!(
            parse_args(["updates", "check", "extra"]),
            Err(ParseError::Updates(
                UpdatesParseError::UnexpectedArguments {
                    subcommand: "check",
                    arguments: vec!["extra".to_string()]
                }
            ))
        );
        let error = parse_args(["updates", "install", "stable"]).unwrap_err();
        assert_eq!(
            error,
            ParseError::Updates(UpdatesParseError::UnexpectedArguments {
                subcommand: "install",
                arguments: vec!["stable".to_string()]
            })
        );
        assert_eq!(
            error.help_topic(),
            HelpTopic::Updates(UpdatesHelpTopic::Install)
        );
        assert_eq!(
            parse_args(["updates", "check", "--notify", "--notify"]),
            Err(ParseError::Updates(UpdatesParseError::DuplicateNotify))
        );
        assert_eq!(
            parse_args(["updates", "background-check", "extra"]),
            Err(ParseError::Updates(
                UpdatesParseError::UnexpectedArguments {
                    subcommand: "background-check",
                    arguments: vec!["extra".to_string()]
                }
            ))
        );
    }

    #[test]
    fn candidate_preflight_accepts_structured_advice_with_repair_checks() {
        assert_eq!(
            parse_args([
                "upgrade-preflight",
                "/tmp/candidate",
                "--json",
                "--remove-legacy-env"
            ]),
            Ok(ParseOutcome::Command(Command::UpgradePreflight {
                candidate_root: PathBuf::from("/tmp/candidate"),
                remove_legacy_env: true,
                json: true
            }))
        );
        assert!(matches!(
            parse_args(["upgrade-preflight", "/tmp/candidate", "--json", "--json"]),
            Err(ParseError::UnexpectedArguments { .. })
        ));
    }

    #[test]
    fn invalid_upgrade_preflight_command_is_rejected() {
        assert_eq!(
            parse_args(["upgrade-preflight"]),
            Err(ParseError::MissingUpgradePreflightRoot)
        );
        assert_eq!(
            parse_args(["upgrade-preflight", "/tmp/candidate", "extra"]),
            Err(ParseError::UnexpectedArguments {
                command: Command::UpgradePreflight {
                    candidate_root: PathBuf::from("/tmp/candidate"),
                    remove_legacy_env: false,
                    json: false,
                },
                arguments: vec!["extra".to_string()],
            })
        );
        assert_eq!(
            parse_args([
                "upgrade-preflight",
                "/tmp/candidate",
                "--remove-legacy-env",
                "--remove-legacy-env"
            ]),
            Err(ParseError::UnexpectedArguments {
                command: Command::UpgradePreflight {
                    candidate_root: PathBuf::from("/tmp/candidate"),
                    remove_legacy_env: true,
                    json: false,
                },
                arguments: vec!["--remove-legacy-env".to_string()],
            })
        );
    }

    #[test]
    fn global_usage_only_mentions_public_commands() {
        let help = usage("lg-buddy");

        for command in [
            "brightness",
            "brightness get",
            "brightness set <0-100>",
            "volume",
            "volume <0-100>",
            "volume up",
            "volume down",
            "volume mute",
            "volume mute on",
            "volume mute off",
            "power on",
            "power off",
            "screen off",
            "screen on",
            "settings",
            "updates",
            "help [COMMAND...]",
        ] {
            assert!(
                help.contains(command),
                "missing `{command}` from help output"
            );
        }
        for command in [
            "startup",
            "shutdown",
            "sleep-pre",
            "\n  sleep ",
            "nm-pre-down",
            "monitor",
            "lifecycle",
            "\n  dev ",
            "screen-off",
            "screen-on",
            "detect-backend",
            "updates background-check",
            "upgrade-preflight",
            "webos-auth-probe",
            "webos-read-probe",
        ] {
            assert!(
                !help.contains(command),
                "package-owned entrypoint `{command}` leaked into global help"
            );
        }
    }

    #[test]
    fn power_usage_mentions_public_commands() {
        let help = power_usage("lg-buddy");

        assert!(help.contains("lg-buddy power on"));
        assert!(help.contains("lg-buddy power off"));
        assert!(!help.contains("startup"));
        assert!(!help.contains("shutdown"));
    }

    #[test]
    fn brightness_usage_mentions_public_commands() {
        let help = brightness_usage("lg-buddy");

        assert!(help.contains("lg-buddy brightness\n"));
        assert!(help.contains("lg-buddy brightness get"));
        assert!(help.contains("lg-buddy brightness set <0-100>"));
    }

    #[test]
    fn volume_usage_mentions_public_commands() {
        let help = volume_usage("lg-buddy");

        for command in [
            "lg-buddy volume\n",
            "lg-buddy volume <0-100>",
            "lg-buddy volume up",
            "lg-buddy volume down",
            "lg-buddy volume mute",
            "lg-buddy volume mute on",
            "lg-buddy volume mute off",
        ] {
            assert!(help.contains(command), "missing `{command}` from help");
        }
        assert!(!help.contains("volume set"));
    }

    #[test]
    fn screen_usage_mentions_public_commands() {
        let help = screen_usage("lg-buddy");

        assert!(help.contains("lg-buddy screen off"));
        assert!(help.contains("lg-buddy screen on"));
        assert!(!help.contains("screen-off"));
        assert!(!help.contains("screen-on"));
    }

    #[test]
    fn settings_usage_is_scoped_to_the_requested_level() {
        let root = settings_usage("lg-buddy", SettingsHelpTopic::Root);
        assert!(root.contains("lg-buddy settings list"));
        assert!(root.contains("lg-buddy settings describe [KEY]"));
        assert!(root.contains("set <KEY> <VALUE>"));

        let set = settings_usage("lg-buddy", SettingsHelpTopic::Set);
        assert!(set.contains("lg-buddy settings set <KEY> <VALUE>"));
        assert!(!set.contains("settings list"));
    }

    #[test]
    fn updates_usage_is_scoped_and_hides_the_timer_entrypoint() {
        let root = updates_usage("lg-buddy", UpdatesHelpTopic::Root);
        assert!(root.contains("lg-buddy updates check [--notify]"));
        assert!(root.contains("lg-buddy updates install"));
        assert!(!root.contains("--channel"));
        assert!(!root.contains("background-check"));

        let check = updates_usage("lg-buddy", UpdatesHelpTopic::Check);
        assert!(!check.contains("--channel"));
        assert!(check.contains("--notify"));
        assert!(!check.contains("background-check"));

        let install = updates_usage("lg-buddy", UpdatesHelpTopic::Install);
        assert!(install.contains("lg-buddy updates install"));
        assert!(install.contains("saved updates.channel"));
        assert!(!install.contains("--channel"));
    }

    #[test]
    fn usage_mentions_settings_commands_without_reserved_notice() {
        let help = usage("lg-buddy");

        for command in ["brightness get", "brightness set <0-100>"] {
            assert!(help.contains(command), "missing `{command}` from help");
        }

        for command in [
            "--version, -V",
            "settings list",
            "settings describe [KEY]",
            "settings get <KEY>",
            "settings set <KEY> <VALUE>",
            "settings unset <KEY>",
            "updates check [--notify]",
        ] {
            assert!(help.contains(command), "missing `{command}` from help");
        }
        assert!(!help.contains("Reserved for write support"));
    }

    #[test]
    fn notification_context_preserves_primary_run_error_source() {
        let err = RunError::NotificationAfterPrimary {
            primary: Box::new(RunError::Io(io::Error::other("disk unavailable"))),
            notification: NotificationError::Transport("bus unavailable".to_string()),
        };

        assert_eq!(
            err.to_string(),
            "disk unavailable; additionally, desktop notification failed: desktop notification service error: bus unavailable"
        );
        assert_eq!(
            err.source()
                .expect("primary source should be preserved")
                .to_string(),
            "disk unavailable"
        );
    }

    fn brightness(value: u8) -> OledBrightness {
        OledBrightness::new(value).expect("test brightness should be valid")
    }

    fn volume(value: u8) -> VolumeLevel {
        VolumeLevel::new(value).expect("test volume should be valid")
    }

    #[test]
    fn requires_current_config_covers_tv_operating_commands_only() {
        use super::requires_current_config;
        let gated = [
            Command::Startup(StartupMode::Boot),
            Command::Shutdown,
            Command::Power(PowerCommand::On),
            Command::Power(PowerCommand::Off),
            Command::SleepPre,
            Command::Sleep,
            Command::Screen(ScreenCommand::Off),
            Command::Screen(ScreenCommand::On),
            Command::ScreenOff,
            Command::ScreenOn,
            Command::Monitor,
            Command::Lifecycle,
            Command::NetworkManagerPreDown,
            Command::Volume(VolumeCommand::Mute(MuteCommand::Toggle)),
            Command::Brightness(BrightnessCommand::Set(brightness(42))),
        ];
        for command in &gated {
            assert!(
                requires_current_config(command),
                "{command:?} should be gated"
            );
        }
        // The migration host and the GUI-forwarding brightness prompt stay
        // available on a stale config.
        let ungated = [
            Command::Brightness(BrightnessCommand::Prompt),
            Command::Dev(DevCommand::WebOsAuthProbe),
            Command::DetectBackend,
            Command::Settings(SettingsCommand::List),
            Command::Updates(UpdatesCommand::Install),
            Command::UpgradePreflight {
                candidate_root: PathBuf::from("/tmp/lgbuddy-preflight-candidate"),
                remove_legacy_env: true,
                json: true,
            },
        ];
        for command in &ungated {
            assert!(
                !requires_current_config(command),
                "{command:?} should not be gated"
            );
        }
    }

    // ---- pure UNIX address validator (migration 257, Slice A) ----

    #[test]
    fn validator_accepts_valid_unix_path_abstract_and_guid_forms() {
        assert!(is_valid_probe_address("unix:path=/tmp/bus.sock"));
        assert!(is_valid_probe_address("unix:path=/run/user/1000/bus"));
        assert!(is_valid_probe_address("unix:abstract=lg-buddy-probe"));
        // Optional guid, with and without an endpoint key order.
        assert!(is_valid_probe_address(
            "unix:path=/tmp/bus.sock,guid=0123456789abcdef0123456789ABCDEF"
        ));
        assert!(is_valid_probe_address(
            "unix:abstract=a,guid=ffffffffffffffffffffffffffffffff"
        ));
        // Escaped bytes that decode to valid values.
        assert!(is_valid_probe_address("unix:abstract=%41%42")); // decodes to "AB"
        assert!(is_valid_probe_address(
            "unix:path=/tmp/bus%20file.sock" // decodes to "/tmp/bus file.sock"
        ));
    }

    #[test]
    fn validator_rejects_semicolon_fallback_lists() {
        assert!(!is_valid_probe_address(
            "unix:path=/tmp/bus.sock;tcp:127.0.0.1:1"
        ));
        assert!(!is_valid_probe_address("unix:path=/tmp/bus.sock;"));
        assert!(!is_valid_probe_address(
            "tcp:127.0.0.1:1;unix:path=/tmp/bus.sock"
        ));
    }

    #[test]
    fn validator_rejects_non_unix_transports() {
        assert!(!is_valid_probe_address("tcp:127.0.0.1:1234"));
        assert!(!is_valid_probe_address("tcp:host=127.0.0.1,port=1234"));
        assert!(!is_valid_probe_address("nonce-tcp:127.0.0.1:1234"));
        assert!(!is_valid_probe_address("unixexec:/usr/bin/fake-busd"));
        assert!(!is_valid_probe_address("autolaunch:"));
        assert!(is_valid_probe_address("unix:path=/tmp/bus.sock")); // control: valid
    }

    #[test]
    fn validator_rejects_wrong_or_missing_unix_prefix() {
        assert!(!is_valid_probe_address("unix"));
        assert!(!is_valid_probe_address("unix:"));
        assert!(is_valid_probe_address("unix:path=/tmp/bus.sock")); // control: valid
        assert!(!is_valid_probe_address("UNIX:path=/tmp/bus.sock"));
        assert!(!is_valid_probe_address(" unix:path=/tmp/bus.sock"));
        assert!(!is_valid_probe_address("unix:path=/tmp/bus.sock "));
    }

    #[test]
    fn validator_rejects_lookup_keys_and_unknown_keys() {
        assert!(!is_valid_probe_address("unix:runtime-dir=/run/user/1000"));
        assert!(!is_valid_probe_address("unix:tmpdir=/tmp"));
        assert!(!is_valid_probe_address("unix:dir=/tmp"));
        assert!(!is_valid_probe_address("unix:bogus=1"));
        assert!(!is_valid_probe_address("unix:path=/tmp/bus.sock,bogus=1"));
    }

    #[test]
    fn validator_rejects_duplicate_endpoint_or_guid_keys() {
        assert!(!is_valid_probe_address("unix:path=/a,path=/b"));
        assert!(!is_valid_probe_address("unix:abstract=a,abstract=b"));
        assert!(is_valid_probe_address(
            "unix:path=/a,guid=0123456789abcdef0123456789abcdef"
        )); // control: one guid ok
        assert!(!is_valid_probe_address(
            "unix:guid=0123456789abcdef0123456789abcdef,guid=ffffffffffffffffffffffffffff"
        ));
    }

    #[test]
    fn validator_rejects_both_endpoint_keys_and_absent_endpoint() {
        assert!(!is_valid_probe_address(
            "unix:path=/tmp/bus.sock,abstract=x"
        ));
        assert!(!is_valid_probe_address(
            "unix:abstract=x,path=/tmp/bus.sock"
        ));
        assert!(!is_valid_probe_address("unix:"));
        assert!(!is_valid_probe_address(
            "unix:guid=0123456789abcdef0123456789abcdef"
        ));
    }

    #[test]
    fn validator_rejects_empty_fields_or_values() {
        assert!(!is_valid_probe_address("unix:"));
        assert!(!is_valid_probe_address("unix="));
        assert!(!is_valid_probe_address("unix:path="));
        assert!(!is_valid_probe_address("unix:abstract="));
        assert!(!is_valid_probe_address("unix:path=/tmp/bus.sock,="));
        assert!(!is_valid_probe_address("unix:path=/tmp/bus.sock,"));
    }

    #[test]
    fn validator_rejects_bad_percent_escapes() {
        assert!(!is_valid_probe_address("unix:path=/tmp/%zz")); // not hex
        assert!(!is_valid_probe_address("unix:path=/tmp/%4")); // one hex digit
        assert!(!is_valid_probe_address("unix:path=/tmp/%")); // bare %
        assert!(!is_valid_probe_address("unix:path=/tmp/%2")); // short escape
        assert!(!is_valid_probe_address("unix:abstract=%zz")); // not hex
    }

    #[test]
    fn validator_rejects_decoded_nul() {
        assert!(!is_valid_probe_address("unix:abstract=%00"));
        assert!(!is_valid_probe_address("unix:path=/tmp/bus%00.sock"));
    }

    #[test]
    fn validator_rejects_raw_nul_and_unescaped_bytes() {
        // Raw NUL in the input string (as &str this is the '\0' character).
        assert!(!is_valid_probe_address("unix:path=/tmp/bus.sock\0"));
        assert!(!is_valid_probe_address("unix:abstract=a\0b"));
        // A non-literal, unescaped byte (space) must be escaped.
        assert!(!is_valid_probe_address("unix:path=/tmp/bus .sock"));
        // Non-UTF-8 bytes are preserved only when escaped; a raw high byte
        // (escaped as %FF) decodes fine, but the decoded value must be usable.
        assert!(is_valid_probe_address("unix:abstract=%FF%20"));
    }

    #[test]
    fn validator_requires_path_to_start_with_slash() {
        assert!(!is_valid_probe_address("unix:path=relative.sock"));
        assert!(!is_valid_probe_address("unix:path=..%2Ftmp%2Fbus.sock"));
        assert!(is_valid_probe_address("unix:path=/tmp/bus.sock")); // control
        assert!(is_valid_probe_address("unix:path=%2ftmp/bus.sock")); // decodes to /tmp/bus.sock
    }

    #[test]
    fn validator_rejects_bad_guid() {
        let good = "0123456789abcdef0123456789abcdef";
        assert!(is_valid_probe_address(&format!(
            "unix:path=/tmp/bus.sock,guid={good}"
        )));
        // Too short.
        assert!(!is_valid_probe_address(
            "unix:path=/tmp/bus.sock,guid=0123456789abcdef"
        ));
        // Too long.
        assert!(!is_valid_probe_address(
            "unix:path=/tmp/bus.sock,guid=0123456789abcdef0123456789abcdef0"
        ));
        // Non-hex digit.
        assert!(!is_valid_probe_address(
            "unix:path=/tmp/bus.sock,guid=0123456789abcdef0123456789abcdefg"
        ));
        // Uppercase hex is allowed.
        assert!(is_valid_probe_address(
            "unix:path=/tmp/bus.sock,guid=0123456789ABCDEF0123456789ABCDEF"
        ));
    }

    #[test]
    fn validator_escaped_non_utf8_bytes_preserved() {
        // %20 decodes to a space (valid in abstract); %FF decodes to 0xFF (valid byte).
        assert!(is_valid_probe_address("unix:abstract=%20%FF"));
        // Decoding must not be required for validation of a plain literal.
        assert!(is_valid_probe_address("unix:abstract=a-b_c/d*e.f"));
    }

    #[test]
    fn probe_missing_bus_is_unavailable_fast() {
        // No env read, no autolaunch, no real bus: a missing address is a
        // fast exit-1. This is the production wrapper (build is skipped).
        assert_eq!(run_gnome_readiness_probe(None), 1);
        assert_eq!(run_gnome_readiness_probe(Some("")), 1);
    }

    #[test]
    fn probe_parse_args_missing_bus_value_errors() {
        // `--bus` takes a value; a dangling `--bus` with no value errors.
        assert_eq!(
            parse_args(["gnome-readiness-probe", "--bus"]),
            Err(ParseError::MissingGnomeReadinessProbeBus)
        );
    }

    #[test]
    fn probe_parse_args_defaults_bus_address_to_none() {
        // `--bus` is optional; omitting it is a valid command with no address.
        assert_eq!(
            parse_args(["gnome-readiness-probe"]),
            Ok(ParseOutcome::Command(Command::GnomeReadinessProbe {
                bus_address: None,
            }))
        );
    }

    #[test]
    fn probe_parse_args_reads_bus_address() {
        assert_eq!(
            parse_args(["gnome-readiness-probe", "--bus", "unix:path=/tmp/bus.sock"]),
            Ok(ParseOutcome::Command(Command::GnomeReadinessProbe {
                bus_address: Some("unix:path=/tmp/bus.sock".to_string()),
            }))
        );
    }

    #[test]
    fn probe_parse_args_rejects_unknown_flags() {
        assert_eq!(
            parse_args(["gnome-readiness-probe", "--bogus"]),
            Err(ParseError::UnexpectedArguments {
                command: Command::GnomeReadinessProbe { bus_address: None },
                arguments: vec!["--bogus".to_string()],
            })
        );
    }

    #[test]
    fn probe_ready_bus_maps_to_exit_0_and_exercises_bus() {
        let (bus, observation, dropped) = ready_probe_bus();
        let stop = AtomicBool::new(false);
        assert_eq!(
            gnome_readiness_probe_exit_code(bus, &stop),
            0,
            "ready bus maps to exit 0"
        );
        // The handler must have driven the real readiness trace (not a stub),
        // observed through the shared state after the client was consumed.
        let trace = observation.lock().unwrap().calls.clone();
        let members: Vec<_> = trace.iter().map(|(m, _)| m.as_str()).collect();
        assert!(members.contains(&"GetNameOwner"));
        assert!(members.contains(&"AddUserActiveWatch"));
        assert!(members.contains(&"GetActive"));
        assert!(members.contains(&"GetIdletime"));
        assert!(members.contains(&"RemoveWatch"));
        // The owned client was consumed and dropped before the mapper returned.
        assert!(*dropped.lock().unwrap(), "owned client must be dropped");
    }

    #[test]
    fn probe_pre_cancelled_maps_to_exit_2() {
        let (bus, observation, dropped) = ready_probe_bus();
        let stop = AtomicBool::new(true);
        assert_eq!(
            gnome_readiness_probe_exit_code(bus, &stop),
            2,
            "pre-cancelled maps to exit 2"
        );
        // Cancellation is checked before any bus call.
        assert!(
            observation.lock().unwrap().calls.is_empty(),
            "no bus calls after pre-cancel"
        );
        assert!(*dropped.lock().unwrap(), "owned client must be dropped");
    }

    #[test]
    fn probe_services_unavailable_maps_to_exit_3() {
        let (mut bus, _observation, dropped) = ready_probe_bus();
        bus.services = false;
        let stop = AtomicBool::new(false);
        assert_eq!(
            gnome_readiness_probe_exit_code(bus, &stop),
            3,
            "services-unavailable maps to exit 3 (not-ready)"
        );
        assert!(*dropped.lock().unwrap(), "owned client must be dropped");
    }

    #[test]
    fn probe_trivial_ok_client_cannot_pass_ready_case() {
        // Negative gate: a stub that always returns Ok and never reads the
        // address must NOT be accepted as a ready bus. The readiness check
        // still drives the real trace, so a non-trivial client is required —
        // a `TrivialOkBus` fails the owner-resolution step (no owner reply),
        // so the handler reports not-ready (3), not ready (0).
        let bus = TrivialOkBus;
        let stop = AtomicBool::new(false);
        assert_eq!(
            gnome_readiness_probe_exit_code(bus, &stop),
            3,
            "a trivial always-Ok stub must not be treated as ready"
        );
    }

    #[test]
    fn probe_unsupported_address_maps_to_unavailable_via_production_wrapper() {
        // End-to-end through the REAL production wrapper (no fake connector):
        // an unsupported explicit address is rejected by the validator BEFORE
        // any connector is invoked and maps to Unavailable (exit 1). No real
        // bus is opened and no autolaunch/exec is attempted.
        assert_eq!(run_gnome_readiness_probe(Some("not-a-dbus-address")), 1);
    }

    #[test]
    fn probe_valid_addresses_reach_connector_once_and_exercise_readiness() {
        // Connector-injection seam, table form: every supported address shape
        // (guid before/after the endpoint, abstract, percent-escaped path
        // delimiters and non-UTF8 bytes) must reach the connector exactly once,
        // byte-for-byte, and the readiness trace is actually driven — observed
        // through the shared handles after the probe consumed the client.
        let cases = [
            "unix:path=/tmp/bus.sock",
            "unix:guid=0123456789abcdef0123456789abcdef,path=/tmp/bus.sock",
            "unix:path=/tmp/bus.sock,guid=0123456789abcdef0123456789abcdef",
            "unix:abstract=lg-buddy-probe",
            "unix:abstract=lg-buddy-probe,guid=0123456789abcdef0123456789abcdef",
            "unix:path=/tmp/a%2Cb.sock",
            "unix:path=/tmp/a%3Bb.sock",
            "unix:path=/tmp/a%3Db.sock",
            "unix:path=/tmp/a%25b.sock",
            "unix:path=%2Ftmp%2Fbus.sock",
            "unix:path=/tmp/bus%FF.sock", // escaped non-UTF8 byte, still absolute
        ];
        for address in cases {
            let call_count = Arc::new(Mutex::new(0usize));
            let seen = Arc::new(Mutex::new(Vec::new()));
            // The connector's bus stashes its shared handles here so the
            // trace and drop state are inspectable after the client is gone.
            let bus_state = Arc::new(Mutex::new(None));
            let state = Arc::clone(&bus_state);
            let counter = Arc::clone(&call_count);
            let observed = Arc::clone(&seen);
            let result = run_gnome_readiness_probe_with_connector(Some(address), move |addr| {
                *counter.lock().unwrap() += 1;
                observed.lock().unwrap().push(addr.to_string());
                let (bus, observation, dropped) = ready_probe_bus();
                *state.lock().unwrap() = Some((Arc::clone(&observation), Arc::clone(&dropped)));
                Ok(bus)
            });
            assert_eq!(result, 0, "{address}: a ready connector maps to exit 0");
            assert_eq!(
                *call_count.lock().unwrap(),
                1,
                "{address}: connector invoked exactly once"
            );
            let seen_calls = {
                let g = seen.lock().unwrap();
                g.as_slice() == [address]
            };
            assert!(seen_calls, "{address}: address passed byte-for-byte");
            let (observation, dropped) = bus_state
                .lock()
                .unwrap()
                .take()
                .expect("a valid address must run the connector");
            let trace = observation.lock().unwrap().calls.clone();
            let members: Vec<_> = trace.iter().map(|(m, _)| m.as_str()).collect();
            assert!(members.contains(&"GetNameOwner"), "{address}");
            assert!(members.contains(&"AddUserActiveWatch"), "{address}");
            assert!(members.contains(&"GetActive"), "{address}");
            assert!(members.contains(&"GetIdletime"), "{address}");
            assert!(members.contains(&"RemoveWatch"), "{address}");
            assert!(
                *dropped.lock().unwrap(),
                "{address}: owned client must be dropped"
            );
        }
    }

    #[test]
    fn probe_rejected_addresses_never_call_connector() {
        // Dangerous/unsupported samples (autolaunch, exec transport, TCP,
        // fallback lists, malformed and missing/empty forms) are rejected by
        // the validator BEFORE the connector, so a fake connector is never
        // invoked — no real autolaunch/exec runs.
        let call_count = Arc::new(Mutex::new(0usize));
        let rejected = [
            // missing / empty / malformed
            "",
            "unix:",
            "unix",
            "UNIX:path=/tmp/bus.sock",
            " unix:path=/tmp/bus.sock",
            // unsupported transports and fallback lists
            "autolaunch:",
            "unixexec:/usr/bin/fake-busd",
            "tcp:127.0.0.1:1",
            "nonce-tcp:127.0.0.1:1",
            "unix:path=/tmp/bus.sock;tcp:127.0.0.1:1",
            // runtime/tmpdir/dir lookup keys and unknown keys
            "unix:runtime-dir=/run/user/1000",
            "unix:tmpdir=/tmp",
            "unix:dir=/tmp",
            "unix:bogus=1",
            "unix:path=/tmp/bus.sock,bogus=1",
            // both endpoint kinds, duplicate endpoint keys, relative path
            "unix:path=/tmp/bus.sock,abstract=probe",
            "unix:path=/tmp/bus.sock,path=/tmp/other.sock",
            "unix:abstract=a,abstract=b",
            "unix:path=relative.sock",
            // empty fields
            "unix:path=",
            "unix:abstract=",
            "unix:path=/tmp/bus.sock,",
            "unix:path=/tmp/bus.sock,=",
            // bad percent escapes, decoded NUL, bad guid
            "unix:path=/tmp/%zz",
            "unix:path=/tmp/%4",
            "unix:path=/tmp/bus%00.sock",
            "unix:guid=0123456789abcdef0123456789abc,path=/tmp/bus.sock", // 31 hex
            // duplicate GUID with a VALID endpoint and TWO individually
            // valid 32-hex GUIDs: proves duplicate-key rejection on its own.
            "unix:guid=0123456789abcdef0123456789abcdef,guid=abcdefabcdefabcdefabcdefabcdefab,path=/tmp/bus.sock",
        ];
        for addr in rejected {
            assert_eq!(
                run_gnome_readiness_probe_with_connector(Some(addr), |_| {
                    *call_count.lock().unwrap() += 1;
                    Err::<TrivialOkBus, _>(SessionBusError::Transport(
                        "rejected address must not reach the connector".into(),
                    ))
                }),
                1,
                "{addr:?} is rejected as Unavailable (exit 1)"
            );
        }
        assert_eq!(
            *call_count.lock().unwrap(),
            0,
            "no rejected sample may reach the connector"
        );
    }

    #[test]
    fn probe_connector_error_maps_to_unavailable() {
        // A VALID address whose connector fails acquisition still maps to
        // Unavailable (exit 1): the seam connects exactly once, the client is
        // never produced, and readiness is never driven.
        let call_count = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&call_count);
        let result = run_gnome_readiness_probe_with_connector(
            Some("unix:path=/tmp/bus.sock"),
            move |addr| {
                *counter.lock().unwrap() += 1;
                assert_eq!(addr, "unix:path=/tmp/bus.sock");
                Err::<TrivialOkBus, _>(SessionBusError::Transport(
                    "fake acquisition failure".into(),
                ))
            },
        );
        assert_eq!(
            result, 1,
            "acquisition failure maps to Unavailable (exit 1)"
        );
        assert_eq!(
            *call_count.lock().unwrap(),
            1,
            "connector invoked exactly once"
        );
    }

    #[test]
    fn probe_error_exit_codes_are_stable() {
        assert_eq!(GnomeReadinessProbeError::Unavailable.exit_code(), 1);
        assert_eq!(GnomeReadinessProbeError::Cancelled.exit_code(), 2);
        assert_eq!(GnomeReadinessProbeError::NotReady.exit_code(), 3);
    }

    #[test]
    fn probe_run_command_dispatches_missing_bus_as_unavailable() {
        // Really dispatches the gnome-readiness-probe command with a missing
        // `--bus` value OR an empty one: run_command takes the fast
        // Unavailable path (exit 1) and surfaces it as the typed RunError —
        // no config read, no runtime, no bus connection is opened.
        for bus_address in [None, Some("".to_string())] {
            let mut out = Vec::new();
            let dispatched =
                crate::run_command(Command::GnomeReadinessProbe { bus_address }, &mut out);
            assert!(
                matches!(
                    dispatched,
                    Err(RunError::GnomeReadinessProbe(
                        GnomeReadinessProbeError::Unavailable
                    ))
                ),
                "a missing or empty bus address must dispatch as Unavailable"
            );
        }
    }
}
