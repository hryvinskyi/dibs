//! A declared slot as the pages name it: its name, its group, where it stands, who holds it and
//! why, until when, when that hold last showed a sign of life, who waits for it or is offered it,
//! and what its declaration says about it.

use time::OffsetDateTime;

use dibs_app::use_cases::SlotLine;

/// Where a slot stands. Whether a hold is stalled is the domain's finding, never the page's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// Nobody holds it, waits for it, or is offered it.
    Free,
    /// Held, nobody waiting.
    Held,
    /// Held, with members waiting.
    Queued,
    /// Held by a run that stopped sending heartbeats.
    Stalled,
    /// Offered to the member first in the queue.
    Offered,
}

impl SlotState {
    /// The state's name: the word a person reads, and what the page's marks key on.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Held => "held",
            Self::Queued => "queued",
            Self::Stalled => "stalled",
            Self::Offered => "offered",
        }
    }
}

/// The hold of a slot's group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotHoldRow {
    /// Why it is held.
    pub purpose: String,
    /// The slot held, when it is another slot of the group.
    pub through: Option<String>,
    /// When it ends unless renewed.
    pub until: OffsetDateTime,
    /// The last sign of life of what keeps it.
    pub last_seen: OffsetDateTime,
}

/// The offer of a slot's group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotOfferRow {
    /// The handle of the member it is offered to.
    pub member: String,
    /// When it lapses.
    pub until: OffsetDateTime,
}

/// A declared slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRow {
    /// The slot's name.
    pub name: String,
    /// Its group, when it has one.
    pub group: Option<String>,
    /// Where it stands.
    pub state: SlotState,
    /// The handle of its group's holder, while it is held.
    pub holder: Option<String>,
    /// Its group's hold.
    pub hold: Option<SlotHoldRow>,
    /// The handles of the members waiting, first in line first.
    pub waiting: Vec<String>,
    /// Its group's offer.
    pub offer: Option<SlotOfferRow>,
    /// Its note.
    pub note: Option<String>,
}

impl SlotRow {
    /// The slot of a line of the status.
    pub fn of(line: &SlotLine) -> Self {
        let waiting: Vec<String> = line
            .queue
            .iter()
            .map(|place| place.member.to_string())
            .collect();
        let state = match (&line.hold, &line.offer) {
            (Some(named), _) if named.hold.is_stalled() => SlotState::Stalled,
            (Some(_), _) if !waiting.is_empty() => SlotState::Queued,
            (Some(_), _) => SlotState::Held,
            (None, Some(_)) => SlotState::Offered,
            (None, None) => SlotState::Free,
        };
        Self {
            name: line.spec.name.to_string(),
            group: line.spec.group.as_ref().map(ToString::to_string),
            state,
            holder: line.hold.as_ref().map(|named| named.holder.to_string()),
            hold: line.hold.as_ref().map(|named| SlotHoldRow {
                purpose: named.hold.purpose.to_string(),
                through: (named.hold.slot != line.spec.name).then(|| named.hold.slot.to_string()),
                until: named.hold.expires_at,
                last_seen: named.hold.last_seen,
            }),
            waiting,
            offer: line.offer.as_ref().map(|named| SlotOfferRow {
                member: named.member.to_string(),
                until: named.offer.until,
            }),
            note: line.spec.note.as_ref().map(ToString::to_string),
        }
    }
}

