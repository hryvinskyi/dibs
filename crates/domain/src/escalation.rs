//! Escalation: when agents do not agree, the question goes to the people the project names to settle
//! it — its arbiters — together with whoever holds what is disputed and whoever asked for it.
//!
//! An escalation is about one subject: a work item, a slot or a conversation. Who it reaches is
//! decided here from the parties of that subject, and whether it still waits is decided from the
//! acknowledgements its message received: it stands until one of the arbiters it reached
//! acknowledges it.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{ItemId, MemberId, ThreadId};
use crate::message::Ack;
use crate::slot::{SlotName, SlotStanding};
use crate::text::string_value;

/// What an escalation is about. Serialised as `{"item": 12}`, `{"slot": "deploy:prod"}` or
/// `{"thread": 5}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationSubject {
    /// A work item: who holds it, and who wants it.
    Item(ItemId),
    /// A slot: who holds it, and who waits for it.
    Slot(SlotName),
    /// A conversation: a message in it that asked for an acknowledgement and did not get it.
    Thread(ThreadId),
}

impl EscalationSubject {
    /// The work item it is about, for an escalation of one.
    pub const fn item(&self) -> Option<ItemId> {
        match self {
            Self::Item(item) => Some(*item),
            Self::Slot(_) | Self::Thread(_) => None,
        }
    }
}

impl fmt::Display for EscalationSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Item(item) => write!(f, "work item #{item}"),
            Self::Slot(slot) => write!(f, "the slot {slot}"),
            Self::Thread(thread) => write!(f, "conversation #{thread}"),
        }
    }
}

/// Why something is escalated, as the member wrote it: 1 to 2000 characters after trimming, with no
/// control characters but line breaks and tabs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EscalationReason(String);

/// Why a text is not a reason for an escalation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EscalationReasonError {
    /// The trimmed text is empty, longer than 2000 characters, or has control characters.
    #[error("a reason is 1 to 2000 characters")]
    Malformed,
}

impl EscalationReason {
    /// The most characters a reason holds.
    pub const MAX_CHARS: usize = 2000;

    /// Parses a reason, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`EscalationReasonError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, EscalationReasonError> {
        let value = value.trim();
        let stray = value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t');
        if value.is_empty() || value.chars().count() > Self::MAX_CHARS || stray {
            return Err(EscalationReasonError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(EscalationReason, EscalationReasonError);

/// The members a subject concerns, besides the arbiters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parties {
    /// Who holds what is disputed: the item's holder, the slot's holder, or, for a conversation,
    /// the holder of its item and the recipients who did not acknowledge.
    pub holders: BTreeSet<MemberId>,
    /// The member whose action caused the escalation: for a slot, the first member waiting for it
    /// (or the one it is offered to); for a conversation, the author of the message that was not
    /// acknowledged. `None` when the subject names nobody, and then the member escalating is it.
    pub requester: Option<MemberId>,
}

/// The parties of a work item: its holder, when a claim, live or stale, is on it. Nobody else is
/// named, so the member escalating is the requester.
pub fn item_parties(holder: Option<MemberId>) -> Parties {
    Parties {
        holders: holder.into_iter().collect(),
        requester: None,
    }
}

/// The parties of a slot: who holds its group, and who waits for it first — the member it is
/// offered to, or else the first in its group's queue.
pub fn slot_parties(standing: &SlotStanding) -> Parties {
    Parties {
        holders: standing.hold.iter().map(|hold| hold.member).collect(),
        requester: standing
            .offer
            .as_ref()
            .map(|offer| offer.member)
            .or_else(|| standing.queue.first().map(|waiter| waiter.member)),
    }
}

/// A message of a conversation that asked for an acknowledgement and still waits for some.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unanswered {
    /// The member it speaks for, who asked.
    pub sender: Option<MemberId>,
    /// The recipients who did not acknowledge it.
    pub awaiting: BTreeSet<MemberId>,
}

/// The parties of a conversation: the holder of the work item it is about, and, for its newest
/// message still waiting for acknowledgements, the recipients who did not give one and the member
/// who asked.
pub fn thread_parties(item_holder: Option<MemberId>, unanswered: Option<&Unanswered>) -> Parties {
    let mut holders: BTreeSet<MemberId> = item_holder.into_iter().collect();
    if let Some(unanswered) = unanswered {
        holders.extend(unanswered.awaiting.iter().copied());
    }
    Parties {
        holders,
        requester: unanswered.and_then(|unanswered| unanswered.sender),
    }
}

/// Why an escalation is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EscalationRefused {
    /// The project names no arbiter who is a person in it, other than the member escalating.
    #[error(
        "this project names no arbiter to settle it; an admin names them with arbiters in the settings"
    )]
    NoArbiters,
}

