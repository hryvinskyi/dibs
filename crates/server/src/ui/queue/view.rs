//! The view of one ticket of the queue — its title, priority, areas, whether it is ready or why it
//! waits, and the command that claims it — as a list row and an inspector print it, built from the
//! status the frame read.
//!
//! The queue is the ready list followed by the open items left out of it, except those a live claim
//! holds: those are claims, and Claims shows them.

use std::collections::BTreeSet;

use dibs_app::named::ItemName;
use dibs_app::records::ItemRecord;
use dibs_app::use_cases::{ExcludedEntry, NamedNotReady, ReadyEntry, StatusView};
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemRepository;

use crate::ui::claims::classify::finished;
use crate::ui::links::{AreaView, LinkContext, LinkedText, PersonView, TicketView};
use crate::ui::queue::listing::waits;

/// A ticket of the queue.
#[derive(Debug, Clone)]
pub(crate) struct QueueItemView {
    /// The item's id.
    pub(crate) id: ItemId,
    /// The item's ticket.
    pub(crate) ticket: TicketView,
    /// The item's title, with the references in it linked.
    pub(crate) title: LinkedText,
    /// How soon it should be done, 0 to 4; lower is sooner.
    pub(crate) priority: u8,
    /// The tracker's own status of the item.
    pub(crate) tracker_status: Option<String>,
    /// The areas the work will probably touch.
    pub(crate) areas: Vec<AreaView>,
    /// Whether it can be claimed now, or why it cannot.
    pub(crate) readiness: Readiness,
    /// The command that claims it.
    pub(crate) command: Command,
}

/// Whether a ticket can be claimed now.
#[derive(Debug, Clone)]
pub(crate) enum Readiness {
    /// It can.
    Ready {
        /// The member whose claim on it went silent; taking it is an explicit take-over.
        was_held_by: Option<PersonView>,
    },
    /// It cannot, for each of these reasons.
    NotReady {
        /// What holds it back, in the order the status gives.
        reasons: Vec<ReasonView>,
    },
}

/// One reason a ticket cannot be claimed now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReasonView {
    /// It waits for other tickets to be finished.
    BlockedBy {
        /// Those tickets.
        items: Vec<TicketView>,
    },
    /// An area of its hint is held by another member's claim.
    AreaHeld {
        /// The area.
        area: AreaView,
        /// The member holding it.
        holder: PersonView,
        /// The ticket of the holder's claim.
        item: Box<TicketView>,
        /// Whether that ticket is finished, so the claim holds the area for nothing.
        finished: bool,
    },
}

/// The command that claims a ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    /// The whole command, as the Copy button copies it.
    pub(crate) text: String,
    /// The ticket, printed as a link inside the command.
    pub(crate) reference: TicketView,
    /// Each `--area`, printed as a link inside the command.
    pub(crate) areas: Vec<AreaView>,
}

/// The queue the status shows: the ready tickets, then the open ones left out of the ready list for
/// another reason than a live claim, each group in the status's order.
pub(crate) fn queue_items(status: &StatusView, links: &LinkContext) -> Vec<QueueItemView> {
    let done: BTreeSet<ItemId> = status
        .live
        .iter()
        .chain(&status.stale)
        .filter(|held| finished(&held.item.item))
        .map(|held| held.item.item.id)
        .collect();
    let ready = status
        .ready
        .ready
        .iter()
        .map(|entry| QueueItemView::ready(entry, links));
    let waiting = status
        .ready
        .excluded
        .iter()
        .filter_map(|entry| QueueItemView::waiting(entry, &done, links));
    ready.chain(waiting).collect()
}

impl QueueItemView {
    /// The ticket of an item the caller can claim now.
    pub(crate) fn ready(entry: &ReadyEntry, links: &LinkContext) -> Self {
        let readiness = Readiness::Ready {
            was_held_by: entry.stale_holder.as_ref().map(PersonView::of),
        };
        Self::build(&entry.item.item, readiness, links)
    }

    /// The ticket of an open item left out of the ready list; none when the queue does not list it,
    /// because a live claim holds it. `done` holds the claimed items whose tickets are finished.
    pub(crate) fn waiting(
        entry: &ExcludedEntry,
        done: &BTreeSet<ItemId>,
        links: &LinkContext,
    ) -> Option<Self> {
        if !waits(entry) {
            return None;
        }
        let reasons = entry
            .reasons
            .iter()
            .filter_map(|reason| ReasonView::of(reason, done, links))
            .collect();
        Some(Self::build(
            &entry.item.item,
            Readiness::NotReady { reasons },
            links,
        ))
    }

