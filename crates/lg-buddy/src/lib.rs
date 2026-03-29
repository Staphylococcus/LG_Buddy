pub mod backend;
pub mod commands;
pub mod config;
pub mod state;
pub mod tv;
pub mod wol;

use crate::backend::{
    configured_backend_from_env_or_config, detect_backend_from_system, BackendDetectionError,
    BackendSelectionError,
};
use crate::commands::run_screen_off;
use crate::config::{ConfigError, ConfigPathError};
use crate::state::StateDirError;
use std::fmt;
use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Startup,
    Shutdown,
    ScreenOff,
    ScreenOn,
    DetectBackend,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Help,
    Command(Command),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    UnknownCommand(String),
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
            Self::Startup => "startup",
            Self::Shutdown => "shutdown",
            Self::ScreenOff => "screen-off",
            Self::ScreenOn => "screen-on",
            Self::DetectBackend => "detect-backend",
        }
    }

    pub fn placeholder_message(self) -> &'static str {
        match self {
            Self::Startup => "TODO: implement startup command",
            Self::Shutdown => "TODO: implement shutdown command",
            Self::ScreenOff => "TODO: implemented via command handler",
            Self::ScreenOn => "TODO: implement screen-on command",
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
  startup         Placeholder startup command
  shutdown        Placeholder shutdown command
  screen-off      Blank the configured TV output if active
  screen-on       Placeholder screen-on command
  detect-backend  Detect the active screen backend
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
        "startup" => Command::Startup,
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
        Command::DetectBackend => run_detect_backend(writer),
        Command::ScreenOff => run_screen_off(writer),
        _ => {
            writeln!(writer, "{}", command.placeholder_message())?;
            Ok(())
        }
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
    use super::{parse_args, run_command, usage, Command, ParseError, ParseOutcome};

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
            Ok(ParseOutcome::Command(Command::Startup))
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
            parse_args(["startup", "boot"]),
            Err(ParseError::UnexpectedArguments {
                command: Command::Startup,
                arguments: vec!["boot".to_string()],
            })
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
    fn run_command_prints_placeholder_message() {
        let mut output = Vec::new();
        run_command(Command::Startup, &mut output).expect("write placeholder message");

        let rendered = String::from_utf8(output).expect("utf8 output");
        assert_eq!(rendered, "TODO: implement startup command\n");
    }
}
