// Updates subcommand parsing: `UpdatesCommand` (check/install/background-check)
// and `UpdatesParseError`. Moved verbatim from updates.rs; only `notify` was
// promoted to `pub(super)` for the parent's call site.
use std::error::Error;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesCommand {
    Check { notify: bool },
    Install,
    BackgroundCheck,
}

impl UpdatesCommand {
    pub fn parse<I, S>(args: I) -> Result<Self, UpdatesParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = args.into_iter();
        let Some(subcommand) = args.next() else {
            return Err(UpdatesParseError::MissingSubcommand);
        };

        match subcommand.as_ref() {
            "check" => parse_check_args(args),
            "install" => parse_no_args("install", UpdatesCommand::Install, args),
            "background-check" => parse_background_check_args(args),
            other => Err(UpdatesParseError::UnknownSubcommand(other.to_string())),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Check { .. } => "check",
            Self::Install => "install",
            Self::BackgroundCheck => "background-check",
        }
    }

    pub(super) fn notify(&self) -> bool {
        match self {
            Self::Check { notify } => *notify,
            Self::Install => false,
            Self::BackgroundCheck => true,
        }
    }
}

fn parse_no_args<I, S>(
    subcommand: &'static str,
    command: UpdatesCommand,
    args: I,
) -> Result<UpdatesCommand, UpdatesParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let arguments = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect::<Vec<_>>();
    if arguments.is_empty() {
        Ok(command)
    } else {
        Err(UpdatesParseError::UnexpectedArguments {
            subcommand,
            arguments,
        })
    }
}

fn parse_background_check_args<I, S>(args: I) -> Result<UpdatesCommand, UpdatesParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let extra_args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_string())
        .collect();
    if extra_args.is_empty() {
        Ok(UpdatesCommand::BackgroundCheck)
    } else {
        Err(UpdatesParseError::UnexpectedArguments {
            subcommand: "background-check",
            arguments: extra_args,
        })
    }
}

fn parse_check_args<I, S>(args: I) -> Result<UpdatesCommand, UpdatesParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut args = args.into_iter();
    let mut notify = false;

    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--notify" => {
                if notify {
                    return Err(UpdatesParseError::DuplicateNotify);
                }

                notify = true;
            }
            other => {
                let mut unexpected = vec![other.to_string()];
                unexpected.extend(args.map(|arg| arg.as_ref().to_string()));
                return Err(UpdatesParseError::UnexpectedArguments {
                    subcommand: "check",
                    arguments: unexpected,
                });
            }
        }
    }

    Ok(UpdatesCommand::Check { notify })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesParseError {
    MissingSubcommand,
    UnknownSubcommand(String),
    DuplicateNotify,
    UnexpectedArguments {
        subcommand: &'static str,
        arguments: Vec<String>,
    },
}

impl fmt::Display for UpdatesParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSubcommand => write!(
                f,
                "missing updates command; expected `updates check [--notify]` or `updates install`"
            ),
            Self::UnknownSubcommand(subcommand) => {
                write!(f, "unknown updates command `{subcommand}`")
            }
            Self::DuplicateNotify => write!(f, "duplicate `--notify` option"),
            Self::UnexpectedArguments {
                subcommand,
                arguments,
            } => write!(
                f,
                "unexpected arguments for `updates {subcommand}`: {}",
                arguments.join(" ")
            ),
        }
    }
}

impl Error for UpdatesParseError {}
