//! The client's answers as the guard reads them.

use dibs_agent_host::ports::{
    ClaimLine, HoldLine, InboxView, PathAnswer, PortError, ReadyLine, StatusView, TouchedBy,
    UnackedLine,
};
use dibs_client::ClientError;
use dibs_client::api::{HeldItem, Inbox, InboxRequest, NotReady, Status, Verdict};

use crate::failure::sentence;

/// The status as the guard reads it.
pub fn status(status: &Status) -> StatusView {
    let line = |held: &HeldItem| ClaimLine {
        item: held.item.id,
        reference: held.item.external_ref.clone(),
        title: held.item.title.clone(),
        holder: held.claim.holder.clone(),
        areas: held.claim.areas.iter().cloned().collect(),
    };
    StatusView {
        live: status.live.iter().map(line).collect(),
        stale: status.stale.iter().map(line).collect(),
        holds: status
            .holds
            .iter()
            .map(|hold| HoldLine {
                area: hold.area.clone(),
                holder: hold.holder.clone(),
                item: hold.item,
            })
            .collect(),
        ready: status
            .ready
            .ready
            .iter()
            .map(|entry| ReadyLine {
                item: entry.item.id,
                reference: entry.item.external_ref.clone(),
                title: entry.item.title.clone(),
                priority: entry.item.priority,
                areas_hint: entry.item.areas_hint.iter().cloned().collect(),
                stale_holder: entry.stale_holder.clone(),
            })
            .collect(),
        blocked: count(status, |reason| {
            matches!(reason, NotReady::BlockedBy { .. })
        }),
        area_held: count(status, |reason| matches!(reason, NotReady::AreaHeld { .. })),
    }
}

/// How many open items nobody holds live are left out of the ready list for a reason `of` picks.
fn count(status: &Status, of: impl Fn(&NotReady) -> bool) -> u64 {
    let unheld = |reasons: &[NotReady]| {
        !reasons
            .iter()
            .any(|reason| matches!(reason, NotReady::Claimed { .. }))
    };
    let counted = status
        .ready
        .excluded
        .iter()
        .filter(|entry| unheld(&entry.reasons) && entry.reasons.iter().any(&of))
        .count();
    u64::try_from(counted).unwrap_or(u64::MAX)
}

/// What the guard asks the inbox for, and keeps on this machine: the conversations with unread
/// messages and the messages awaiting the member's acknowledgement.
pub const GUARD_INBOX: InboxRequest = InboxRequest {
    unread: true,
    unacked: true,
};

/// The conversations with unread messages, and the messages awaiting an acknowledgement, as the
/// guard reads them.
pub fn inbox(inbox: &Inbox) -> InboxView {
    let unread: Vec<_> = inbox
        .threads
        .iter()
        .filter(|summary| summary.unread > 0)
        .collect();
    InboxView {
        unread: u64::try_from(unread.len()).unwrap_or(u64::MAX),
        subjects: unread
            .iter()
            .map(|summary| summary.thread.subject.clone())
            .collect(),
        unacked: inbox
            .unacked
            .iter()
            .map(|unacked| UnackedLine {
                message: unacked.message.id,
                sender: unacked.message.author.clone(),
                subject: unacked.thread.subject.clone(),
                importance: unacked.message.importance,
            })
            .collect(),
    }
}

/// A verdict as the guard reads it.
pub fn answer(verdict: &Verdict) -> PathAnswer {
    match verdict {
        Verdict::Allowed => PathAnswer::Allowed,
        Verdict::AllowedWithNote {
            rule,
            note,
            touched_by,
        } => PathAnswer::Noted {
            rule: rule.0.clone(),
            note: note.clone(),
            touched_by: touched_by
                .iter()
                .map(|other| TouchedBy {
                    holder: other.holder.clone(),
                    item: other.item,
                    item_ref: other.item_ref.clone(),
                })
                .collect(),
        },
        Verdict::NeedsClaim => PathAnswer::NeedsClaim,
        Verdict::HeldBy {
            holder,
            item,
            item_ref,
            area,
            ..
        } => PathAnswer::HeldBy {
            holder: holder.clone(),
            item: *item,
            item_ref: item_ref.clone(),
            area: area.clone(),
        },
        Verdict::Granted { by, area } => PathAnswer::Granted {
            by: by.clone(),
            area: area.clone(),
        },
    }
}

/// A client failure as the guard reads it: no decision from the server, or a refusal with its
/// sentence and the way on.
pub fn port_error(error: &ClientError) -> PortError {
    let text = sentence(&error.to_string());
    if error.is_unavailable() {
        return PortError::Unavailable(text);
    }
    if matches!(error, ClientError::Unauthorized { .. }) {
        return PortError::Refused(format!("{text} Sign in again with dibs login."));
    }
    PortError::Refused(text)
}

#[cfg(test)]
mod tests {
    use dibs_client::ErrorDetails;

    use super::*;

    #[test]
    fn a_server_that_gives_no_decision_is_unavailable_and_a_refusal_is_not() {
        let unavailable = port_error(&ClientError::Unexpected {
            server: "https://dibs.example.com".to_owned(),
            status: 403,
        });
        assert!(matches!(unavailable, PortError::Unavailable(_)));
        let refused = port_error(&ClientError::Refused {
            status: 422,
            code: "invalid".to_owned(),
            message: "that is not a path".to_owned(),
            details: Box::new(ErrorDetails::default()),
        });
        assert_eq!(
            refused,
            PortError::Refused("That is not a path.".to_owned())
        );
        let token = port_error(&ClientError::Unauthorized {
            message: "The token is not valid.".to_owned(),
        });
        assert!(
            token
                .to_string()
                .ends_with("Sign in again with dibs login.")
        );
    }
}
