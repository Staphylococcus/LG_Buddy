use std::env;
use std::process::ExitCode;

use lg_buddy::{help, parse_args, run_command, version, ParseOutcome};

fn main() -> ExitCode {
    let program = env::args().next().unwrap_or_else(|| "lg-buddy".to_string());

    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.first().is_some_and(|arg| arg == "kwin-setup") {
        return ExitCode::from(lg_buddy::setup::run_kwin_setup(&arguments[1..]));
    }

    match parse_args(arguments) {
        Ok(ParseOutcome::Help(topic)) => {
            print!("{}", help(&program, topic));
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Version) => {
            print!("{}", version::version_text());
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Command(command)) => match run_command(command, &mut std::io::stdout()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("LG Buddy: {err}");
                ExitCode::from(match err {
                    lg_buddy::RunError::Setup(error) => error.exit_code(),
                    lg_buddy::RunError::GnomeReadinessProbe(error) => error.exit_code(),
                    _ => 1,
                })
            }
        },
        Err(err) => {
            let topic = err.help_topic();
            eprintln!("LG Buddy: {err}");
            eprintln!();
            eprint!("{}", help(&program, topic));
            ExitCode::from(2)
        }
    }
}
