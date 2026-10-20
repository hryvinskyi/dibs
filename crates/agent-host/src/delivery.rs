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
//!
//! A session that waits for its person can be pushed to, on a host that can wake one:
//! - only while it waits — a session at work hears from its hooks — and, when its hooks left no
//!   stamp that says which, only after the hooks had a head start;
//! - only a high or urgent message whose acknowledgement grants nothing, or a slot offer: an
//!   acknowledgement is the person's to give, and a normal message waits for the hooks;
//! - at most a few wakes per sender within a window, beyond which the hooks deliver;
//! - what was pushed is shown by the session's hooks once, later, unless something shows it was
//!   seen: the agent read the conversation, acknowledged the message or looked at the slot, or the
//!   person submitted a prompt.

use std::collections::BTreeMap;

use time::{Duration, OffsetDateTime};

use dibs_domain::event::EventKind;
use dibs_domain::handle::Handle;
use dibs_domain::ids::MemberId;
use dibs_domain::message::interrupts;

use crate::capabilities::Capabilities;
use crate::ports::{
    ClaimedDelivery, InboxView, Pending, PushedDelivery, Seen, SessionActivity, SessionLedger,
    SessionRef, Wake,
};

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

/// How long a push may go without a sign it was seen before the session's hooks show it.
pub const PUSH_SEEN_WITHIN: Duration = Duration::minutes(5);

/// How long a stamp of work counts: a turn the person interrupts ends without a stamp of waiting.
pub const WORK_STAMP_FOR: Duration = Duration::minutes(10);

/// How long the hooks get to deliver something before a push may claim it, when no stamp says
/// whether the session works or waits.
pub const HOOKS_FIRST: Duration = Duration::minutes(2);

/// The most wakes one sender's messages make in one session within [`WAKE_WINDOW`].
pub const WAKES_PER_SENDER: usize = 3;

/// The window [`WAKES_PER_SENDER`] counts in.
pub const WAKE_WINDOW: Duration = Duration::minutes(15);

/// Whether a session's agent works or waits for its person, as its stamps say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// It waits: its last stamp is the end of a turn.
    Waiting,
    /// It works: a prompt or a tool came after the last end of a turn, within
    /// [`WORK_STAMP_FOR`].
    Working,
    /// No stamp says which.
    Unknown,
}

/// What the stamps of a session say at `now`.
pub fn session_state(activity: &SessionActivity, now: OffsetDateTime) -> SessionState {
    match (activity.working, activity.waiting) {
        (working, Some(waiting)) if working.is_none_or(|working| waiting >= working) => {
            SessionState::Waiting
        }
        (Some(working), _) if now - working <= WORK_STAMP_FOR => SessionState::Working,
        _ => SessionState::Unknown,
    }
}

/// Whether a delivery may be pushed at all: a slot offer, or a high or urgent message whose
/// acknowledgement grants nothing.
pub fn pushable(pending: &Pending) -> bool {
    match pending {
        Pending::Message(message) => message.importance.is_raised() && !message.ack_grants,
        Pending::Offer(_) => true,
    }
}

/// Who a push of `pending` wakes the session for: the sender of a message; nobody for an offer,
/// which no member wrote.
fn sender(pending: &Pending) -> Option<&Handle> {
    match pending {
        Pending::Message(message) => Some(&message.sender),
        Pending::Offer(_) => None,
    }
}

/// What may be pushed now, of `candidates`, after the session's ledger: nothing within a minute of
/// the last showing; otherwise at most [`PER_DELIVERY`] pushable items `me` did not write, that
/// were neither shown nor pushed, and whose sender has woken the session fewer than
/// [`WAKES_PER_SENDER`] times within [`WAKE_WINDOW`].
pub fn to_push(
    ledger: &SessionLedger,
    candidates: &[Pending],
    me: &Handle,
    now: OffsetDateTime,
) -> Vec<Pending> {
    if next_showing(ledger, now).is_some() {
        return Vec::new();
    }
    let pushed_before = |pending: &Pending| {
        let subject = pending.subject();
        ledger
            .pushed
            .iter()
            .any(|pushed| pushed.pending.subject() == subject)
    };
    let mut wakes: Vec<&Handle> = ledger
        .wakes
        .iter()
        .filter(|wake| wake.at <= now && now - wake.at < WAKE_WINDOW)
        .map(|wake| &wake.sender)
        .collect();
    let mut chosen = Vec::new();
    for pending in candidates {
        if chosen.len() == PER_DELIVERY {
            break;
        }
        if !pushable(pending)
            || sender(pending) == Some(me)
            || ledger.shown.contains(&pending.subject().key())
            || pushed_before(pending)
        {
            continue;
        }
        if let Some(sender) = sender(pending) {
            if wakes.iter().filter(|woke| **woke == sender).count() >= WAKES_PER_SENDER {
                continue;
            }
            wakes.push(sender);
        }
        chosen.push(pending.clone());
    }
    chosen
}

