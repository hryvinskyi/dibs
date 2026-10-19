//! The `dibs-server` binary: serves when given no command, and runs the operator's commands on its
//! database otherwise. Every command reads its configuration from the environment; a configuration
//! it refuses exits with status 2 and a sentence naming the variable at fault, and a command that
//! refuses or fails exits with status 1.

use std::process::ExitCode;

use clap::Parser;

use dibs_server::cli::Cli;
use dibs_server::commands::run;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = error.exit_code();
            if error.print().is_err() {
                eprintln!("dibs-server: the command line is not valid.");
            }
            return ExitCode::from(u8::try_from(code).unwrap_or(2));
        }
    };
    run(cli, |var| std::env::var(var).ok()).await
}
