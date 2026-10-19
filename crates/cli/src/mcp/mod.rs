//! `dibs mcp`: the operations of the command line as tools of a Model Context Protocol server on
//! standard input and output. The server is neutral about who calls it; it registers one session
//! of its own, labelled `<host>/mcp-<pid>`, and every claim it takes belongs to the member who
//! signed in.

mod answer;
mod inputs;
mod link;
mod tools;

use rmcp::ServiceExt;

use crate::context::Context;
use crate::failure::Failure;
use crate::mcp::tools::Tools;

/// Serves the tools until the client closes standard input.
///
/// The sign-in and the session registration start at once but do not hold up the handshake: a
/// server that is away at start is met again by the first call that needs it.
///
/// # Errors
/// A [`Failure`] when the transport cannot start or breaks.
pub async fn serve(ctx: &Context) -> Result<(), Failure> {
    let tools = Tools::new(ctx.clone());
    let opening = tools.clone();
    tokio::spawn(async move { opening.open().await });
    let service = tools
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| transport_failure(&error))?;
    service
        .waiting()
        .await
        .map_err(|error| transport_failure(&error))?;
    Ok(())
}

fn transport_failure(error: &dyn std::fmt::Display) -> Failure {
    tracing::debug!(error = %error, "the tool server's transport failed");
    Failure::config(
        "mcp_transport",
        "The tool server could not talk over standard input and output.",
    )
    .after_answer()
}