#[cfg(test)]
mod tests {
    use dibs_app::use_cases::{NamedHold, NamedOffer, NamedWaiter};
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{MemberId, SessionId, WaiterId};
    use dibs_domain::session::HostName;
    use dibs_domain::slot::{
        DEFAULT_MAX_WAIT, DEFAULT_OFFER_FOR, Liveness, Purpose, SlotGroup, SlotHold, SlotName,
        SlotOffer, SlotSpec, SlotWaiter,
    };
    use time::Duration;
    use time::macros::datetime;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-10-19 14:00:00 UTC);

    fn name(value: &str) -> SlotName {
        SlotName::parse(value).unwrap()
    }

    fn handle(value: &str) -> Handle {
        Handle::parse(value).unwrap()
    }

    fn purpose(value: &str) -> Purpose {
        Purpose::parse(value).unwrap()
    }

    fn line(slot: &str) -> SlotLine {
        SlotLine {
            spec: SlotSpec {
                name: name(slot),
                max_hold: Duration::minutes(30),
                offer_for: DEFAULT_OFFER_FOR,
                max_wait: DEFAULT_MAX_WAIT,
                commands: Vec::new(),
                run_only: false,
                group: Some(SlotGroup::parse("deploy").unwrap()),
                freezes: Vec::new(),
                note: None,
            },
            hold: None,
            queue: Vec::new(),
            offer: None,
        }
    }

    fn hold(slot: &str, holder: &str, stalled: bool) -> NamedHold {
        NamedHold {
            hold: SlotHold {
                slot: name(slot),
                member: MemberId(2),
                session: SessionId(7),
                host: HostName::parse("laptop").unwrap(),
                purpose: purpose("ship the release"),
                item: None,
                liveness: Liveness::Session,
                taken_at: NOW - Duration::minutes(20),
                expires_at: NOW + Duration::minutes(10),
                last_seen: NOW - Duration::minutes(4),
                stalled_at: stalled.then_some(NOW - Duration::minutes(2)),
            },
            holder: handle(holder),
        }
    }

    fn waiter(slot: &str, member: &str, id: u64) -> NamedWaiter {
        NamedWaiter {
            waiter: SlotWaiter {
                id: WaiterId(id),
                slot: name(slot),
                member: MemberId(id + 10),
                purpose: purpose("my turn"),
                since: NOW - Duration::minutes(5),
                last_seen: NOW,
            },
            member: handle(member),
        }
    }

    fn offer(slot: &str, member: &str) -> NamedOffer {
        NamedOffer {
            offer: SlotOffer {
                slot: name(slot),
                member: MemberId(3),
                purpose: purpose("my turn"),
                until: NOW + Duration::minutes(3),
            },
            member: handle(member),
        }
    }

    #[test]
    fn a_slot_nobody_holds_waits_for_or_is_offered_is_free() {
        let row = SlotRow::of(&line("deploy:prod"));
        assert_eq!(row.state, SlotState::Free);
        assert_eq!(row.holder, None);
        assert_eq!(row.hold, None);
        assert!(row.waiting.is_empty());
        assert_eq!(row.offer, None);
    }

    #[test]
    fn a_hold_with_nobody_waiting_is_held_and_carries_its_purpose_until_and_last_sign_of_life() {
        let mut slot = line("deploy:prod");
        slot.hold = Some(hold("deploy:prod", "bob", false));
        let row = SlotRow::of(&slot);
        assert_eq!(row.state, SlotState::Held);
        assert_eq!(row.holder.as_deref(), Some("bob"));
        assert_eq!(
            row.hold,
            Some(SlotHoldRow {
                purpose: "ship the release".to_owned(),
                through: None,
                until: NOW + Duration::minutes(10),
                last_seen: NOW - Duration::minutes(4),
            })
        );
    }

    #[test]
    fn a_hold_with_members_waiting_is_queued_and_lists_them_first_in_line_first() {
        let mut slot = line("deploy:prod");
        slot.hold = Some(hold("deploy:prod", "bob", false));
        slot.queue = vec![
            waiter("deploy:prod", "carol", 1),
            waiter("deploy:prod", "dave", 2),
        ];
        let row = SlotRow::of(&slot);
        assert_eq!(row.state, SlotState::Queued);
        assert_eq!(row.waiting, ["carol", "dave"]);
    }

    #[test]
    fn a_stalled_hold_is_stalled_even_with_members_waiting() {
        let mut slot = line("deploy:prod");
        slot.hold = Some(hold("deploy:prod", "bob", true));
        slot.queue = vec![waiter("deploy:prod", "carol", 1)];
        let row = SlotRow::of(&slot);
        assert_eq!(row.state, SlotState::Stalled);
        assert_eq!(row.waiting, ["carol"]);
    }

    #[test]
    fn a_hold_whose_last_sign_of_life_is_old_is_not_stalled_until_the_domain_says_so() {
        let mut slot = line("deploy:prod");
        let mut silent = hold("deploy:prod", "bob", false);
        silent.hold.last_seen = NOW - Duration::hours(3);
        slot.hold = Some(silent);
        assert_eq!(SlotRow::of(&slot).state, SlotState::Held);
    }

    #[test]
    fn an_offer_without_a_hold_is_offered_and_names_its_member_and_deadline() {
        let mut slot = line("deploy:prod");
        slot.offer = Some(offer("deploy:prod", "carol"));
        let row = SlotRow::of(&slot);
        assert_eq!(row.state, SlotState::Offered);
        assert_eq!(row.holder, None);
        assert_eq!(
            row.offer,
            Some(SlotOfferRow {
                member: "carol".to_owned(),
                until: NOW + Duration::minutes(3),
            })
        );
    }

    #[test]
    fn a_hold_of_another_slot_of_the_group_names_the_slot_held() {
        let mut slot = line("deploy:staging");
        slot.hold = Some(hold("deploy:prod", "bob", false));
        let row = SlotRow::of(&slot);
        assert_eq!(
            row.hold.and_then(|hold| hold.through).as_deref(),
            Some("deploy:prod")
        );
    }

    #[test]
    fn every_state_has_a_name_of_its_own() {
        let states = [
            SlotState::Free,
            SlotState::Held,
            SlotState::Queued,
            SlotState::Stalled,
            SlotState::Offered,
        ];
        let names: Vec<&str> = states.iter().map(|state| state.name()).collect();
        assert_eq!(names, ["free", "held", "queued", "stalled", "offered"]);
    }
}
