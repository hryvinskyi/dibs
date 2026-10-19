//! Conversations: `send`, `inbox`, `thread` and `read`.

use std::fmt::Write as _;
use std::io::Read;

use time::format_description::well_known::Rfc3339;

use dibs_client::api::{Recipients, StartThread};
use dibs_domain::message::Body;

use crate::cli::{InboxArgs, SendArgs, ThreadArgs};
use crate::context::Context;
use crate::failure::{Failure, sentence};
use crate::output::thread_line;

/// Starts a conversation with members or the project; the body `-` is read from standard input.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn send(args: SendArgs, ctx: &Context) -> Result<(), Failure> {
    let text = if args.body == "-" {
        let mut text = String::new();
        std::io::stdin()
            .lock()
            .read_to_string(&mut text)
            .map_err(|_| {
                Failure::config(
                    "no_body",
                    "The message could not be read from standard input.",
                )
            })?;
        text
    } else {
        args.body
    };
    let body = Body::parse(&text).map_err(|error| {
        Failure::config(
            "invalid_body",
            sentence(&format!("the message is not valid: {error}")),
        )
    })?;
    let connection = ctx.connect().await?;
    let item = match &args.item {
        Some(key) => Some(connection.resolve(key).await?),
        None => None,
    };
    let to = if args.project {
        Recipients::Project
    } else {
        Recipients::Members(args.to)
    };
    let started = connection
        .client
        .start_thread(&StartThread {
            subject: args.subject,
            to,
            item,
            label: connection.label()?,
            body,
        })
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&started, || {
        format!(
            "Started conversation #{}: {}.",
            started.thread.id, started.thread.subject
        )
    });
    Ok(())
}

/// Lists the conversations waiting for the caller.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn inbox(args: InboxArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let threads = connection
        .client
        .inbox(args.unread)
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&threads, || {
        if threads.threads.is_empty() {
            return "No conversations are waiting for you.".to_owned();
        }
        threads
            .threads
            .iter()
            .map(thread_line)
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

/// Shows a conversation and its messages, oldest first. Showing it does not mark it read.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn thread(args: ThreadArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let detail = connection
        .client
        .thread(args.id)
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&detail, || {
        let mut text = format!("#{} {}\n", detail.thread.id, detail.thread.subject);
        for message in &detail.messages {
            let at = message.created_at.format(&Rfc3339).unwrap_or_default();
            let _ = write!(
                text,
                "\n{} ({}) at {at}:\n{}\n",
                message.author, message.label, message.body
            );
        }
        text
    });
    Ok(())
}

/// Marks a conversation read.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn read(args: ThreadArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let marked = connection
        .client
        .mark_read(args.id)
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&marked, || {
        format!(
            "Marked #{} read ({} new messages).",
            args.id, marked.newly_read
        )
    });
    Ok(())
}
