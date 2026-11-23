//! Conversations: `send`, `inbox`, `thread`, `read` and `ack`.

use std::fmt::Write as _;
use std::io::Read;

use time::format_description::well_known::Rfc3339;

use dibs_client::api::{
    EscalationRequest, InboxRequest, Message, NoSessionAtWork, ReadTogether, Recipients,
    StartThread,
};
use dibs_domain::escalation::EscalationSubject;
use dibs_domain::message::Body;

use crate::cli::{AckArgs, EscalateArgs, InboxArgs, ReadArgs, SendArgs, ThreadArgs};
use crate::context::Context;
use crate::failure::{Failure, sentence};
use crate::output::{asking_text, author_text, thread_line, unacked_line, unread_line};

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
            importance: args.importance,
            ack_required: args.ack,
        })
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&started, || {
        let mut text = format!(
            "Started conversation #{}: {}.{}",
            started.thread.id,
            started.thread.subject,
            asking_text(&started.message)
        );
        for away in &started.no_session_at_work {
            text.push('\n');
            text.push_str(&no_session_text(away));
        }
        text
    });
    Ok(())
}

/// What `dibs send` says of a recipient none of whose sessions was seen within the last hour: the
/// message waits for their next session, and, when they hold a live claim, how to ask them for an
/// area rather than wait.
fn no_session_text(away: &NoSessionAtWork) -> String {
    let member = &away.member;
    let mut text = format!("{member} has no session at work now; it waits for their next session.");
    if away.holds_live_claim {
        let _ = write!(
            text,
            " To get an area {member} holds, ask for a grant: dibs ask-grant <your item> --area <area> (MCP: dibs_ask_grant). Their agent may decide it, and otherwise dibs grants it once the project's grant_after has passed."
        );
    }
    text
}

/// Asks the project's arbiters to settle a dispute about a work item, a slot or a conversation.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn escalate(args: EscalateArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let subject = match (&args.item, args.slot, args.thread) {
        (Some(key), _, _) => EscalationSubject::Item(connection.resolve(key).await?),
        (None, Some(slot), _) => EscalationSubject::Slot(slot),
        (None, None, Some(thread)) => EscalationSubject::Thread(thread),
        (None, None, None) => {
            return Err(Failure::config(
                "no_subject",
                "Name what to escalate: a work item, --slot or --thread.",
            ));
        }
    };
    let sent = connection
        .client
        .escalate(&EscalationRequest::about(subject, args.reason))
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&sent, || {
        let escalation = &sent.escalation;
        let names = |handles: &[dibs_domain::handle::Handle]| {
            handles
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "Escalated in conversation #{} to {} (arbiters: {}). It stands until an arbiter acknowledges it; meanwhile pick other work, and follow their answer there (dibs thread {}).",
            escalation.thread_id,
            names(&escalation.reached),
            names(&escalation.arbiters),
            escalation.thread_id
        )
    });
    Ok(())
}

/// Lists the conversations waiting for the caller and, when asked, the messages awaiting their
/// acknowledgement.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn inbox(args: InboxArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let inbox = connection
        .client
        .inbox(InboxRequest {
            unread: args.unread,
            unacked: args.unacked,
            deliverable: false,
        })
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&inbox, || {
        let mut text = unread_line(inbox.unread());
        if inbox.threads.is_empty() {
            text.push_str("\nNo conversations are waiting for you.");
        }
        for summary in &inbox.threads {
            let _ = write!(text, "\n{}", thread_line(summary));
        }
        if args.unacked {
            if inbox.unacked.is_empty() {
                text.push_str("\nNo message waits for your acknowledgement.");
            } else {
                text.push_str("\nWaiting for your acknowledgement:");
                for unacked in &inbox.unacked {
                    let _ = write!(text, "\n{}", unacked_line(unacked));
                }
            }
        }
        text
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
                "\nmessage {}, {} at {at}{}:\n{}\n",
                message.id,
                author_text(message),
                marks(message),
                message.body
            );
            if let Some(acks) = &message.acks {
                let entries: Vec<String> = acks
                    .iter()
                    .map(|ack| match ack.acked_at {
                        Some(at) => format!(
                            "{} acked at {}",
                            ack.member,
                            at.format(&Rfc3339).unwrap_or_default()
                        ),
                        None => format!("{} waiting", ack.member),
                    })
                    .collect();
                let _ = writeln!(text, "Acknowledgements: {}.", entries.join(", "));
            }
        }
        text
    });
    Ok(())
}

