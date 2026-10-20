//! The client's answers as the guard reads them.

use dibs_agent_host::SessionKey;
use dibs_agent_host::ports::{
    ClaimLine, ClaimedDelivery as GuardClaim, HeldWay, HoldLine, InboxView, OfferLine, PathAnswer,
    Pending, PendingMessage, PortError, ReadyLine, SlotHoldLine, SlotHoldState, SlotLine,
    SlotOfferState, SlotState, SlotWaiterState, StatusView, TouchedBy, UnackedLine, WideAreaLine,
    WideLine,
};
use dibs_client::ClientError;
use dibs_client::api::{
    Deliverable, HeldItem, HeldReason, Inbox, InboxRequest, NotReady, PendingOffer, Slot, Status,
    Verdict, WideAreaState,
};
use dibs_client::state::{ClaimedDelivery, RememberedMessage};

use crate::failure::sentence;

/// The status as the guard reads it.
pub fn status(status: &Status) -> StatusView {
    let line = |held: &HeldItem| ClaimLine {
        item: held.item.id,
        reference: held.item.external_ref.clone(),
        title: held.item.title.clone(),
        holder: held.claim.holder.clone(),
        areas: held.claim.areas.iter().cloned().collect(),
        prs: held
            .claim
            .review
            .as_ref()
            .map(|review| review.prs.iter().cloned().collect())
            .unwrap_or_default(),
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
        wide: status
            .wide
            .iter()
            .map(|wide| WideLine {
                item: wide.item.id,
                reference: wide.item.external_ref.clone(),
                title: wide.item.title.clone(),
                holder: wide.claim.holder.clone(),
                reason: wide
                    .claim
                    .wide
                    .as_ref()
                    .map(|scope| scope.reason.clone())
                    .unwrap_or_default(),
                areas: wide
                    .areas
                    .iter()
                    .map(|area| WideAreaLine {
                        area: area.area.clone(),
                        holder: area.holder.clone(),
                        granted: area.state == WideAreaState::Granted,
                    })
                    .collect(),
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
        slots: status
            .slots
            .iter()
            .map(|slot| SlotLine {
                slot: slot.name.clone(),
                hold: slot
                    .hold
                    .as_ref()
                    .filter(|hold| hold.slot == slot.name)
                    .map(|hold| SlotHoldLine {
                        holder: hold.holder.clone(),
                        purpose: hold.purpose.clone(),
                        expires_at: hold.expires_at,
                        last_seen: hold.last_seen,
                        stalled: hold.stalled,
                    }),
                waiting: slot
                    .queue
                    .iter()
                    .map(|waiter| (waiter.member.clone(), waiter.slot.clone()))
                    .collect(),
            })
            .collect(),
        integration_silent_since: status.integration_silent_since,
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
/// messages, the messages awaiting the member's acknowledgement, and the messages still to be
/// delivered to one of the member's sessions.
pub const GUARD_INBOX: InboxRequest = InboxRequest {
    unread: true,
    unacked: true,
    deliverable: true,
};

/// The conversations with unread messages, the messages awaiting an acknowledgement or still to be
/// delivered, and the deliveries this machine claimed, as the guard reads them. A claim this dibs
/// cannot read back is left out.
pub fn inbox(inbox: &Inbox, claims: &[ClaimedDelivery]) -> InboxView {
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
        offers: inbox
            .offers
            .iter()
            .map(|offer| OfferLine {
                slot: offer.slot.clone(),
                until: offer.until,
            })
            .collect(),
        deliverable: inbox.deliverable.iter().map(pending_message).collect(),
        claims: claims.iter().filter_map(claimed).collect(),
    }
}

/// A message still to be delivered, as the guard reads it.
pub fn pending_message(entry: &Deliverable) -> PendingMessage {
    remembered_message(&RememberedMessage {
        id: entry.message.id,
        thread: entry.thread.id,
        author: entry.message.author.clone(),
        system: entry.message.system,
        subject: entry.thread.subject.clone(),
        importance: entry.message.importance,
        awaiting_ack: entry.awaiting_ack,
        body: entry.message.body.as_str().to_owned(),
    })
}

/// A remembered message, as the guard reads it.
fn remembered_message(message: &RememberedMessage) -> PendingMessage {
    PendingMessage {
        message: message.id,
        thread: message.thread,
        sender: message.author.clone(),
        from_dibs: message.system,
        subject: message.subject.clone(),
        importance: message.importance,
        awaiting_ack: message.awaiting_ack,
        body: message.body.clone(),
    }
}

/// A claim this machine remembers, as the guard reads it.
fn claimed(claim: &ClaimedDelivery) -> Option<GuardClaim> {
    let pending = match (&claim.message, &claim.offer) {
        (Some(message), _) => Pending::Message(remembered_message(message)),
        (None, Some(offer)) => Pending::Offer(OfferLine {
            slot: offer.slot.clone(),
            until: offer.until,
        }),
        (None, None) => return None,
    };
    Some(GuardClaim {
        pending,
        host: claim.host.clone(),
        session: SessionKey::parse(&claim.session)?,
        won: claim.won,
        at: claim.at,
    })
}

/// A claim made for the guard, as this machine remembers it.
pub fn remembered(claim: &GuardClaim) -> ClaimedDelivery {
    let (message, offer) = match &claim.pending {
        Pending::Message(message) => (Some(deliverable(message)), None),
        Pending::Offer(offer) => (
            None,
            Some(PendingOffer {
                slot: offer.slot.clone(),
                until: offer.until,
            }),
        ),
    };
    ClaimedDelivery {
        subject: claim.pending.subject().key(),
        host: claim.host.clone(),
        session: claim.session.as_str().to_owned(),
        won: claim.won,
        at: claim.at,
        message,
        offer,
    }
}

/// What a remembered claim keeps of a message: enough to show it once the server no longer lists
/// it.
fn deliverable(message: &PendingMessage) -> RememberedMessage {
    RememberedMessage {
        id: message.message,
        thread: message.thread,
        author: message.sender.clone(),
        system: message.from_dibs,
        subject: message.subject.clone(),
        importance: message.importance,
        awaiting_ack: message.awaiting_ack,
        body: message.body.clone(),
    }
}

/// Where each slot asked about stands, as the guard reads it.
pub fn slot_state(slot: &Slot) -> SlotState {
    SlotState {
        slot: slot.name.clone(),
        hold: slot.hold.as_ref().map(|hold| SlotHoldState {
            slot: hold.slot.clone(),
            holder: hold.holder.clone(),
            purpose: hold.purpose.clone(),
            session: hold.session_id,
            host: hold.host.clone(),
            item: hold.item,
            liveness: hold.liveness.clone(),
            taken_at: hold.taken_at,
            expires_at: hold.expires_at,
            last_seen: hold.last_seen,
            stalled: hold.stalled,
        }),
        queue: slot
            .queue
            .iter()
            .map(|waiter| SlotWaiterState {
                member: waiter.member.clone(),
                slot: waiter.slot.clone(),
                purpose: waiter.purpose.clone(),
                since: waiter.since,
                last_seen: waiter.last_seen,
            })
            .collect(),
        offer: slot.offer.as_ref().map(|offer| SlotOfferState {
            member: offer.member.clone(),
            slot: offer.slot.clone(),
            purpose: offer.purpose.clone(),
            until: offer.until,
        }),
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
            reason,
            prs,
            claim,
            waiting,
            escalates_at,
        } => PathAnswer::HeldBy {
            holder: holder.clone(),
            item: *item,
            item_ref: item_ref.clone(),
            area: area.clone(),
            way: match reason {
                HeldReason::InReview => HeldWay::InReview { prs: prs.clone() },
                HeldReason::AwaitingAck => HeldWay::AwaitingAck,
                HeldReason::AwaitingHandover => HeldWay::AwaitingHandover {
                    escalates_at: *escalates_at,
                },
                HeldReason::AskHandover => {
                    claim.map_or(HeldWay::AskHolder, |claim| HeldWay::AskHandover { claim })
                }
                HeldReason::AskHolder => match waiting {
                    Some(waiting) => HeldWay::WaitedFor {
                        waiter: waiting.holder.clone(),
                        claim: waiting.item,
                        claim_ref: waiting.item_ref.clone(),
                    },
                    None => HeldWay::AskHolder,
                },
                HeldReason::Other => HeldWay::AskHolder,
            },
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
    fn a_debt_holders_verdict_carries_the_way_to_the_handover() {
        let held = |reason, claim| {
            answer(&Verdict::HeldBy {
                holder: None,
                item: dibs_domain::ids::ItemId(7),
                item_ref: None,
                area: dibs_domain::area::QualifiedArea::parse("api:billing").unwrap(),
                reason,
                prs: Vec::new(),
                claim,
                waiting: None,
                escalates_at: None,
            })
        };
        let way = |answer: PathAnswer| match answer {
            PathAnswer::HeldBy { way, .. } => way,
            other => panic!("not held: {other:?}"),
        };
        assert_eq!(
            way(held(HeldReason::AwaitingHandover, None)),
            HeldWay::AwaitingHandover { escalates_at: None }
        );
        assert_eq!(
            way(held(
                HeldReason::AskHandover,
                Some(dibs_domain::ids::ItemId(42))
            )),
            HeldWay::AskHandover {
                claim: dibs_domain::ids::ItemId(42)
            }
        );
        assert_eq!(
            way(held(HeldReason::AskHandover, None)),
            HeldWay::AskHolder,
            "without the claim to declare it on, the holder is asked"
        );
    }

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
