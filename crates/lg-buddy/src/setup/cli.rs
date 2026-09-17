//! Terminal rendering and input for the backend-owned onboarding flow.
use super::{
    flow::{AuthorizationMode, FlowOutcome, OnboardingFlow, StepAnswer},
    StepFailure, StepInput, StepResponse,
};
use crate::{config::HdmiInput, pairing::PairingRequest, HelpTopic, ParseError, ParseOutcome};
use std::io::{self, BufRead, IsTerminal, Write};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupOptions {
    pub noninteractive: bool,
    pub yes: bool,
    pub allow_build_dependencies: bool,
    pub address: Option<String>,
    pub mac: Option<String>,
    pub input: Option<HdmiInput>,
}

pub(crate) fn parse(args: impl Iterator<Item = String>) -> Result<ParseOutcome, ParseError> {
    let mut options = SetupOptions::default();
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(ParseOutcome::Help(HelpTopic::Setup)),
            "--non-interactive" => options.noninteractive = true,
            "--yes" | "-y" => options.yes = true,
            "--allow-build-dependencies" => {
                options.allow_build_dependencies = true;
            }
            "--tv-ip" | "--tv-mac" | "--input" => {
                let value = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| ParseError::Setup(format!("{arg} requires a value")))?;
                match arg.as_str() {
                    "--tv-ip" if options.address.is_none() => options.address = Some(value),
                    "--tv-mac" if options.mac.is_none() => options.mac = Some(value),
                    "--input" if options.input.is_none() => {
                        options.input = Some(value.parse().map_err(|_| {
                            ParseError::Setup(
                                "--input must be HDMI_1, HDMI_2, HDMI_3 or HDMI_4".into(),
                            )
                        })?)
                    }
                    _ => return Err(ParseError::Setup(format!("duplicate option {arg}"))),
                }
            }
            _ => return Err(ParseError::Setup(format!("unexpected setup option {arg}"))),
        }
    }
    Ok(ParseOutcome::Command(crate::Command::Setup(options)))
}

pub(crate) fn usage(program: &str) -> String {
    format!(
        "\
Complete LG Buddy setup or repair an existing installation.

Usage: {program} setup [OPTIONS]

  --tv-ip ADDRESS              TV IPv4 address
  --tv-mac ADDRESS             TV network MAC address
  --input HDMI_1..HDMI_4        PC input (default: saved input or HDMI_1)
  --yes, -y                    Approve pairing and required service/integration work
  --allow-build-dependencies   Also approve compiler/development package installation
  --non-interactive            Never read input or request a password
  --help, -h                   Show this help

Completed steps are skipped. Saved settings are preserved; edit them with settings.
Interactive authorization uses sudo in the terminal. Noninteractive authorization
requires existing sudo permission. No graphical authorization dialogs are opened.
Without terminal input, noninteractive mode is automatic. --yes never grants
permission to install build dependencies; that requires its separate option.
Exit status: 0 complete, 1 failed/blocked, 2 invalid arguments, 3 input needed,
130 cancelled. Completed work is retained; rerun setup to continue."
    )
}

#[derive(Debug)]
pub enum SetupError {
    Incomplete(&'static str),
    Cancelled,
    Failed(StepFailure),
    Io(io::Error),
}
impl SetupError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Incomplete(_) => 3,
            Self::Cancelled => 130,
            _ => 1,
        }
    }
}
impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incomplete(message) => write!(f, "Setup incomplete. {message}"),
            Self::Cancelled => write!(
                f,
                "Setup cancelled. Completed work was retained; rerun setup to continue."
            ),
            Self::Failed(error) => write!(
                f,
                "{}: {}\nDetails: {}",
                error.presentation.summary(),
                error.presentation.detail(),
                error.diagnostic
            ),
            Self::Io(error) => write!(f, "Setup could not continue: {error}"),
        }
    }
}
impl std::error::Error for SetupError {}
impl From<io::Error> for SetupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn run(options: SetupOptions, writer: &mut impl Write) -> Result<(), SetupError> {
    let interactive = !options.noninteractive && io::stdin().is_terminal();
    let mut flow = OnboardingFlow::open(if interactive {
        AuthorizationMode::Terminal
    } else {
        AuthorizationMode::Noninteractive
    })
    .map_err(SetupError::Failed)?;
    let signals = super::terminal_signals::CancellationSignals::install(flow.cancellation())?;
    let result = render(
        &mut flow,
        &options,
        interactive,
        &mut io::BufReader::new(super::terminal_signals::TerminalInput),
        writer,
    );
    drop(signals);
    result
}

