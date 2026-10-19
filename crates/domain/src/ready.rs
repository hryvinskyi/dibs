//! Ready work: the items a member can pick up now, soonest first, and why each of the others cannot
//! be picked up.

use std::collections::{BTreeMap, BTreeSet};

use time::OffsetDateTime;

use crate::area::QualifiedArea;
use crate::claim::{Claim, held_by_other};
use crate::dependency::{Dependency, blockers};
use crate::ids::{ItemId, MemberId};
use crate::item::{ItemStatus, Priority};
use crate::settings::ProjectSettings;

/// An item as the ready list weighs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<'a> {
    /// Its id.
    pub id: ItemId,
    /// Where it stands.
    pub status: ItemStatus,
    /// Whether its tracker closed it.
    pub closed: bool,
    /// How soon it should be done.
    pub priority: Priority,
    /// The areas the work will probably touch.
    pub areas_hint: &'a BTreeSet<QualifiedArea>,
    /// When it was created.
    pub created_at: OffsetDateTime,
}

/// An item a member can pick up now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyItem {
    /// The item.
    pub item: ItemId,
    /// How soon.
    pub priority: Priority,
    /// The member whose claim on it went stale: the item is ready, and taking it means an explicit
    /// takeover.
    pub stale_claim: Option<MemberId>,
}

/// Why an open item cannot be picked up now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotReady {
    /// A member's live claim holds it.
    Claimed {
        /// The member.
        member: MemberId,
    },
    /// Items it waits for are neither done nor closed.
    BlockedBy {
        /// Those items, in id order.
        items: Vec<ItemId>,
    },
    /// An area of its hint is held by another member's live claim.
    AreaHeld {
        /// The area.
        area: QualifiedArea,
        /// The holder.
        member: MemberId,
        /// The item of the holder's claim.
        item: ItemId,
    },
}

/// An open item that is not ready, with every reason why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excluded {
    /// The item.
    pub item: ItemId,
    /// How soon.
    pub priority: Priority,
    /// The reasons: its claim first, then its blockers, then the held areas in area order.
    pub reasons: Vec<NotReady>,
}

/// The ready list and, beside it, the open items left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyList {
    /// The items `me` can pick up, by priority, then oldest first, then by id.
    pub ready: Vec<ReadyItem>,
    /// The open items that are not ready, in the same order.
    pub excluded: Vec<Excluded>,
}

