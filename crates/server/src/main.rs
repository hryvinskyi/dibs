//! The `dibs-server` binary: reads its configuration from the environment, logs as JSON lines, and
//! serves until it is told to stop. A configuration it refuses exits with status 2 and a sentence
//! naming the variable at fault; a failure to start or serve exits with status 1.

use std::process::ExitCode;

use dibs_server::config::Config;
use dibs_server::serve::{serve, shutdown_signal};

#[tokio::main]
async fn main() -> ExitCode {
    let config = match Config::from_lookup(|var| std::env::var(var).ok()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("dibs-server: {error}.");
            return ExitCode::from(2);
        }
    };
    tracing_subscriber::fmt()
        .json()
        .with_max_level(config.log)
        .with_current_span(false)
        .with_span_list(false)
        .init();
    match serve(config, shutdown_signal()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = ?error, "the server stopped");
            eprintln!("dibs-server: {error}.");
            ExitCode::FAILURE
        }
    }
}
