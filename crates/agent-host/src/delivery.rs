//! Delivery between tool calls: what the background sync claims from the server for a session, what
//! that session may then put in front of its agent, and how often.
//!
//! The rules, whatever the host:
//! - only a host that tells the main agent from a subagent, and carries text to the agent after a
//!   tool or with a prompt, is claimed for: what reaches a subagent ends with it;
//! - a member's own messages are never delivered to them;
//! - the server hands each pressing message and each offer to one session of the member, the first
//!   that claims it; a claim is made in the background and shown from the next event on;
//! - a session shows something at most once a minute, and never the same thing twice.

use time::{Duration, OffsetDateTime};

use dibs_domain::handle::Handle;
use dibs_domain::message::interrupts;

use crate::capabilities::Capabilities;
use crate::ports::{ClaimedDelivery, InboxView, Pending, SessionLedger, SessionRef};

/// The least time between two deliveries to one session.
pub const DELIVER_EVERY: Duration = Duration::minutes(1);

/// The most items one delivery puts in front of the agent.
pub const PER_DELIVERY: usize = 3;

/// The most deliveries one background sync claims.
pub const CLAIMS_PER_SYNC: usize = 5;

/// How long a claim is remembered on this machine.
pub const CLAIMS_KEPT_FOR: Duration = Duration::hours(24);

/// How many shown subjects a session's ledger keeps.
pub const SHOWN_KEPT: usize = 200;

/// Whether a host's sessions are claimed for: the host says which thread an event comes from and
/// carries text to the agent between its tool calls or with a prompt.
pub fn can_deliver(capabilities: Capabilities) -> bool {
    capabilities.tells_subagent
        && (capabilities.context_after_tool || capabilities.context_at_prompt)
}

/// What a background sync claims, at most [`CLAIMS_PER_SYNC`]: the slots the server offers `me`
/// and the pressing messages others wrote to them, of which this machine claimed none yet.
pub fn to_claim(inbox: &InboxView, me: &Handle) -> Vec<Pending> {
    let claimed = |pending: &Pending| {
        let subject = pending.subject();
        inbox
            .claims
            .iter()
            .any(|claim| claim.pending.subject() == subject)
    };
    let offers = inbox.offers.iter().cloned().map(Pending::Offer);
    let messages = inbox
        .deliverable
        .iter()
        .filter(|message| {
            message.sender != *me && interrupts(message.importance, message.awaiting_ack)
        })
        .cloned()
        .map(Pending::Message);
    offers
        .chain(messages)
        .filter(|pending| !claimed(pending))
        .take(CLAIMS_PER_SYNC)
        .collect()
}

/// The claims worth keeping at `now`: those made within [`CLAIMS_KEPT_FOR`].
pub fn kept_claims(claims: Vec<ClaimedDelivery>, now: OffsetDateTime) -> Vec<ClaimedDelivery> {
    claims
        .into_iter()
        .filter(|claim| claim.at <= now && now - claim.at < CLAIMS_KEPT_FOR)
        .collect()
}

/// What `session` won and may still show: an offer only while the inbox, as the server last
/// answered it, still lists it, so the server's clock, not this machine's, says when it lapsed.
pub fn won_by(inbox: &InboxView, session: &SessionRef) -> Vec<Pending> {
    inbox
        .claims
        .iter()
        .filter(|claim| {
            claim.won && claim.host == session.host.as_str() && claim.session == session.key
        })
        .filter(|claim| match &claim.pending {
            Pending::Offer(offer) => inbox.offers.contains(offer),
            Pending::Message(_) => true,
        })
        .map(|claim| claim.pending.clone())
        .collect()
}

/// What to show now, and the ledger after showing it: at most [`PER_DELIVERY`] of `won` that the
/// session has not shown and `me` did not write, unless the session showed something within
/// [`DELIVER_EVERY`]. `None` when there is nothing to show now.
pub fn choose(
    ledger: &SessionLedger,
    won: &[Pending],
    me: &Handle,
    now: OffsetDateTime,
) -> Option<(Vec<Pending>, SessionLedger)> {
    if ledger
        .last_injection
        .is_some_and(|at| at <= now && now - at < DELIVER_EVERY)
    {
        return None;
    }
    let show: Vec<Pending> = won
        .iter()
        .filter(|pending| !ledger.shown.contains(&pending.subject().key()))
        .filter(|pending| match pending {
            Pending::Message(message) => message.sender != *me,
            Pending::Offer(_) => true,
        })
        .take(PER_DELIVERY)
        .cloned()
        .collect();
    if show.is_empty() {
        return None;
    }
    let mut shown = ledger.shown.clone();
    shown.extend(show.iter().map(|pending| pending.subject().key()));
    let excess = shown.len().saturating_sub(SHOWN_KEPT);
    shown.drain(..excess);
    Some((
        show,
        SessionLedger {
            last_injection: Some(now),
            shown,
        },
    ))
}

#[cfg(test)]
mod tests {
    use dibs_domain::ids::{MessageId, ThreadId};
    use dibs_domain::message::Importance;
    use dibs_domain::slot::SlotName;

    use super::*;
    use crate::adapter::HostId;
    use crate::event::SessionKey;
    use crate::ports::{OfferLine, PendingMessage};

    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn message(
        id: u64,
        sender: &str,
        importance: Importance,
        awaiting_ack: bool,
    ) -> PendingMessage {
        PendingMessage {
            message: MessageId(id),
            thread: ThreadId(id + 100),
            sender: handle(sender),
            from_dibs: false,
            subject: "Staging".parse().unwrap(),
            importance,
            awaiting_ack,
            body: "Hold every deploy.".to_owned(),
        }
    }

