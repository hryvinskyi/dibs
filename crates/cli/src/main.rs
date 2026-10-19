//! The `dibs` command-line binary and its composition root: it reads the environment once, builds
//! the client from it, runs one command, and exits `0` when it succeeded, `1` when the service
//! refused, `2` for a usage or configuration error, `3` when the server could not be reached.

mod cli;
mod commands;
mod context;
mod failure;
mod git;
mod mcp;
mod output;

use std::collections::HashMap;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;
use crate::context::Context;
use crate::failure::Failure;
use crate::output::Printer;

/// The variable that turns on diagnostic logging to standard error (`debug`, `info`, …).
const LOG_VAR: &str = "DIBS_LOG";

fn main() -> ExitCode {
    let cli = Cli::parse();
    let env: HashMap<String, String> = std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    if let Some(level) = env
        .get(LOG_VAR)
        .and_then(|level| level.parse::<tracing::Level>().ok())
    {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_max_level(level)
            .init();
    }
    let printer = Printer::new(cli.json);
    match run(cli, env, printer) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            printer.failure(&failure);
            failure.exit_code()
        }
    }
}

fn run(cli: Cli, env: HashMap<String, String>, printer: Printer) -> Result<(), Failure> {
    let cwd = std::env::current_dir()
        .map_err(|_| Failure::config("no_directory", "The current directory cannot be read."))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Failure::config("no_runtime", "The command could not start its runtime."))?;
    let ctx = Context::new(env, cwd, printer);
    runtime.block_on(commands::run(cli.command, &ctx))
}
