//! What dibs tells a member when time alone ended something of theirs among the slots: their hold
//! expired, or the slot offered to them lapsed and they left the queue. Nobody acts when that
//! happens, so nobody else would tell them.

use crate::event::EventKind;
use crate::ids::MemberId;
use crate::slot::spec::SlotName;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotNoticeKind {
    /// The member's hold reached its expiry.
    HoldExpired,
    /// The slot offered to the member lapsed untaken; they are out of the queue.
    OfferLapsed,
}

/// One member to be told about one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotNotice {
    /// Whom to tell.
    pub member: MemberId,
    /// The slot.
    pub slot: SlotName,
    /// What happened.
    pub kind: SlotNoticeKind,
}

/// The notices the slot events of one change owe, in the order of the events: one to the holder of
/// each hold that expired and one to the member of each offer that lapsed. Every other event owes
/// none.
pub fn notices_owed(events: &[EventKind]) -> Vec<SlotNotice> {
    events
        .iter()
        .filter_map(|event| {
            let (member, slot, kind) = match event {
                EventKind::SlotExpired(e) => (e.member, &e.slot, SlotNoticeKind::HoldExpired),
                EventKind::SlotOfferLapsed(e) => (e.member, &e.slot, SlotNoticeKind::OfferLapsed),
                _ => return None,
            };
            Some(SlotNotice {
                member,
                slot: slot.clone(),
                kind,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{SlotMember, SlotReleased};
    use crate::testing::{ALICE, BOB};

    #[test]
    fn an_expiry_and_a_lapse_each_owe_their_member_one_notice() {
        let slot = SlotName::parse("deploy:prod").unwrap();
        let one = |member| SlotMember {
            slot: slot.clone(),
            member,
        };
        let events = vec![
            EventKind::SlotExpired(one(ALICE)),
            EventKind::SlotOffered(one(BOB)),
            EventKind::SlotReleased(SlotReleased {
                slot: slot.clone(),
                member: BOB,
                exit: None,
            }),
            EventKind::SlotOfferLapsed(one(BOB)),
            EventKind::SlotLeft(one(ALICE)),
        ];
        assert_eq!(
            notices_owed(&events),
            [
                SlotNotice {
                    member: ALICE,
                    slot: slot.clone(),
                    kind: SlotNoticeKind::HoldExpired
                },
                SlotNotice {
                    member: BOB,
                    slot,
                    kind: SlotNoticeKind::OfferLapsed
                },
            ]
        );
        assert!(notices_owed(&[]).is_empty());
    }
}
