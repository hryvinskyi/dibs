//! What the queue lists, and the narrowings a link can ask of it.
//!
//! The queue lists the ready items, then the open items left out of the ready list for another
//! reason than a claim, each group in the status's order. An item a live claim holds is a claim,
//! not a ticket that waits, so the queue never lists it. Every page that lists the queue, counts
//! it, or decides whether an item has a page in it asks [`listed`], so they agree on one rule.

use dibs_app::records::ItemRecord;
use dibs_app::use_cases::{ExcludedEntry, NamedNotReady, StatusView};
use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;

/// An item the queue lists, and whether it can be claimed now.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Listed<'a> {
    /// The item.
    pub(crate) item: &'a ItemRecord,
    /// Whether it can be claimed now, or why it cannot.
    pub(crate) standing: Standing<'a>,
}

/// Whether an item the queue lists can be claimed now.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Standing<'a> {
    /// It can.
    Ready {
        /// The member whose claim on it went silent; taking it is an explicit take-over.
        stale_holder: Option<&'a Handle>,
    },
    /// It cannot, for each of these reasons, in the order the status gives.
    NotReady {
        /// What holds it back.
        reasons: &'a [NamedNotReady],
    },
}

impl<'a> Listed<'a> {
    /// Why it cannot be claimed now; empty for a ready item.
    pub(crate) fn reasons(&self) -> &'a [NamedNotReady] {
        match self.standing {
            Standing::Ready { .. } => &[],
            Standing::NotReady { reasons } => reasons,
        }
    }
}

/// The items the queue lists, the ready ones first.
pub(crate) fn listed(status: &StatusView) -> impl Iterator<Item = Listed<'_>> {
    let ready = status.ready.ready.iter().map(|entry| Listed {
        item: &entry.item.item,
        standing: Standing::Ready {
            stale_holder: entry.stale_holder.as_ref(),
        },
    });
    let waiting = status
        .ready
        .excluded
        .iter()
        .filter(|entry| waits(entry))
        .map(|entry| Listed {
            item: &entry.item.item,
            standing: Standing::NotReady {
                reasons: &entry.reasons,
            },
        });
    ready.chain(waiting)
}

/// Whether an item left out of the ready list waits in the queue: no claim holds it.
fn waits(entry: &ExcludedEntry) -> bool {
    !entry
        .reasons
        .iter()
        .any(|reason| matches!(reason, NamedNotReady::Claimed { .. }))
}

/// A part of the queue a link names exactly: the tickets a claim keeps out of the queue by holding
/// an area they will touch, the tickets that will touch an area, or both at once. The empty
/// narrowing keeps every ticket.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Narrowing {
    /// The item of the claim whose held areas the tickets wait for.
    pub(crate) kept_by: Option<ItemId>,
    /// The area the tickets will touch.
    pub(crate) touches: Option<QualifiedArea>,
}

impl Narrowing {
    /// The tickets the claim on the item `id` keeps out of the queue.
    pub(crate) fn kept_by(id: ItemId) -> Self {
        Self {
            kept_by: Some(id),
            touches: None,
        }
    }

    /// The tickets that will touch `area`.
    pub(crate) fn touching(area: QualifiedArea) -> Self {
        Self {
            kept_by: None,
            touches: Some(area),
        }
    }

    /// Whether it leaves out any ticket.
    pub(crate) fn narrows(&self) -> bool {
        self.kept_by.is_some() || self.touches.is_some()
    }

    /// Whether it keeps the ticket `entry`.
    pub(crate) fn keeps(&self, entry: &Listed<'_>) -> bool {
        let kept = self.kept_by.is_none_or(|id| {
            entry.reasons().iter().any(
                |reason| matches!(reason, NamedNotReady::AreaHeld { item, .. } if item.id == id),
            )
        });
        let touching = self
            .touches
            .as_ref()
            .is_none_or(|area| entry.item.areas_hint.contains(area));
        kept && touching
    }
}

#[cfg(test)]
mod tests {
    use dibs_app::named::{ItemName, NamedItem};
    use dibs_app::use_cases::{ReadyEntry, ReadyView};
    use dibs_domain::ids::MemberId;
    use dibs_domain::item::{ItemRepository, Title};
    use time::OffsetDateTime;

    use super::*;

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    fn named(id: u64, areas: &[&str]) -> NamedItem {
        let mut item = ItemRecord::new(
            ItemId(id),
            None,
            Title::parse("Work").unwrap(),
            ItemRepository::Unnamed,
            MemberId(1),
            OffsetDateTime::UNIX_EPOCH,
        );
        item.areas_hint = areas.iter().map(|text| area(text)).collect();
        NamedItem {
            item,
            created_by: handle("alice"),
        }
    }

    fn name(id: u64) -> ItemName {
        ItemName {
            id: ItemId(id),
            external_ref: None,
            title: Title::parse("Other work").unwrap(),
            link: None,
        }
    }

