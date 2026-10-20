//! `dibs mcp`: the operations of the command line as tools of a Model Context Protocol server on
//! standard input and output. The server is neutral about who calls it; it registers one session
//! of its own, labelled `<host>/mcp-<pid>`, and every claim it takes belongs to the member who
//! signed in. Started for a host that can wake a session waiting for its person, and opted in for
//! the session, it also pushes what would otherwise wait for the session's next tool call.

mod answer;
mod inputs;
mod link;
mod push;
mod tools;

pub use push::{PUSH_VAR, opted_in};

use futures_util::future::{self, Either};
use rmcp::ServiceExt;

use crate::cli::McpArgs;
use crate::context::Context;
use crate::failure::Failure;
use crate::mcp::tools::Tools;
use crate::{guard, hosts};

/// Serves the tools until the client closes standard input.
///
/// The sign-in and the session registration start at once but do not hold up the handshake: a
/// server that is away at start is met again by the first call that needs it.
///
/// # Errors
/// A [`Failure`] for a host this binary does not link, or when the transport cannot start or
/// breaks.
pub async fn serve(args: McpArgs, ctx: &Context) -> Result<(), Failure> {
    let opted = push::opted_in(ctx.var(push::PUSH_VAR).as_deref())
        && !guard::switched_off(ctx.var(guard::OFF_VAR).as_deref());
    let pusher = push::chosen(args.host.as_deref(), &hosts::registry(), opted, |name| {
        ctx.var(name)
    })?;
    let tools = Tools::new(ctx.clone(), pusher.clone());
    let opening = tools.clone();
    tokio::spawn(async move { opening.open().await });
    let service = tools
        .clone()
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|error| transport_failure(&error))?;
    let peer = service.peer().clone();
    let waiting = std::pin::pin!(service.waiting());
    let ended = match pusher {
        Some(pusher) => {
            let following = std::pin::pin!(push::follow(tools, peer, pusher));
            match future::select(waiting, following).await {
                Either::Left((ended, _)) => ended,
                Either::Right(((), waiting)) => waiting.await,
            }
        }
        None => waiting.await,
    };
    ended.map_err(|error| transport_failure(&error))?;
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