/// Marks a conversation read, or together every message dibs wrote or every message about a work
/// item, that the caller may read.
///
/// # Errors
/// The [`Failure`] of the call.
pub async fn read(args: ReadArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    if let Some(thread) = args.id {
        let marked = connection
            .client
            .mark_read(thread)
            .await
            .map_err(|error| connection.failed(error))?;
        ctx.printer.show(&marked, || {
            format!(
                "Marked #{thread} read ({} new messages).",
                marked.newly_read
            )
        });
        return Ok(());
    }
    let together = match (args.dibs, &args.item) {
        (true, _) => ReadTogether::FromDibs,
        (false, Some(key)) => ReadTogether::Item(connection.resolve(key).await?),
        (false, None) => {
            return Err(Failure::config(
                "nothing_to_read",
                "Name what to mark read: a conversation, --dibs or --item.",
            ));
        }
    };
    let marked = connection
        .client
        .mark_read_together(together)
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&marked, || {
        format!("Marked {} messages read.", marked.newly_read)
    });
    Ok(())
}

/// What a message's head says beside its author: its importance when raised, and that it asks
/// for an acknowledgement.
fn marks(message: &Message) -> String {
    let mut marks = Vec::new();
    if message.importance.is_raised() {
        marks.push(message.importance.to_string());
    }
    if message.ack_required {
        marks.push("asks for an ack".to_owned());
    }
    if marks.is_empty() {
        String::new()
    } else {
        format!(" [{}]", marks.join(", "))
    }
}

/// Acknowledges a message the caller was sent; acknowledging it again answers the first
/// acknowledgement.
///
/// # Errors
/// The [`Failure`] of the call: a refusal when the message asks for no acknowledgement or when the
/// caller is not one of its recipients. Acknowledging a message again is no failure.
pub async fn ack(args: AckArgs, ctx: &Context) -> Result<(), Failure> {
    let connection = ctx.connect().await?;
    let acked = connection
        .client
        .ack(args.id)
        .await
        .map_err(|error| connection.failed(error))?;
    ctx.printer.show(&acked, || {
        let mut text = if acked.created {
            let mut text = format!(
                "Acknowledged message {} in conversation #{}; its author can see it.",
                acked.ack.message, acked.ack.thread
            );
            for grant in &acked.granted {
                let _ = write!(
                    text,
                    " That grants {} edits in {} for item {}.",
                    grant.to, grant.area, grant.item
                );
            }
            text
        } else {
            format!(
                "You acknowledged message {} in conversation #{} already, at {}; nothing changed.",
                acked.ack.message,
                acked.ack.thread,
                acked.ack.at.format(&Rfc3339).unwrap_or_default()
            )
        };
        if acked.announcement_ended {
            text.push_str(
                " It announced a wide claim that has ended, so the acknowledgement grants nothing.",
            );
        }
        text
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use dibs_client::api::NoSessionAtWork;
    use dibs_domain::handle::Handle;

    use super::no_session_text;

    #[test]
    fn a_recipient_away_with_a_live_claim_is_asked_for_a_grant_dibs_decides_after_the_wait() {
        let text = no_session_text(&NoSessionAtWork {
            member: Handle::parse("bob").unwrap(),
            holds_live_claim: true,
        });
        assert!(
            text.contains("dibs ask-grant <your item> --area <area> (MCP: dibs_ask_grant)"),
            "{text}"
        );
        assert!(
            text.contains("otherwise dibs grants it once the project's grant_after has passed"),
            "{text}"
        );
        assert!(!text.contains("it waits until they do"), "{text}");
    }
}