    fn held_by(id: u64, on: &str) -> NamedNotReady {
        NamedNotReady::AreaHeld {
            area: area(on),
            holder: handle("bob"),
            item: name(id),
        }
    }

    fn excluded(id: u64, areas: &[&str], reasons: Vec<NamedNotReady>) -> ExcludedEntry {
        ExcludedEntry {
            item: named(id, areas),
            link: None,
            reasons,
        }
    }

    fn status(ready: Vec<ReadyEntry>, excluded: Vec<ExcludedEntry>) -> StatusView {
        StatusView {
            ready: ReadyView { ready, excluded },
            live: Vec::new(),
            stale: Vec::new(),
            holds: Vec::new(),
            wide: Vec::new(),
            slots: Vec::new(),
            worktrees: Vec::new(),
            escalations: Vec::new(),
            integration_silent_since: None,
            overlaps: Vec::new(),
        }
    }

    /// The ready entry of `id`, its claim gone silent when `stale` names the holder.
    fn ready(id: u64, areas: &[&str], stale: Option<&str>) -> ReadyEntry {
        ReadyEntry {
            item: named(id, areas),
            link: None,
            stale_holder: stale.map(handle),
        }
    }

    /// A project where 1 and 2 are ready (2's claim went silent), 3 is claimed, 4 waits for the area
    /// the claim on 9 holds, 5 waits for 1 to be done and also for the claim on 9, and 6 is claimed
    /// while waiting for an area.
    fn project() -> StatusView {
        let claimed = || NamedNotReady::Claimed {
            holder: handle("bob"),
        };
        status(
            vec![
                ready(1, &["api:billing"], None),
                ready(2, &["api:search"], Some("carol")),
            ],
            vec![
                excluded(3, &["api:billing"], vec![claimed()]),
                excluded(4, &["api:billing"], vec![held_by(9, "api:billing")]),
                excluded(
                    5,
                    &["api:billing", "api:search"],
                    vec![
                        NamedNotReady::BlockedBy {
                            items: vec![name(1)],
                        },
                        held_by(9, "api:billing"),
                    ],
                ),
                excluded(
                    6,
                    &["api:billing"],
                    vec![claimed(), held_by(9, "api:billing")],
                ),
            ],
        )
    }

    fn ids<'a>(entries: impl Iterator<Item = Listed<'a>>) -> Vec<u64> {
        entries.map(|entry| entry.item.id.0).collect()
    }

    #[test]
    fn the_queue_lists_the_ready_items_then_those_left_out_for_another_reason_than_a_claim() {
        let project = project();
        assert_eq!(ids(listed(&project)), [1, 2, 4, 5]);
    }

    #[test]
    fn a_listed_item_says_whether_it_is_ready_and_why_not() {
        let project = project();
        let entries: Vec<Listed<'_>> = listed(&project).collect();
        assert!(matches!(
            entries[0].standing,
            Standing::Ready { stale_holder: None }
        ));
        assert!(matches!(
            entries[1].standing,
            Standing::Ready { stale_holder: Some(holder) } if holder.as_str() == "carol"
        ));
        assert!(entries[0].reasons().is_empty());
        assert_eq!(entries[2].reasons(), [held_by(9, "api:billing")]);
        assert_eq!(entries[3].reasons().len(), 2);
    }

    #[test]
    fn the_empty_narrowing_keeps_every_ticket() {
        let project = project();
        let every = Narrowing::default();
        assert!(!every.narrows());
        assert_eq!(
            ids(listed(&project).filter(|entry| every.keeps(entry))),
            [1, 2, 4, 5]
        );
    }

    #[test]
    fn a_claim_keeps_out_the_tickets_that_wait_for_an_area_it_holds_and_no_others() {
        let project = project();
        let kept = Narrowing::kept_by(ItemId(9));
        assert!(kept.narrows());
        assert_eq!(
            ids(listed(&project).filter(|entry| kept.keeps(entry))),
            [4, 5]
        );
        let other = Narrowing::kept_by(ItemId(1));
        assert_eq!(
            ids(listed(&project).filter(|entry| other.keeps(entry))),
            Vec::<u64>::new(),
            "waiting for an item to be done is not being kept out by its claim"
        );
    }

    #[test]
    fn an_area_keeps_the_tickets_that_will_touch_it_ready_or_not() {
        let project = project();
        let billing = Narrowing::touching(area("api:billing"));
        assert_eq!(
            ids(listed(&project).filter(|entry| billing.keeps(entry))),
            [1, 4, 5]
        );
        let search = Narrowing::touching(area("api:search"));
        assert_eq!(
            ids(listed(&project).filter(|entry| search.keeps(entry))),
            [2, 5]
        );
    }

    #[test]
    fn both_narrowings_at_once_keep_the_tickets_both_keep() {
        let project = project();
        let both = Narrowing {
            kept_by: Some(ItemId(9)),
            touches: Some(area("api:search")),
        };
        assert_eq!(ids(listed(&project).filter(|entry| both.keeps(entry))), [5]);
    }
}
