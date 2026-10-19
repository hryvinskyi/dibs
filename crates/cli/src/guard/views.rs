//! The client's answers as the guard reads them.

use dibs_agent_host::ports::{ClaimLine, HoldLine, InboxView, PathAnswer, PortError, StatusView};
use dibs_client::ClientError;
use dibs_client::api::{HeldItem, Status, Threads, Verdict};

use crate::failure::sentence;

/// The status as the guard reads it.
pub fn status(status: &Status) -> StatusView {
    let line = |held: &HeldItem| ClaimLine {
        item: held.item.id,
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
    }
}

/// The conversations with unread messages as the guard reads them.
pub fn inbox(threads: &Threads) -> InboxView {
    let unread: Vec<_> = threads
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
                .map(|other| (other.holder.clone(), other.item))
                .collect(),
        },
        Verdict::NeedsClaim => PathAnswer::NeedsClaim,
        Verdict::HeldBy {
            holder, item, area, ..
        } => PathAnswer::HeldBy {
            holder: holder.clone(),
            item: *item,
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
