use std::env;
use std::process::ExitCode;

use lg_buddy::{parse_args, run_command, usage, ParseOutcome};

fn main() -> ExitCode {
    let program = env::args().next().unwrap_or_else(|| "lg-buddy".to_string());

    match parse_args(env::args().skip(1)) {
        Ok(ParseOutcome::Help) => {
            print!("{}", usage(&program));
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Command(command)) => match run_command(command, &mut std::io::stdout()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("LG Buddy: {err}");
                ExitCode::from(1)
            }
        },
        Err(err) => {
            eprintln!("LG Buddy: {err}");
            eprintln!();
            eprint!("{}", usage(&program));
            ExitCode::from(2)
        }
    }
}
