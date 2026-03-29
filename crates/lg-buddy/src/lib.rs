pub mod config;
pub mod state;

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
            Self::ScreenOff => "TODO: implement screen-off command",
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
  screen-off      Placeholder screen-off command
  screen-on       Placeholder screen-on command
  detect-backend  Placeholder detect-backend command
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

pub fn run_command<W: Write>(command: Command, writer: &mut W) -> io::Result<()> {
    writeln!(writer, "{}", command.placeholder_message())
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
        run_command(Command::ScreenOff, &mut output).expect("write placeholder message");

        let rendered = String::from_utf8(output).expect("utf8 output");
        assert_eq!(rendered, "TODO: implement screen-off command\n");
    }
}
