//! The `dibs` command-line binary and its composition root: it reads the environment once, builds
//! the client from it, runs one command, and exits `0` when it succeeded, `1` when the service
//! refused, `2` for a usage or configuration error, `3` when the server could not be reached.
//!
//! `dibs hook` is the exception: it answers an agent host, and its status is the host adapter's,
//! never `2`, whatever went wrong — a command line clap refuses included.

mod cli;
mod commands;
mod context;
mod failure;
mod git;
mod guard;
mod hosts;
mod mcp;
mod output;
mod release;

use std::collections::HashMap;
use std::ffi::OsString;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;
use clap::error::ErrorKind;

use crate::cli::{Cli, Command, Invocation};
use crate::context::Context;
use crate::failure::Failure;
use crate::output::Printer;

/// The variable that turns on diagnostic logging to standard error (`debug`, `info`, …).
const LOG_VAR: &str = "DIBS_LOG";

fn main() -> ExitCode {
    let started = Instant::now();
    remove_replaced_binary();
    let args: Vec<OsString> = std::env::args_os().collect();
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
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => {
            let shown = matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            );
            if let Some((host, event)) = commands::hook::hook_words(&args).filter(|_| !shown) {
                return commands::hook::hook(&host, &event, &env, started);
            }
            error.exit()
        }
    };
    let printer = Printer::new(cli.json);
    let command = match cli.command {
        Invocation::Hook(hook) => {
            return commands::hook::hook(&hook.host, &hook.event, &env, started);
        }
        Invocation::Doctor => {
            return match commands::doctor::doctor(&env, printer) {
                Ok(()) => ExitCode::SUCCESS,
                Err(failure) => {
                    printer.failure(&failure);
                    failure.exit_code()
                }
            };
        }
        Invocation::Service(command) => command,
    };
    match run(command, env, printer) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            printer.failure(&failure);
            failure.exit_code()
        }
    }
}

/// Deletes the binary a self-update renamed aside on Windows, which cannot delete a running
/// executable; on the next start it no longer runs.
#[cfg(windows)]
fn remove_replaced_binary() {
    if let Ok(binary) = std::env::current_exe() {
        dibs_client::update::install::remove_previous(&binary);
    }
}

/// Nothing is left aside where a running binary can be replaced in place.
#[cfg(not(windows))]
fn remove_replaced_binary() {}

fn run(command: Command, env: HashMap<String, String>, printer: Printer) -> Result<(), Failure> {
    let cwd = std::env::current_dir()
        .map_err(|_| Failure::config("no_directory", "The current directory cannot be read."))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Failure::config("no_runtime", "The command could not start its runtime."))?;
    let off = guard::switched_off(env.get(guard::OFF_VAR).map(String::as_str));
    let reports_after = command.reports_after(off);
    let ctx = Context::new(env, cwd, printer);
    let result = runtime.block_on(commands::run(command, &ctx));
    if reports_after {
        commands::sync::report_later(&ctx);
    }
    result
}