    fn offer(minutes: i64) -> OfferLine {
        OfferLine {
            slot: SlotName::parse("deploy:prod").unwrap(),
            until: NOW + Duration::minutes(minutes),
        }
    }

    fn session(key: &'static str) -> SessionRef {
        SessionRef {
            host: HostId::new("neutral"),
            key: SessionKey::fixture(key),
        }
    }

    fn claim(pending: Pending, key: &'static str, won: bool, minutes_ago: i64) -> ClaimedDelivery {
        ClaimedDelivery {
            pending,
            host: "neutral".to_owned(),
            session: session(key).key,
            won,
            at: NOW - Duration::minutes(minutes_ago),
        }
    }

    #[test]
    fn only_hosts_that_tell_a_subagent_apart_and_carry_text_between_calls_are_claimed_for() {
        let full = Capabilities {
            tells_subagent: true,
            context_after_tool: true,
            context_at_prompt: true,
            ..Capabilities::default()
        };
        assert!(can_deliver(full));
        assert!(!can_deliver(Capabilities {
            tells_subagent: false,
            ..full
        }));
        assert!(!can_deliver(Capabilities {
            context_after_tool: false,
            context_at_prompt: false,
            ..full
        }));
    }

    #[test]
    fn a_sync_claims_offers_and_pressing_messages_of_others_not_claimed_yet() {
        let bob = handle("bob");
        let urgent = message(1, "alice", Importance::Urgent, false);
        let mine = message(2, "bob", Importance::Urgent, false);
        let asking = message(3, "alice", Importance::Normal, true);
        let ordinary = message(4, "alice", Importance::Normal, false);
        let inbox = InboxView {
            deliverable: vec![urgent.clone(), mine, asking.clone(), ordinary],
            offers: vec![offer(5)],
            claims: vec![claim(Pending::Message(asking), "s-2", false, 1)],
            ..InboxView::default()
        };
        assert_eq!(
            to_claim(&inbox, &bob),
            [Pending::Offer(offer(5)), Pending::Message(urgent)]
        );
    }

    #[test]
    fn a_session_shows_what_it_won_and_an_offer_while_the_server_lists_it() {
        let won = Pending::Message(message(1, "alice", Importance::High, false));
        let lost = Pending::Message(message(2, "alice", Importance::High, false));
        let theirs = Pending::Message(message(3, "alice", Importance::High, false));
        let inbox = InboxView {
            claims: vec![
                claim(won.clone(), "s-1", true, 1),
                claim(lost, "s-1", false, 1),
                claim(theirs, "s-2", true, 1),
                claim(Pending::Offer(offer(5)), "s-1", true, 1),
                claim(Pending::Offer(offer(-1)), "s-1", true, 9),
            ],
            offers: vec![offer(5)],
            ..InboxView::default()
        };
        assert_eq!(
            won_by(&inbox, &session("s-1")),
            [won, Pending::Offer(offer(5))]
        );
        let kept = kept_claims(
            inbox.claims.clone(),
            NOW + Duration::hours(24) - Duration::minutes(5),
        );
        assert_eq!(kept.len(), 4, "the oldest one ages out");
    }

    #[test]
    fn a_session_shows_each_thing_once_and_at_most_once_a_minute() {
        let bob = handle("bob");
        let first = Pending::Message(message(1, "alice", Importance::Urgent, false));
        let second = Pending::Message(message(2, "alice", Importance::Urgent, false));
        let (shown, ledger) = choose(
            &SessionLedger::default(),
            std::slice::from_ref(&first),
            &bob,
            NOW,
        )
        .unwrap();
        assert_eq!(shown, std::slice::from_ref(&first));
        assert_eq!(ledger.last_injection, Some(NOW));
        let won = [first.clone(), second.clone()];
        assert_eq!(
            choose(&ledger, &won, &bob, NOW + Duration::seconds(59)),
            None,
            "within the minute"
        );
        let (later, after) = choose(&ledger, &won, &bob, NOW + Duration::seconds(60)).unwrap();
        assert_eq!(later, [second], "never the same message twice");
        assert_eq!(choose(&after, &won, &bob, NOW + Duration::minutes(5)), None);
    }

    #[test]
    fn a_members_own_message_is_never_shown_to_them() {
        let bob = handle("bob");
        let own = Pending::Message(message(1, "bob", Importance::Urgent, true));
        assert_eq!(choose(&SessionLedger::default(), &[own], &bob, NOW), None);
    }

    #[test]
    fn one_delivery_carries_a_few_items_and_the_ledger_stays_bounded() {
        let bob = handle("bob");
        let won: Vec<Pending> = (1..=5)
            .map(|id| Pending::Message(message(id, "alice", Importance::High, false)))
            .collect();
        let full = SessionLedger {
            last_injection: None,
            shown: (0..SHOWN_KEPT).map(|n| format!("old-{n}")).collect(),
        };
        let (shown, ledger) = choose(&full, &won, &bob, NOW).unwrap();
        assert_eq!(shown.len(), PER_DELIVERY);
        assert_eq!(ledger.shown.len(), SHOWN_KEPT);
        assert_eq!(ledger.shown.last().map(String::as_str), Some("3"));
    }
}
