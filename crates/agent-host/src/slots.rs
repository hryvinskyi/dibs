//! The slots a command line touches, as the domain's board: the server names members by handle, the
//! domain decides over member ids, so each handle gets an id of its own here and the verdict's ids
//! are named back.

use std::collections::BTreeMap;

use dibs_domain::handle::Handle;
use dibs_domain::ids::{MemberId, WaiterId};
use dibs_domain::slot::{SlotBoard, SlotHold, SlotOffer, SlotWaiter};

use crate::ports::SlotState;

/// The board of the slots asked about, and who each member id is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownBoard {
    /// The holds, waiters and offers of the slots' groups, each once.
    pub board: SlotBoard,
    /// The member the guard decides for.
    pub me: MemberId,
    members: BTreeMap<MemberId, Handle>,
}

impl KnownBoard {
    /// The board of `states`, deciding for `me`.
    pub fn of(states: &[SlotState], me: &Handle) -> Self {
        let mut known = Self {
            board: SlotBoard::default(),
            me: MemberId(1),
            members: BTreeMap::from([(MemberId(1), me.clone())]),
        };
        for state in states {
            if let Some(hold) = &state.hold
                && known.board.hold(&hold.slot).is_none()
            {
                let member = known.id_of(&hold.holder);
                known.board.holds.push(SlotHold {
                    slot: hold.slot.clone(),
                    member,
                    session: hold.session,
                    host: hold.host.clone(),
                    purpose: hold.purpose.clone(),
                    item: hold.item,
                    liveness: hold.liveness.clone(),
                    taken_at: hold.taken_at,
                    expires_at: hold.expires_at,
                    last_seen: hold.last_seen,
                    stalled_at: hold.stalled.then_some(hold.last_seen),
                });
            }
            for waiter in &state.queue {
                let member = known.id_of(&waiter.member);
                let placed = known
                    .board
                    .waiters
                    .iter()
                    .any(|known| known.member == member && known.slot == waiter.slot);
                if !placed {
                    let id = WaiterId(u64::try_from(known.board.waiters.len()).unwrap_or(0) + 1);
                    known.board.waiters.push(SlotWaiter {
                        id,
                        slot: waiter.slot.clone(),
                        member,
                        purpose: waiter.purpose.clone(),
                        since: waiter.since,
                        last_seen: waiter.last_seen,
                    });
                }
            }
            if let Some(offer) = &state.offer
                && !known
                    .board
                    .offers
                    .iter()
                    .any(|known| known.slot == offer.slot)
            {
                let member = known.id_of(&offer.member);
                known.board.offers.push(SlotOffer {
                    slot: offer.slot.clone(),
                    member,
                    purpose: offer.purpose.clone(),
                    until: offer.until,
                });
            }
        }
        known
    }

    /// The handle of a member id of this board.
    pub fn handle(&self, member: MemberId) -> Option<&Handle> {
        self.members.get(&member)
    }

    fn id_of(&mut self, handle: &Handle) -> MemberId {
        if let Some((id, _)) = self.members.iter().find(|(_, known)| *known == handle) {
            return *id;
        }
        let id = MemberId(u64::try_from(self.members.len()).unwrap_or(0) + 1);
        self.members.insert(id, handle.clone());
        id
    }
}

#[cfg(test)]
mod tests {
    use time::{Duration, OffsetDateTime};

    use dibs_domain::repo::RepoId;
    use dibs_domain::session::HostName;
    use dibs_domain::slot::{
        CommandVerdict, Liveness, Purpose, SlotName, command_verdict, split_commands,
    };

    use super::*;
    use crate::ports::{SlotHoldState, SlotOfferState, SlotWaiterState};

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn slot(text: &str) -> SlotName {
        SlotName::parse(text).unwrap()
    }

    fn held_by(holder: &str) -> SlotState {
        let at = OffsetDateTime::UNIX_EPOCH;
        SlotState {
            slot: slot("deploy:prod"),
            hold: Some(SlotHoldState {
                slot: slot("deploy:prod"),
                holder: handle(holder),
                purpose: Purpose::parse("ship 2.4").unwrap(),
                session: dibs_domain::ids::SessionId(7),
                host: HostName::parse("laptop").unwrap(),
                item: None,
                liveness: Liveness::Session,
                taken_at: at,
                expires_at: at + Duration::minutes(30),
                last_seen: at,
                stalled: false,
            }),
            queue: vec![SlotWaiterState {
                member: handle("carol"),
                slot: slot("deploy:prod"),
                purpose: Purpose::parse("hotfix").unwrap(),
                since: at,
                last_seen: at,
            }],
            offer: None,
        }
    }

    #[test]
    fn members_are_told_apart_by_handle_and_named_back() {
        let me = handle("bob");
        let state = held_by("alice");
        let known = KnownBoard::of(&[state.clone(), state], &me);
        assert_eq!(known.board.holds.len(), 1, "one group's hold once");
        assert_eq!(known.board.waiters.len(), 1);
        let holder = known.board.holds[0].member;
        assert_ne!(holder, known.me);
        assert_eq!(known.handle(holder), Some(&handle("alice")));
        assert_eq!(known.handle(known.me), Some(&me));
        let mine = KnownBoard::of(&[held_by("bob")], &me);
        assert_eq!(mine.board.holds[0].member, mine.me, "my own hold is mine");
    }

    #[test]
    fn the_domain_decides_over_the_board_as_it_would_over_the_servers() {
        let settings = dibs_domain::settings::ProjectSettings::parse(
            &serde_json::from_str(
                r#"{"slots": [{"name": "deploy:prod", "max_hold": "30m",
                    "commands": [{"matches": "^deploy\\.sh prod"}]}]}"#,
            )
            .unwrap(),
            &dibs_domain::hot_rule::HotRuleRegistry::standard(),
        )
        .unwrap();
        let me = handle("bob");
        let decide = |known: &KnownBoard| {
            command_verdict(
                &split_commands("./deploy.sh prod"),
                &RepoId::implicit(),
                settings.slots(),
                &known.board,
                known.me,
                None,
            )
        };
        let theirs = KnownBoard::of(&[held_by("alice")], &me);
        assert!(matches!(
            decide(&theirs),
            CommandVerdict::NeedsSlot {
                waiting: 1,
                holder: Some(_),
                ..
            }
        ));
        let offered = KnownBoard::of(
            &[SlotState {
                hold: None,
                offer: Some(SlotOfferState {
                    member: me.clone(),
                    slot: slot("deploy:prod"),
                    purpose: Purpose::parse("ship").unwrap(),
                    until: OffsetDateTime::UNIX_EPOCH,
                }),
                ..held_by("alice")
            }],
            &me,
        );
        assert!(matches!(
            decide(&offered),
            CommandVerdict::NeedsSlot {
                offered_to_me: true,
                holder: None,
                ..
            }
        ));
        let mine = KnownBoard::of(&[held_by("bob")], &me);
        assert!(matches!(decide(&mine), CommandVerdict::Holding { .. }));
    }
}
