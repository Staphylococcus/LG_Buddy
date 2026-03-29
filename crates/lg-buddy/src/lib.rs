pub mod backend;
pub mod commands;
pub mod config;
pub mod gnome;
pub mod session;
pub mod state;
pub mod tv;
pub mod wol;

use crate::backend::{
    configured_backend_from_env_or_config, detect_backend_from_system, BackendDetectionError,
    BackendSelectionError,
};
use crate::commands::{run_screen_off, run_screen_on, run_shutdown};
use crate::config::{ConfigError, ConfigPathError};
use crate::state::StateDirError;
use std::fmt;
use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Startup(StartupMode),
    Shutdown,
    ScreenOff,
    ScreenOn,
    DetectBackend,
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
    Help,
    Command(Command),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnknownCommand(String),
    UnknownStartupMode(String),
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
            Self::UnknownStartupMode(mode) => {
                write!(f, "unknown startup mode `{mode}`")
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
    Io(io::Error),
    Policy(String),
    ConfigPath(ConfigPathError),
    Config(ConfigError),
    StateDir(StateDirError),
    BackendSelection(BackendSelectionError),
    BackendDetection(BackendDetectionError),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Policy(err) => write!(f, "{err}"),
            Self::ConfigPath(err) => write!(f, "{err}"),
            Self::Config(err) => write!(f, "{err}"),
            Self::StateDir(err) => write!(f, "{err}"),
            Self::BackendSelection(err) => write!(f, "{err}"),
            Self::BackendDetection(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Policy(_) => None,
            Self::ConfigPath(err) => Some(err),
            Self::Config(err) => Some(err),
            Self::StateDir(err) => Some(err),
            Self::BackendSelection(err) => Some(err),
            Self::BackendDetection(err) => Some(err),
        }
    }
}

impl From<io::Error> for RunError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl Command {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Startup(_) => "startup",
            Self::Shutdown => "shutdown",
            Self::ScreenOff => "screen-off",
            Self::ScreenOn => "screen-on",
            Self::DetectBackend => "detect-backend",
        }
    }

    pub fn placeholder_message(self) -> &'static str {
        match self {
            Self::Startup(_) => "TODO: implemented via command handler",
            Self::Shutdown => "TODO: implemented via command handler",
            Self::ScreenOff => "TODO: implemented via command handler",
            Self::ScreenOn => "TODO: implemented via command handler",
            Self::DetectBackend => "TODO: implement detect-backend command",
        }
    }
}

pub fn usage(program: &str) -> String {
    format!(
        "\
LG Buddy Rust runtime

Usage:
  {program} <command>
  {program} --help

Commands:
  startup [mode]  Start or restore the TV output
  shutdown        Power off the TV when LG Buddy owns the active input
  screen-off      Blank the configured TV output if active
  screen-on       Restore the TV output after an LG Buddy screen-off
  detect-backend  Detect the active screen backend

Startup modes:
  auto            Restore on wake when LG Buddy owns the system marker, otherwise boot
  boot            Always treat startup as a cold boot
  wake            Only restore when LG Buddy owns the system marker
"
    )
}

pub fn parse_args<I, S>(args: I) -> Result<ParseOutcome, ParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(ParseOutcome::Help);
    };

    let first = first.as_ref();
    if matches!(first, "-h" | "--help" | "help") {
        return Ok(ParseOutcome::Help);
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
        "shutdown" => Command::Shutdown,
        "screen-off" => Command::ScreenOff,
        "screen-on" => Command::ScreenOn,
        "detect-backend" => Command::DetectBackend,
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

pub fn run_command<W: Write>(command: Command, writer: &mut W) -> Result<(), RunError> {
    match command {
        Command::Startup(mode) => crate::commands::run_startup(writer, mode),
        Command::Shutdown => run_shutdown(writer),
        Command::DetectBackend => run_detect_backend(writer),
        Command::ScreenOff => run_screen_off(writer),
        Command::ScreenOn => run_screen_on(writer),
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
    use super::{parse_args, usage, Command, ParseError, ParseOutcome, StartupMode};

    #[test]
    fn no_args_prints_help() {
        assert_eq!(parse_args(Vec::<String>::new()), Ok(ParseOutcome::Help));
    }

    #[test]
    fn explicit_help_prints_help() {
        assert_eq!(parse_args(["--help"]), Ok(ParseOutcome::Help));
        assert_eq!(parse_args(["-h"]), Ok(ParseOutcome::Help));
        assert_eq!(parse_args(["help"]), Ok(ParseOutcome::Help));
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
            parse_args(["screen-off"]),
            Ok(ParseOutcome::Command(Command::ScreenOff))
        );
        assert_eq!(
            parse_args(["screen-on"]),
            Ok(ParseOutcome::Command(Command::ScreenOn))
        );
        assert_eq!(
            parse_args(["detect-backend"]),
            Ok(ParseOutcome::Command(Command::DetectBackend))
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
    fn usage_mentions_all_commands() {
        let help = usage("lg-buddy");

        for command in [
            "startup",
            "shutdown",
            "screen-off",
            "screen-on",
            "detect-backend",
        ] {
            assert!(
                help.contains(command),
                "missing `{command}` from help output"
            );
        }
    }

    #[test]
    fn usage_mentions_startup_modes() {
        let help = usage("lg-buddy");

        for mode in ["auto", "boot", "wake"] {
            assert!(help.contains(mode), "missing startup mode `{mode}`");
        }
    }
}