    fn build(item: &ItemRecord, readiness: Readiness, links: &LinkContext) -> Self {
        let home = match &item.repository {
            ItemRepository::Listed(repo) => Some(repo),
            ItemRepository::Unlisted(_) | ItemRepository::Unnamed => None,
        };
        let ticket = TicketView::of(item.id, item.external_ref.as_ref(), links);
        let areas: Vec<AreaView> = item.areas_hint.iter().map(AreaView::of).collect();
        Self {
            id: item.id,
            command: Command::of(&ticket, &areas),
            ticket,
            title: LinkedText::of(item.title.as_str(), home, links),
            priority: item.priority.get(),
            tracker_status: item.source_status.as_ref().map(ToString::to_string),
            areas,
            readiness,
        }
    }

    /// Whether it can be claimed now.
    pub(crate) fn is_ready(&self) -> bool {
        matches!(self.readiness, Readiness::Ready { .. })
    }

    /// The text a list's filter matches against: the ticket's full reference, the title, the areas
    /// and, for a ticket that is not ready, the holders and the tickets its reasons name.
    pub(crate) fn text(&self) -> String {
        let title: String = self
            .title
            .pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect();
        let reasons = match &self.readiness {
            Readiness::Ready { .. } => Vec::new(),
            Readiness::NotReady { reasons } => reasons.iter().flat_map(ReasonView::words).collect(),
        };
        [self.ticket.title.as_str(), &title]
            .into_iter()
            .chain(self.areas.iter().map(|area| area.full.as_str()))
            .chain(reasons)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl ReasonView {
    /// The view of a reason a ticket is not ready; none for a live claim, which makes the ticket a
    /// claim rather than a waiting one. `done` holds the claimed items whose tickets are finished.
    pub(crate) fn of(
        reason: &NamedNotReady,
        done: &BTreeSet<ItemId>,
        links: &LinkContext,
    ) -> Option<Self> {
        match reason {
            NamedNotReady::Claimed { .. } => None,
            NamedNotReady::BlockedBy { items } => Some(Self::BlockedBy {
                items: items.iter().map(|item| ticket_of(item, links)).collect(),
            }),
            NamedNotReady::AreaHeld { area, holder, item } => Some(Self::AreaHeld {
                area: AreaView::of(area),
                holder: PersonView::of(holder),
                item: Box::new(ticket_of(item, links)),
                finished: done.contains(&item.id),
            }),
        }
    }

    /// The words of the reason a list's filter matches.
    fn words(&self) -> Vec<&str> {
        match self {
            Self::BlockedBy { items } => items.iter().map(|item| item.title.as_str()).collect(),
            Self::AreaHeld { holder, item, .. } => {
                vec![holder.handle.as_str(), item.title.as_str()]
            }
        }
    }
}

/// The ticket an item name stands for.
fn ticket_of(item: &ItemName, links: &LinkContext) -> TicketView {
    TicketView::of(item.id, item.external_ref.as_ref(), links)
}

impl Command {
    /// The command that claims the ticket `reference` with the areas `areas`: `dibs claim` and the
    /// ticket's full reference, or `#{id}` when it has none, then `--area repo:name` for each area.
    pub(crate) fn of(reference: &TicketView, areas: &[AreaView]) -> Self {
        let text = areas
            .iter()
            .fold(format!("dibs claim {}", reference.title), |text, area| {
                format!("{text} --area {}", area.full)
            });
        Self {
            text,
            reference: reference.clone(),
            areas: areas.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dibs_app::named::NamedItem;
    use dibs_domain::area::QualifiedArea;
    use dibs_domain::forge::ForgeRegistry;
    use dibs_domain::handle::Handle;
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::ids::MemberId;
    use dibs_domain::item::{ExternalRef, Priority, Title};
    use dibs_domain::ref_scheme::RefSchemeRegistry;
    use dibs_domain::repo::RepoId;
    use dibs_domain::settings::{ProjectSettings, SettingsDocument};
    use time::OffsetDateTime;

    use super::*;
    use crate::ui::links::Place;

    fn context(places: Vec<(ItemId, Option<ExternalRef>, Place)>) -> LinkContext {
        let document: SettingsDocument = serde_json::from_value(serde_json::json!({
            "repos": [{"id": "api", "remote": "https://github.com/acme/widgets.git",
                       "areas": [{"name": "billing", "paths": ["billing/**"]}]}]
        }))
        .unwrap();
        LinkContext::new(
            ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap(),
            Arc::new(RefSchemeRegistry::standard()),
            Arc::new(ForgeRegistry::standard()),
            places,
        )
    }

    fn external(text: &str) -> ExternalRef {
        ExternalRef::parse(text).unwrap()
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn name(id: u64, reference: Option<&str>) -> ItemName {
        ItemName {
            id: ItemId(id),
            external_ref: reference.map(external),
            title: Title::parse("Other work").unwrap(),
            link: None,
        }
    }

    fn record(id: u64, reference: Option<&str>, title: &str, areas: &[&str]) -> ItemRecord {
        let mut item = ItemRecord::new(
            ItemId(id),
            reference.map(external),
            Title::parse(title).unwrap(),
            ItemRepository::Listed(RepoId::parse("api").unwrap()),
            MemberId(1),
            OffsetDateTime::UNIX_EPOCH,
        );
        item.areas_hint = areas.iter().map(|text| area(text)).collect();
        item
    }

    fn named(item: ItemRecord) -> NamedItem {
        NamedItem {
            item,
            created_by: handle("alice"),
        }
    }

    #[test]
    fn a_live_claim_gives_no_reason_view() {
        let none = context(Vec::new());
        let claimed = NamedNotReady::Claimed {
            holder: handle("bob"),
        };
        assert_eq!(ReasonView::of(&claimed, &BTreeSet::new(), &none), None);
    }

    #[test]
    fn blockers_are_named_by_their_tickets() {
        let none = context(Vec::new());
        let blocked = NamedNotReady::BlockedBy {
            items: vec![name(3, Some("gh:acme/widgets#3")), name(4, None)],
        };
        let Some(ReasonView::BlockedBy { items }) =
            ReasonView::of(&blocked, &BTreeSet::new(), &none)
        else {
            panic!("a blocker is a reason");
        };
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["widgets#3", "#4"]);
    }

    #[test]
    fn a_held_area_names_its_holder_and_ticket_and_whether_that_ticket_is_finished() {
        let none = context(Vec::new());
        let held = NamedNotReady::AreaHeld {
            area: area("api:billing"),
            holder: handle("bob"),
            item: name(9, Some("gh:acme/widgets#9")),
        };
        let open = ReasonView::of(&held, &BTreeSet::new(), &none);
        let Some(ReasonView::AreaHeld {
            area,
            holder,
            item,
            finished,
        }) = open
        else {
            panic!("a held area is a reason");
        };
        assert_eq!(area.full, "api:billing");
        assert_eq!(holder.handle, "bob");
        assert_eq!(item.label, "widgets#9");
        assert!(!finished);
        let done = BTreeSet::from([ItemId(9)]);
        assert!(matches!(
            ReasonView::of(&held, &done, &none),
            Some(ReasonView::AreaHeld { finished: true, .. })
        ));
        let elsewhere = BTreeSet::from([ItemId(8)]);
        assert!(matches!(
            ReasonView::of(&held, &elsewhere, &none),
            Some(ReasonView::AreaHeld {
                finished: false,
                ..
            })
        ));
    }

    #[test]
    fn the_command_is_the_full_reference_then_an_area_each() {
        let none = context(Vec::new());
        let gh = external("gh:acme/widgets#42");
        let ticket = TicketView::of(ItemId(7), Some(&gh), &none);
        let areas = [
            AreaView::of(&area("api:billing")),
            AreaView::of(&area("web:search")),
        ];
        let with = Command::of(&ticket, &areas);
        assert_eq!(
            with.text,
            "dibs claim gh:acme/widgets#42 --area api:billing --area web:search"
        );
        assert_eq!(with.reference, ticket);
        assert_eq!(with.areas, areas);
        assert_eq!(
            Command::of(&ticket, &[]).text,
            "dibs claim gh:acme/widgets#42"
        );
    }

    #[test]
    fn a_ticket_without_a_reference_is_claimed_by_its_id() {
        let none = context(Vec::new());
        let ticket = TicketView::of(ItemId(7), None, &none);
        let areas = [AreaView::of(&area("api:billing"))];
        assert_eq!(
            Command::of(&ticket, &areas).text,
            "dibs claim #7 --area api:billing"
        );
        assert_eq!(Command::of(&ticket, &[]).text, "dibs claim #7");
    }

    #[test]
    fn a_ready_ticket_remembers_the_member_whose_claim_went_silent() {
        let none = context(Vec::new());
        let mut item = record(7, Some("gh:acme/widgets#7"), "Rates", &["api:billing"]);
        item.priority = Priority::new(2).unwrap();
        let entry = ReadyEntry {
            item: named(item),
            link: None,
            stale_holder: Some(handle("carol")),
        };
        let view = QueueItemView::ready(&entry, &none);
        assert!(view.is_ready());
        assert_eq!(view.priority, 2);
        assert_eq!(
            view.command.text,
            "dibs claim gh:acme/widgets#7 --area api:billing"
        );
        assert!(matches!(
            view.readiness,
            Readiness::Ready { was_held_by: Some(person) } if person.handle == "carol"
        ));
    }

    #[test]
    fn an_excluded_ticket_is_listed_unless_a_live_claim_holds_it() {
        let none = context(Vec::new());
        let entry = |reasons: Vec<NamedNotReady>| ExcludedEntry {
            item: named(record(
                7,
                Some("gh:acme/widgets#7"),
                "Rates",
                &["api:billing"],
            )),
            link: None,
            reasons,
        };
        let held = NamedNotReady::AreaHeld {
            area: area("api:billing"),
            holder: handle("bob"),
            item: name(9, Some("gh:acme/widgets#9")),
        };
        let claimed = NamedNotReady::Claimed {
            holder: handle("bob"),
        };
        let done = BTreeSet::new();
        let waiting = QueueItemView::waiting(&entry(vec![held.clone()]), &done, &none).unwrap();
        assert!(!waiting.is_ready());
        let Readiness::NotReady { reasons } = &waiting.readiness else {
            panic!("an excluded ticket is not ready");
        };
        assert_eq!(reasons.len(), 1);
        assert!(QueueItemView::waiting(&entry(vec![claimed.clone()]), &done, &none).is_none());
        assert!(QueueItemView::waiting(&entry(vec![claimed, held]), &done, &none).is_none());
    }

    #[test]
    fn the_filter_text_holds_the_reference_the_title_the_areas_and_what_a_waiting_ticket_names() {
        let none = context(Vec::new());
        let item = record(7, Some("gh:acme/widgets#7"), "Rates", &["api:billing"]);
        let held = NamedNotReady::AreaHeld {
            area: area("api:billing"),
            holder: handle("bob"),
            item: name(9, Some("gh:acme/widgets#9")),
        };
        let blocked = NamedNotReady::BlockedBy {
            items: vec![name(3, Some("gh:acme/widgets#3"))],
        };
        let waiting = QueueItemView::waiting(
            &ExcludedEntry {
                item: named(item.clone()),
                link: None,
                reasons: vec![blocked, held],
            },
            &BTreeSet::new(),
            &none,
        )
        .unwrap();
        let text = waiting.text();
        for word in [
            "gh:acme/widgets#7",
            "Rates",
            "api:billing",
            "bob",
            "gh:acme/widgets#9",
            "gh:acme/widgets#3",
        ] {
            assert!(text.contains(word), "{word} in {text}");
        }
        let ready = QueueItemView::ready(
            &ReadyEntry {
                item: named(item),
                link: None,
                stale_holder: Some(handle("carol")),
            },
            &none,
        );
        assert!(!ready.text().contains("carol"), "{}", ready.text());
    }

    #[test]
    fn the_queue_lists_ready_tickets_then_waiting_ones_and_leaves_out_the_claimed() {
        let known = context(Vec::new());
        let status = StatusView {
            ready: dibs_app::use_cases::ReadyView {
                ready: vec![ReadyEntry {
                    item: named(record(1, Some("gh:acme/widgets#1"), "First", &[])),
                    link: None,
                    stale_holder: None,
                }],
                excluded: vec![
                    ExcludedEntry {
                        item: named(record(2, Some("gh:acme/widgets#2"), "Second", &[])),
                        link: None,
                        reasons: vec![NamedNotReady::Claimed {
                            holder: handle("bob"),
                        }],
                    },
                    ExcludedEntry {
                        item: named(record(3, Some("gh:acme/widgets#3"), "Third", &[])),
                        link: None,
                        reasons: vec![NamedNotReady::BlockedBy {
                            items: vec![name(1, Some("gh:acme/widgets#1"))],
                        }],
                    },
                ],
            },
            live: Vec::new(),
            stale: Vec::new(),
            holds: Vec::new(),
            wide: Vec::new(),
            slots: Vec::new(),
            worktrees: Vec::new(),
            escalations: Vec::new(),
            integration_silent_since: None,
            overlaps: Vec::new(),
        };
        let ids: Vec<ItemId> = queue_items(&status, &known)
            .iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids, [ItemId(1), ItemId(3)]);
    }
}