/// The ready list as `me` sees it at `now`.
///
/// An item is weighed when it is open — not done, not dropped, not closed by its tracker. It is
/// ready when no live claim holds it (a stale claim does not hide it), nothing it waits for still
/// blocks it, and no area of its hint is held by another member's live claim. Every other open item
/// goes to the excluded list with its reasons.
pub fn ready_items(
    items: &[Candidate<'_>],
    claims: &[Claim],
    dependencies: &[Dependency],
    me: MemberId,
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> ReadyList {
    let stale_after = settings.stale_after();
    let states: BTreeMap<ItemId, (ItemStatus, bool)> = items
        .iter()
        .map(|item| (item.id, (item.status, item.closed)))
        .collect();
    let claim_of: BTreeMap<ItemId, &Claim> =
        claims.iter().map(|claim| (claim.item, claim)).collect();
    let mut open: Vec<&Candidate<'_>> = items
        .iter()
        .filter(|item| {
            !item.closed && matches!(item.status, ItemStatus::Open | ItemStatus::Claimed)
        })
        .collect();
    open.sort_by_key(|item| (item.priority, item.created_at, item.id));
    let mut list = ReadyList::default();
    for item in open {
        let claim = claim_of.get(&item.id).copied();
        let mut reasons = Vec::new();
        if let Some(live) = claim.filter(|claim| claim.is_live(now, stale_after)) {
            reasons.push(NotReady::Claimed {
                member: live.member,
            });
        }
        let waiting = blockers(item.id, dependencies, |id| states.get(&id).copied());
        if !waiting.is_empty() {
            reasons.push(NotReady::BlockedBy { items: waiting });
        }
        let others = claims.iter().filter(|claim| claim.item != item.id);
        for area in item.areas_hint {
            if let Some(holder) = held_by_other(area, me, others.clone(), now, stale_after) {
                reasons.push(NotReady::AreaHeld {
                    area: area.clone(),
                    member: holder.member,
                    item: holder.item,
                });
            }
        }
        if reasons.is_empty() {
            list.ready.push(ReadyItem {
                item: item.id,
                priority: item.priority,
                stale_claim: claim.map(|stale| stale.member),
            });
        } else {
            list.excluded.push(Excluded {
                item: item.id,
                priority: item.priority,
                reasons,
            });
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::dependency::DependencySource;
    use crate::testing::{ALICE, BOB, CAROL, area, areas, at, claim, two_repo_settings};

    struct Item {
        id: u64,
        status: ItemStatus,
        closed: bool,
        priority: u8,
        hint: BTreeSet<QualifiedArea>,
        created: i64,
    }

    fn item(id: u64, priority: u8, created: i64, hint: &[&str]) -> Item {
        Item {
            id,
            status: ItemStatus::Open,
            closed: false,
            priority,
            hint: areas(hint),
            created,
        }
    }

    fn candidates(items: &[Item]) -> Vec<Candidate<'_>> {
        items
            .iter()
            .map(|item| Candidate {
                id: ItemId(item.id),
                status: item.status,
                closed: item.closed,
                priority: Priority::new(item.priority).unwrap(),
                areas_hint: &item.hint,
                created_at: at(item.created),
            })
            .collect()
    }

    fn edge(item: u64, blocked_by: u64) -> Dependency {
        Dependency {
            item: ItemId(item),
            blocked_by: ItemId(blocked_by),
            source: DependencySource::Manual,
        }
    }

    fn ready_ids(list: &ReadyList) -> Vec<u64> {
        list.ready.iter().map(|ready| ready.item.get()).collect()
    }

    #[test]
    fn ready_work_is_ordered_by_priority_then_age_then_id() {
        let items = [
            item(1, 2, 30, &[]),
            item(2, 0, 50, &[]),
            item(3, 2, 10, &[]),
            item(4, 4, 0, &[]),
            item(5, 2, 10, &[]),
        ];
        let list = ready_items(
            &candidates(&items),
            &[],
            &[],
            ALICE,
            at(60),
            &two_repo_settings(),
        );
        assert_eq!(ready_ids(&list), [2, 3, 5, 1, 4]);
        assert!(list.excluded.is_empty());
    }

    #[test]
    fn only_open_items_are_weighed() {
        let mut done = item(1, 2, 0, &[]);
        done.status = ItemStatus::Done;
        let mut dropped = item(2, 2, 0, &[]);
        dropped.status = ItemStatus::Dropped;
        let mut closed = item(3, 2, 0, &[]);
        closed.closed = true;
        let items = [done, dropped, closed, item(4, 2, 0, &[])];
        let list = ready_items(
            &candidates(&items),
            &[],
            &[],
            ALICE,
            at(1),
            &two_repo_settings(),
        );
        assert_eq!(ready_ids(&list), [4]);
        assert!(list.excluded.is_empty());
    }

    #[test]
    fn a_live_claim_excludes_its_item_and_a_stale_one_shows_it_as_stale() {
        let settings = two_repo_settings();
        let mut claimed = item(1, 2, 0, &[]);
        claimed.status = ItemStatus::Claimed;
        let mut stale = item(2, 2, 1, &[]);
        stale.status = ItemStatus::Claimed;
        let items = [claimed, stale];
        let now = at(0) + settings.stale_after() + Duration::minutes(30);
        let claims = [claim(1, BOB, &[], 60), claim(2, CAROL, &[], 0)];
        let list = ready_items(&candidates(&items), &claims, &[], ALICE, now, &settings);
        assert_eq!(
            list.ready,
            [ReadyItem {
                item: ItemId(2),
                priority: Priority::default(),
                stale_claim: Some(CAROL),
            }]
        );
        assert_eq!(
            list.excluded,
            [Excluded {
                item: ItemId(1),
                priority: Priority::default(),
                reasons: vec![NotReady::Claimed { member: BOB }],
            }]
        );
    }

    #[test]
    fn my_own_live_claim_also_takes_an_item_off_the_ready_list() {
        let mut mine = item(1, 2, 0, &[]);
        mine.status = ItemStatus::Claimed;
        let items = [mine];
        let claims = [claim(1, ALICE, &[], 0)];
        let list = ready_items(
            &candidates(&items),
            &claims,
            &[],
            ALICE,
            at(1),
            &two_repo_settings(),
        );
        assert_eq!(
            list.excluded[0].reasons,
            [NotReady::Claimed { member: ALICE }]
        );
    }

    #[test]
    fn a_blocked_item_names_the_blockers_that_still_block_it() {
        let mut done = item(3, 2, 0, &[]);
        done.status = ItemStatus::Done;
        let mut closed = item(4, 2, 0, &[]);
        closed.closed = true;
        let items = [
            item(1, 2, 0, &[]),
            item(2, 2, 0, &[]),
            done,
            closed,
            item(5, 2, 0, &[]),
        ];
        let deps = [edge(1, 2), edge(1, 3), edge(1, 4), edge(5, 3), edge(5, 4)];
        let list = ready_items(
            &candidates(&items),
            &[],
            &deps,
            ALICE,
            at(1),
            &two_repo_settings(),
        );
        assert_eq!(ready_ids(&list), [2, 5]);
        assert_eq!(
            list.excluded[0].reasons,
            [NotReady::BlockedBy {
                items: vec![ItemId(2)]
            }]
        );
    }

    #[test]
    fn a_hinted_area_another_member_holds_excludes_the_item_but_mine_does_not() {
        let items = [
            item(1, 2, 0, &["api:billing"]),
            item(2, 2, 0, &["api:search"]),
            item(3, 2, 0, &["web:checkout"]),
        ];
        let mut held_by_bob = item(10, 2, 0, &[]);
        held_by_bob.status = ItemStatus::Claimed;
        let mut held_by_me = item(11, 2, 0, &[]);
        held_by_me.status = ItemStatus::Claimed;
        let mut all = items.into_iter().collect::<Vec<_>>();
        all.push(held_by_bob);
        all.push(held_by_me);
        let claims = [
            claim(10, BOB, &["api:billing"], 0),
            claim(11, ALICE, &["api:search"], 0),
        ];
        let list = ready_items(
            &candidates(&all),
            &claims,
            &[],
            ALICE,
            at(1),
            &two_repo_settings(),
        );
        assert_eq!(ready_ids(&list), [2, 3]);
        let first = list.excluded.iter().find(|e| e.item == ItemId(1)).unwrap();
        assert_eq!(
            first.reasons,
            [NotReady::AreaHeld {
                area: area("api:billing"),
                member: BOB,
                item: ItemId(10),
            }]
        );
        let bobs = ready_items(
            &candidates(&all),
            &claims,
            &[],
            BOB,
            at(1),
            &two_repo_settings(),
        );
        assert_eq!(ready_ids(&bobs), [1, 3]);
    }

    #[test]
    fn every_reason_an_item_has_is_listed_in_order() {
        let settings = two_repo_settings();
        let mut target = item(1, 1, 0, &["web:checkout", "api:billing"]);
        target.status = ItemStatus::Claimed;
        let mut other = item(9, 2, 0, &[]);
        other.status = ItemStatus::Claimed;
        let items = [target, item(2, 2, 0, &[]), other];
        let claims = [
            claim(1, CAROL, &[], 0),
            claim(9, BOB, &["api:billing", "web:checkout"], 0),
        ];
        let list = ready_items(
            &candidates(&items),
            &claims,
            &[edge(1, 2)],
            ALICE,
            at(1),
            &settings,
        );
        let first = list.excluded.iter().find(|e| e.item == ItemId(1)).unwrap();
        assert_eq!(
            first.reasons,
            [
                NotReady::Claimed { member: CAROL },
                NotReady::BlockedBy {
                    items: vec![ItemId(2)]
                },
                NotReady::AreaHeld {
                    area: area("api:billing"),
                    member: BOB,
                    item: ItemId(9),
                },
                NotReady::AreaHeld {
                    area: area("web:checkout"),
                    member: BOB,
                    item: ItemId(9),
                },
            ]
        );
    }
}