/// Whom an escalation by `by` reaches: every arbiter who is still a person of the project, the
/// subject's holders and its requester — the member escalating when the subject names no requester
/// — never `by` themself, who writes it. `people` are the members who may receive messages.
///
/// # Errors
/// [`EscalationRefused::NoArbiters`] when no arbiter other than `by` is among `people`.
pub fn escalation_reach(
    arbiters: &BTreeSet<MemberId>,
    parties: &Parties,
    by: MemberId,
    people: &BTreeSet<MemberId>,
) -> Result<BTreeSet<MemberId>, EscalationRefused> {
    let judges: BTreeSet<MemberId> = arbiters
        .iter()
        .copied()
        .filter(|arbiter| *arbiter != by && people.contains(arbiter))
        .collect();
    if judges.is_empty() {
        return Err(EscalationRefused::NoArbiters);
    }
    let requester = parties.requester.unwrap_or(by);
    Ok(judges
        .into_iter()
        .chain(parties.holders.iter().copied())
        .chain(std::iter::once(requester))
        .filter(|member| *member != by && people.contains(member))
        .collect())
}

/// The first acknowledgement of an escalation by one of the arbiters it reached, which settles it;
/// `None` while it still waits for them.
pub fn settled_by(arbiters: &BTreeSet<MemberId>, acks: &[Ack]) -> Option<Ack> {
    acks.iter()
        .filter(|ack| arbiters.contains(&ack.member))
        .min_by_key(|ack| (ack.at, ack.member))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MessageId;
    use crate::testing::{ALICE, BOB, CAROL, at};

    const DAVE: MemberId = MemberId(4);
    const ERIN: MemberId = MemberId(5);

    fn everyone() -> BTreeSet<MemberId> {
        BTreeSet::from([ALICE, BOB, CAROL, DAVE, ERIN])
    }

    #[test]
    fn an_escalation_reaches_the_arbiters_the_holder_and_the_requester_but_not_its_writer() {
        let parties = Parties {
            holders: BTreeSet::from([BOB]),
            requester: Some(CAROL),
        };
        let reached =
            escalation_reach(&BTreeSet::from([ALICE]), &parties, DAVE, &everyone()).unwrap();
        assert_eq!(reached, BTreeSet::from([ALICE, BOB, CAROL]));
    }

    #[test]
    fn without_a_requester_the_member_escalating_is_it_and_writes_it_unasked() {
        let parties = Parties {
            holders: BTreeSet::from([BOB]),
            requester: None,
        };
        let reached =
            escalation_reach(&BTreeSet::from([ALICE]), &parties, CAROL, &everyone()).unwrap();
        assert_eq!(reached, BTreeSet::from([ALICE, BOB]));
    }

    #[test]
    fn an_arbiter_who_escalates_needs_another_arbiter() {
        let parties = Parties::default();
        assert_eq!(
            escalation_reach(&BTreeSet::from([ALICE]), &parties, ALICE, &everyone()),
            Err(EscalationRefused::NoArbiters)
        );
        assert_eq!(
            escalation_reach(&BTreeSet::new(), &parties, ALICE, &everyone()),
            Err(EscalationRefused::NoArbiters)
        );
        assert_eq!(
            escalation_reach(&BTreeSet::from([ALICE, ERIN]), &parties, ALICE, &everyone()),
            Ok(BTreeSet::from([ERIN]))
        );
    }

    #[test]
    fn a_member_who_left_is_neither_an_arbiter_nor_reached() {
        let present = BTreeSet::from([ALICE, BOB, CAROL]);
        let parties = Parties {
            holders: BTreeSet::from([DAVE]),
            requester: Some(BOB),
        };
        assert_eq!(
            escalation_reach(&BTreeSet::from([ERIN]), &parties, CAROL, &present),
            Err(EscalationRefused::NoArbiters)
        );
        assert_eq!(
            escalation_reach(&BTreeSet::from([ERIN, ALICE]), &parties, CAROL, &present),
            Ok(BTreeSet::from([ALICE, BOB]))
        );
    }

    #[test]
    fn a_slots_requester_is_who_it_is_offered_to_or_else_who_waits_first() {
        use crate::ids::{SessionId, WaiterId};
        use crate::session::HostName;
        use crate::slot::{Liveness, Purpose, SlotHold, SlotOffer, SlotWaiter};

        let slot = SlotName::parse("deploy:prod").unwrap();
        let purpose = Purpose::parse("ship").unwrap();
        let waiter = |id, member, minutes| SlotWaiter {
            id: WaiterId(id),
            slot: slot.clone(),
            member,
            purpose: purpose.clone(),
            since: at(minutes),
            last_seen: at(minutes),
        };
        let mut standing = SlotStanding {
            slot: slot.clone(),
            hold: Some(SlotHold {
                slot: slot.clone(),
                member: ALICE,
                session: SessionId(1),
                host: HostName::parse("laptop").unwrap(),
                purpose: purpose.clone(),
                item: None,
                liveness: Liveness::Session,
                taken_at: at(0),
                expires_at: at(30),
                last_seen: at(0),
                stalled_at: None,
            }),
            queue: vec![waiter(1, BOB, 1), waiter(2, CAROL, 2)],
            offer: None,
        };
        assert_eq!(
            slot_parties(&standing),
            Parties {
                holders: BTreeSet::from([ALICE]),
                requester: Some(BOB)
            }
        );
        standing.hold = None;
        standing.offer = Some(SlotOffer {
            slot: slot.clone(),
            member: DAVE,
            purpose,
            until: at(9),
        });
        assert_eq!(
            slot_parties(&standing),
            Parties {
                holders: BTreeSet::new(),
                requester: Some(DAVE)
            }
        );
    }

    #[test]
    fn a_conversations_requester_is_the_sender_of_the_unanswered_message() {
        let unanswered = Unanswered {
            sender: Some(BOB),
            awaiting: BTreeSet::from([CAROL]),
        };
        assert_eq!(
            thread_parties(Some(ALICE), Some(&unanswered)),
            Parties {
                holders: BTreeSet::from([ALICE, CAROL]),
                requester: Some(BOB)
            }
        );
        assert_eq!(
            thread_parties(None, None),
            Parties::default(),
            "nothing unanswered names nobody"
        );
        assert_eq!(item_parties(Some(ALICE)).holders, BTreeSet::from([ALICE]));
        assert_eq!(item_parties(None), Parties::default());
    }

    fn ack(member: MemberId, minutes: i64) -> Ack {
        Ack {
            message: MessageId(9),
            member,
            at: at(minutes),
        }
    }

    #[test]
    fn an_escalation_stands_until_an_arbiter_acknowledges_it() {
        let arbiters = BTreeSet::from([ALICE, ERIN]);
        assert_eq!(settled_by(&arbiters, &[]), None);
        assert_eq!(
            settled_by(&arbiters, &[ack(BOB, 1)]),
            None,
            "the holder's acknowledgement settles nothing"
        );
        assert_eq!(
            settled_by(&arbiters, &[ack(BOB, 1), ack(ERIN, 5), ack(ALICE, 3)]),
            Some(ack(ALICE, 3)),
            "the first arbiter's acknowledgement settles it"
        );
    }

    #[test]
    fn subjects_serialise_as_one_named_field() {
        let item = serde_json::to_value(EscalationSubject::Item(ItemId(12))).unwrap();
        assert_eq!(item, serde_json::json!({"item": 12}));
        let slot = EscalationSubject::Slot(SlotName::parse("deploy:prod").unwrap());
        assert_eq!(
            serde_json::to_value(&slot).unwrap(),
            serde_json::json!({"slot": "deploy:prod"})
        );
        let thread = serde_json::to_value(EscalationSubject::Thread(ThreadId(5))).unwrap();
        assert_eq!(thread, serde_json::json!({"thread": 5}));
        assert_eq!(EscalationSubject::Item(ItemId(12)).item(), Some(ItemId(12)));
        assert_eq!(slot.item(), None);
        let back: EscalationSubject =
            serde_json::from_value(serde_json::json!({"slot": "deploy:prod"})).unwrap();
        assert_eq!(back, slot);
        assert_eq!(slot.to_string(), "the slot deploy:prod");
    }

    #[test]
    fn reason_table() {
        assert_eq!(
            EscalationReason::parse("  We both need billing today.\nWho goes first?  ")
                .unwrap()
                .as_str(),
            "We both need billing today.\nWho goes first?"
        );
        assert!(EscalationReason::parse(&"r".repeat(2000)).is_ok());
        for bad in ["", "   ", "a\u{7}b", &"r".repeat(2001)] {
            assert_eq!(
                EscalationReason::parse(bad),
                Err(EscalationReasonError::Malformed),
                "{bad:?}"
            );
        }
    }
}