/// The ledger once `written` was pushed to the session at `now` and `unwritten`, claimed for it,
/// could not be: both wait to be seen, the first for [`PUSH_SEEN_WITHIN`], the second not at all;
/// each sender of what was written woke the session once more.
pub fn after_pushing(
    ledger: &SessionLedger,
    written: &[Pending],
    unwritten: &[Pending],
    now: OffsetDateTime,
) -> SessionLedger {
    let mut after = ledger.clone();
    if !written.is_empty() {
        after.last_injection = Some(now);
    }
    after
        .wakes
        .retain(|wake| wake.at <= now && now - wake.at < WAKE_WINDOW);
    after.wakes.extend(written.iter().filter_map(|pending| {
        sender(pending).map(|sender| Wake {
            sender: sender.clone(),
            at: now,
        })
    }));
    after
        .pushed
        .extend(written.iter().map(|pending| PushedDelivery {
            pending: pending.clone(),
            written: true,
            due: now + PUSH_SEEN_WITHIN,
        }));
    after
        .pushed
        .extend(unwritten.iter().map(|pending| PushedDelivery {
            pending: pending.clone(),
            written: false,
            due: now,
        }));
    let excess = after.pushed.len().saturating_sub(SHOWN_KEPT);
    after.pushed.drain(..excess);
    after
}

/// The ledger once `seen` showed what it concerns reached the agent or its person: the pushes it
/// concerns are no longer waiting. A prompt clears every push that was written, not one that never
/// reached the host.
pub fn after_seen(ledger: &SessionLedger, seen: &Seen) -> SessionLedger {
    let mut after = ledger.clone();
    after.pushed.retain(|pushed| !concerns(seen, pushed));
    after
}

fn concerns(seen: &Seen, pushed: &PushedDelivery) -> bool {
    match (seen, &pushed.pending) {
        (Seen::Person, _) => pushed.written,
        (Seen::Thread(thread), Pending::Message(message)) => message.thread == *thread,
        (Seen::Message(id), Pending::Message(message)) => message.message == *id,
        (Seen::Slot(slot), Pending::Offer(offer)) => offer.slot == *slot,
        (Seen::Thread(_) | Seen::Message(_), Pending::Offer(_))
        | (Seen::Slot(_), Pending::Message(_)) => false,
    }
}

/// What was pushed and is due for the session's hooks to show at `now`.
pub fn due_pushes(ledger: &SessionLedger, now: OffsetDateTime) -> Vec<Pending> {
    ledger
        .pushed
        .iter()
        .filter(|pushed| pushed.due <= now)
        .map(|pushed| pushed.pending.clone())
        .collect()
}

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
    if next_showing(ledger, now).is_some() {
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
    let mut after = after_showing(ledger, &show, now);
    let shown: Vec<_> = show.iter().map(Pending::subject).collect();
    after
        .pushed
        .retain(|pushed| !shown.contains(&pushed.pending.subject()));
    Some((show, after))
}

/// The ledger once `shown` was put in front of the agent at `now`: the time, and the subjects
/// shown, the oldest dropped past [`SHOWN_KEPT`].
pub fn after_showing(
    ledger: &SessionLedger,
    shown: &[Pending],
    now: OffsetDateTime,
) -> SessionLedger {
    let mut subjects = ledger.shown.clone();
    subjects.extend(shown.iter().map(|pending| pending.subject().key()));
    let excess = subjects.len().saturating_sub(SHOWN_KEPT);
    subjects.drain(..excess);
    SessionLedger {
        last_injection: Some(now),
        shown: subjects,
        ..ledger.clone()
    }
}