pub(super) fn render(
    flow: &mut OnboardingFlow,
    options: &SetupOptions,
    interactive: bool,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<(), SetupError> {
    let cancellation = flow.cancellation();
    writeln!(writer, "LG Buddy setup")?;
    loop {
        let snapshot = flow.snapshot();
        match snapshot.outcome {
            FlowOutcome::Complete => {
                writeln!(writer, "Setup complete.")?;
                return Ok(());
            }
            FlowOutcome::Cancelled => return Err(SetupError::Cancelled),
            FlowOutcome::Incomplete => {}
        }
        let (_, response) = snapshot
            .current()
            .expect("incomplete flow has remaining work");
        let answer = match response {
            StepResponse::InputRequired(StepInput::Pairing { saved }) => {
                writeln!(
                    writer,
                    "Pair a TV. Turn it on and approve the connection request on the TV."
                )?;
                let address = options
                    .address
                    .clone()
                    .or_else(|| saved.map(|s| s.address().to_string()));
                let mac = options
                    .mac
                    .clone()
                    .or_else(|| saved.map(|s| s.mac().to_string()));
                let input = options
                    .input
                    .or_else(|| saved.map(|s| s.input()))
                    .unwrap_or(HdmiInput::Hdmi1);
                let request = loop {
                    let (address, mac, input) = if interactive {
                        (
                            prompt(reader, writer, "TV IP address", address.as_deref())?,
                            prompt(reader, writer, "TV MAC address", mac.as_deref())?,
                            prompt(
                                reader,
                                writer,
                                "PC input (HDMI_1..HDMI_4)",
                                Some(input.as_str()),
                            )?,
                        )
                    } else {
                        (
                            address
                                .clone()
                                .ok_or(SetupError::Incomplete("Supply --tv-ip and --tv-mac."))?,
                            mac.clone()
                                .ok_or(SetupError::Incomplete("Supply --tv-ip and --tv-mac."))?,
                            input.as_str().into(),
                        )
                    };
                    let parsed = input
                        .parse()
                        .ok()
                        .and_then(|input| PairingRequest::parse(&address, &mac, input).ok());
                    if let Some(request) = parsed {
                        break request;
                    }
                    writeln!(writer, "Enter a valid IPv4 address, MAC address and HDMI_1, HDMI_2, HDMI_3 or HDMI_4.")?;
                    if !interactive {
                        return Err(SetupError::Incomplete("Correct the supplied TV details."));
                    }
                };
                consent(
                    options.yes,
                    interactive,
                    reader,
                    writer,
                    "Pair this TV?",
                    "Use --yes to approve pairing.",
                )?;
                StepAnswer::Pairing(request)
            }
            StepResponse::ActionRequired {
                explanation,
                requires_authorization,
            } => {
                writeln!(writer, "{explanation}")?;
                if *requires_authorization {
                    writeln!(
                        writer,
                        "Administrator permission is required.{}",
                        if interactive {
                            " sudo may ask for your password in this terminal."
                        } else {
                            " Existing sudo permission will be used."
                        }
                    )?;
                }
                consent(
                    options.yes,
                    interactive,
                    reader,
                    writer,
                    "Continue?",
                    "Use --yes to approve the required setup work.",
                )?;
                StepAnswer::Continue
            }
            StepResponse::InputRequired(StepInput::BuildDependencies { explanation }) => {
                writeln!(writer, "{explanation}")?;
                consent(
                    options.allow_build_dependencies,
                    interactive,
                    reader,
                    writer,
                    "Install build dependencies?",
                    "Use --allow-build-dependencies to approve development package installation.",
                )?;
                StepAnswer::InstallBuildDependencies
            }
            StepResponse::Failed(error) | StepResponse::Blocked(error) => {
                if !interactive || !error.retryable {
                    return Err(SetupError::Failed(error.clone()));
                }
                writeln!(writer, "{}", SetupError::Failed(error.clone()))?;
                consent(false, true, reader, writer, "Retry?", "")?;
                flow.refresh();
                continue;
            }
            StepResponse::Cancelled => return Err(SetupError::Cancelled),
            _ => unreachable!("only unresolved responses are current"),
        };
        if cancellation.can_cancel() && super::terminal_signals::interrupted() {
            cancellation.cancel();
        }
        let mut output_error = None;
        flow.advance(snapshot.token, answer, &mut |event| {
            if let StepResponse::Running { message, .. } = event.response {
                if let Err(error) = writeln!(writer, "{message}").and_then(|()| writer.flush()) {
                    output_error = Some(error);
                    cancellation.cancel();
                }
            }
        });
        if let Some(error) = output_error {
            return Err(error.into());
        }
    }
}

fn prompt(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    label: &str,
    default: Option<&str>,
) -> Result<String, SetupError> {
    if super::terminal_signals::interrupted() {
        return Err(SetupError::Cancelled);
    }
    write!(
        writer,
        "{label}{} (q to cancel): ",
        default.map(|v| format!(" [{v}]")).unwrap_or_default()
    )?;
    writer.flush()?;
    let mut reply = String::new();
    match reader.read_line(&mut reply) {
        Ok(0) => return Err(SetupError::Cancelled),
        Err(_) if super::terminal_signals::interrupted() => return Err(SetupError::Cancelled),
        Err(error) => return Err(error.into()),
        _ => {}
    }
    if super::terminal_signals::interrupted() || reply.trim().eq_ignore_ascii_case("q") {
        return Err(SetupError::Cancelled);
    }
    Ok(if reply.trim().is_empty() {
        default.unwrap_or("").into()
    } else {
        reply.trim().into()
    })
}
fn consent(
    approved: bool,
    interactive: bool,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    question: &str,
    missing: &'static str,
) -> Result<(), SetupError> {
    if approved {
        return Ok(());
    }
    if !interactive {
        return Err(SetupError::Incomplete(missing));
    }
    loop {
        match prompt(reader, writer, &format!("{question} [y/N]"), Some("n"))?
            .to_ascii_lowercase()
            .as_str()
        {
            "y" | "yes" => return Ok(()),
            "n" | "no" => return Err(SetupError::Cancelled),
            _ => writeln!(writer, "Please answer yes or no.")?,
        }
    }
}

#[cfg(test)]
mod tests;