/// When the session may be shown something again after its ledger, at the earliest; `None` when
/// it may now.
pub fn next_showing(ledger: &SessionLedger, now: OffsetDateTime) -> Option<OffsetDateTime> {
    ledger
        .last_injection
        .filter(|at| *at <= now && now - *at < DELIVER_EVERY)
        .map(|at| at + DELIVER_EVERY)
}

/// Whether an event of the project's log may bring `me` something to deliver: a message somebody
/// else or dibs posted, or a slot offered to them. `members` names the members the event names.
/// What it brings is read from the inbox, which says what reaches `me`; the event only says when
/// to look.
pub fn may_bring(kind: &EventKind, members: &BTreeMap<MemberId, Handle>, me: &Handle) -> bool {
    match kind {
        EventKind::MessagePosted(posted) => posted
            .author
            .is_none_or(|author| members.get(&author) != Some(me)),
        EventKind::SlotOffered(offered) => members.get(&offered.member) == Some(me),
        _ => false,
    }
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
            item: None,
            ack_grants: false,
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
            ..SessionLedger::default()
        };
        let (shown, ledger) = choose(&full, &won, &bob, NOW).unwrap();
        assert_eq!(shown.len(), PER_DELIVERY);
        assert_eq!(ledger.shown.len(), SHOWN_KEPT);
        assert_eq!(ledger.shown.last().map(String::as_str), Some("3"));
    }

    #[test]
    fn the_next_showing_is_a_minute_after_the_last() {
        let ledger = SessionLedger {
            last_injection: Some(NOW),
            shown: Vec::new(),
            ..SessionLedger::default()
        };
        assert_eq!(
            next_showing(&ledger, NOW + Duration::seconds(10)),
            Some(NOW + DELIVER_EVERY)
        );
        assert_eq!(next_showing(&ledger, NOW + DELIVER_EVERY), None);
        assert_eq!(next_showing(&SessionLedger::default(), NOW), None);
        assert_eq!(
            next_showing(&ledger, NOW - Duration::seconds(5)),
            None,
            "a showing recorded in the future does not hold anything back"
        );
        let after = after_showing(&ledger, &[Pending::Offer(offer(5))], NOW + DELIVER_EVERY);
        assert_eq!(after.last_injection, Some(NOW + DELIVER_EVERY));
        assert_eq!(after.shown, [Pending::Offer(offer(5)).subject().key()]);
    }

    #[test]
    fn only_a_message_by_someone_else_or_a_slot_offered_to_me_may_bring_a_delivery() {
        use dibs_domain::event::{MessagePosted, SlotMember};

        let bob = handle("bob");
        let members = BTreeMap::from([(MemberId(1), handle("alice")), (MemberId(2), bob.clone())]);
        let posted = |author: Option<u64>| {
            EventKind::MessagePosted(MessagePosted {
                thread: ThreadId(1),
                message: MessageId(1),
                author: author.map(MemberId),
            })
        };
        let offered = |member: u64| {
            EventKind::SlotOffered(SlotMember {
                slot: SlotName::parse("deploy:prod").unwrap(),
                member: MemberId(member),
            })
        };
        assert!(may_bring(&posted(Some(1)), &members, &bob));
        assert!(may_bring(&posted(None), &members, &bob), "dibs wrote it");
        assert!(!may_bring(&posted(Some(2)), &members, &bob), "my own");
        assert!(may_bring(&offered(2), &members, &bob));
        assert!(!may_bring(&offered(1), &members, &bob));
    }

    #[test]
    fn a_session_waits_when_its_last_stamp_ends_a_turn_and_works_for_ten_minutes_after_a_tool() {
        let stamps = |working: Option<i64>, waiting: Option<i64>| SessionActivity {
            working: working.map(|minutes| NOW + Duration::minutes(minutes)),
            waiting: waiting.map(|minutes| NOW + Duration::minutes(minutes)),
        };
        let at = |minutes| NOW + Duration::minutes(minutes);
        assert_eq!(
            session_state(&stamps(Some(1), Some(2)), at(3)),
            SessionState::Waiting
        );
        assert_eq!(
            session_state(&stamps(None, Some(2)), at(3)),
            SessionState::Waiting
        );
        assert_eq!(
            session_state(&stamps(Some(2), Some(1)), at(3)),
            SessionState::Working
        );
        assert_eq!(
            session_state(&stamps(Some(2), Some(1)), at(13)),
            SessionState::Unknown,
            "a turn the person interrupted leaves no stamp of waiting"
        );
        assert_eq!(
            session_state(&stamps(None, None), at(0)),
            SessionState::Unknown
        );
    }

    #[test]
    fn only_raised_messages_that_grant_nothing_and_offers_are_pushed() {
        let mut grants = message(1, "alice", Importance::Urgent, true);
        grants.ack_grants = true;
        assert!(
            !pushable(&Pending::Message(grants)),
            "acknowledging it would grant"
        );
        assert!(!pushable(&Pending::Message(message(
            2,
            "alice",
            Importance::Normal,
            true
        ))));
        assert!(pushable(&Pending::Message(message(
            3,
            "alice",
            Importance::High,
            false
        ))));
        assert!(pushable(&Pending::Offer(offer(5))));
    }

    #[test]
    fn a_sender_wakes_a_session_three_times_in_fifteen_minutes_and_the_hooks_deliver_the_rest() {
        let bob = handle("bob");
        let from = |id: u64, sender: &str| {
            Pending::Message(message(id, sender, Importance::Urgent, false))
        };
        let mut ledger = SessionLedger::default();
        let mut pushed = Vec::new();
        for minute in 0..5 {
            let now = NOW + Duration::minutes(minute * 2);
            let picks = to_push(&ledger, &[from(minute as u64 + 1, "alice")], &bob, now);
            if !picks.is_empty() {
                ledger = after_pushing(&ledger, &picks, &[], now);
                pushed.extend(picks);
            }
        }
        assert_eq!(pushed.len(), 3, "{pushed:?}");
        let later = NOW + Duration::minutes(10);
        assert_eq!(
            to_push(&ledger, &[from(9, "carol")], &bob, later).len(),
            1,
            "another sender still wakes it"
        );
        assert_eq!(
            to_push(
                &ledger,
                &[from(10, "alice")],
                &bob,
                NOW + Duration::minutes(16)
            )
            .len(),
            1,
            "the window moved on"
        );
        assert!(
            to_push(&ledger, &[from(11, "bob")], &bob, later).is_empty(),
            "my own"
        );
        assert!(
            to_push(&ledger, &[pushed[0].clone()], &bob, later).is_empty(),
            "never twice"
        );
        assert!(
            to_push(
                &ledger,
                &[from(12, "dave")],
                &bob,
                NOW + Duration::minutes(4) + Duration::seconds(30)
            )
            .is_empty(),
            "within a minute of the last push"
        );
    }

    #[test]
    fn a_push_waits_five_minutes_to_be_seen_and_one_never_written_is_due_at_once() {
        let written = Pending::Message(message(1, "alice", Importance::Urgent, false));
        let unwritten = Pending::Offer(offer(9));
        let ledger = after_pushing(
            &SessionLedger::default(),
            std::slice::from_ref(&written),
            std::slice::from_ref(&unwritten),
            NOW,
        );
        assert_eq!(ledger.last_injection, Some(NOW));
        assert_eq!(ledger.wakes.len(), 1, "an offer wakes nobody's budget");
        assert_eq!(due_pushes(&ledger, NOW), std::slice::from_ref(&unwritten));
        assert_eq!(
            due_pushes(&ledger, NOW + PUSH_SEEN_WITHIN),
            [written.clone(), unwritten.clone()]
        );
        let read = after_seen(&ledger, &Seen::Thread(ThreadId(101)));
        assert_eq!(
            due_pushes(&read, NOW + PUSH_SEEN_WITHIN),
            std::slice::from_ref(&unwritten)
        );
        let acked = after_seen(&ledger, &Seen::Message(MessageId(1)));
        assert_eq!(acked.pushed.len(), 1);
        let taken = after_seen(
            &ledger,
            &Seen::Slot(SlotName::parse("deploy:prod").unwrap()),
        );
        assert_eq!(due_pushes(&taken, NOW), Vec::<Pending>::new());
        let person = after_seen(&ledger, &Seen::Person);
        assert_eq!(
            due_pushes(&person, NOW + PUSH_SEEN_WITHIN),
            [unwritten],
            "a push that never reached the host is not seen by anyone"
        );
    }
}
