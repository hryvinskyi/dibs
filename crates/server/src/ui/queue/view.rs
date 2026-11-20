//! The view of one ticket of the queue — its title, priority, areas, whether it is ready or why it
//! waits, and the command that claims it — as a list row and an inspector print it, built from the
//! status the frame read.
//!
//! Which tickets the queue holds is decided by [`listed`]; this builds the view of each.

use std::collections::BTreeSet;

use dibs_app::named::ItemName;
use dibs_app::records::ItemRecord;
use dibs_app::use_cases::{NamedNotReady, StatusView};
use dibs_domain::ids::ItemId;
use dibs_domain::item::{ItemKey, ItemRepository};

use crate::ui::claims::classify::finished;
use crate::ui::links::{AreaView, LinkContext, LinkedPiece, LinkedText, PersonView, TicketView};
use crate::ui::queue::listing::{Listed, Standing, listed};

/// The bytes a word of a command keeps unquoted: a POSIX shell gives none of them a meaning inside
/// a word.
const SHELL_SAFE: &[u8] = b"_-.:/#@+=,%";

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

/// The command that claims a ticket: what the Copy button copies, and the same text as the page
/// prints it, the ticket and each area a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    /// The whole command, as the Copy button copies it.
    pub(crate) text: String,
    /// The same text in pieces, the ticket and each area linked; together they read `text` exactly.
    pub(crate) shown: LinkedText,
}

/// The queue the status shows, in the order [`listed`] gives.
pub(crate) fn queue_items(status: &StatusView, links: &LinkContext) -> Vec<QueueItemView> {
    let done: BTreeSet<ItemId> = status
        .live
        .iter()
        .chain(&status.stale)
        .filter(|held| finished(&held.item.item))
        .map(|held| held.item.item.id)
        .collect();
    listed(status)
        .map(|entry| QueueItemView::of(&entry, &done, links))
        .collect()
}

impl QueueItemView {
    /// The ticket of an item the queue lists. `done` holds the claimed items whose tickets are
    /// finished.
    pub(crate) fn of(entry: &Listed<'_>, done: &BTreeSet<ItemId>, links: &LinkContext) -> Self {
        let readiness = match entry.standing {
            Standing::Ready { stale_holder } => Readiness::Ready {
                was_held_by: stale_holder.map(PersonView::of),
            },
            Standing::NotReady { reasons } => Readiness::NotReady {
                reasons: reasons
                    .iter()
                    .filter_map(|reason| ReasonView::of(reason, done, links))
                    .collect(),
            },
        };
        Self::build(entry.item, readiness, links)
    }

    fn build(item: &ItemRecord, readiness: Readiness, links: &LinkContext) -> Self {
        let home = match &item.repository {
            ItemRepository::Listed(repo) => Some(repo),
            ItemRepository::Unlisted(_) | ItemRepository::Unnamed => None,
        };
        let ticket = TicketView::of(item.id, item.external_ref.as_ref(), links);
        let areas: Vec<AreaView> = item.areas_hint.iter().map(AreaView::of).collect();
        let key = item
            .external_ref
            .clone()
            .map_or(ItemKey::Local(item.id), ItemKey::External);
        Self {
            id: item.id,
            command: Command::of(&key, &ticket, &areas),
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
    /// The command that claims the item named `key`, whose ticket is `ticket`, with the areas
    /// `areas`: `dibs claim` and the key — the full reference, or the bare id when there is none —
    /// then `--area repo:name` for each area, every argument quoted for a shell when it needs to
    /// be. The key leads where the ticket's name leads, and each area to its page.
    pub(crate) fn of(key: &ItemKey, ticket: &TicketView, areas: &[AreaView]) -> Self {
        let mut pieces = vec![
            LinkedPiece {
                text: "dibs claim ".to_owned(),
                link: None,
            },
            LinkedPiece {
                text: shell_word(&key.to_string()),
                link: Some(ticket.main.clone()),
            },
        ];
        for area in areas {
            pieces.push(LinkedPiece {
                text: " --area ".to_owned(),
                link: None,
            });
            pieces.push(LinkedPiece {
                text: shell_word(&area.full),
                link: Some(area.link()),
            });
        }
        Self {
            text: pieces.iter().map(|piece| piece.text.as_str()).collect(),
            shown: LinkedText { pieces },
        }
    }
}

/// `word` as one word of a POSIX shell command line that reads back as `word` and nothing else:
/// unchanged when it is made only of ASCII letters, digits and `_-.:/#@+=,%` and does not start
/// with `#`, which would begin a comment; else in single quotes, each quote inside written `'\''`.
/// The empty word is `''`.
fn shell_word(word: &str) -> String {
    let safe = !word.is_empty()
        && !word.starts_with('#')
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || SHELL_SAFE.contains(&byte));
    if safe {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dibs_app::named::NamedItem;
    use dibs_app::use_cases::{ExcludedEntry, ReadyEntry, ReadyView};
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

    /// The ready entry of `item`, its claim gone silent when `stale` names the holder.
    fn ready(item: ItemRecord, stale: Option<&str>) -> ReadyEntry {
        ReadyEntry {
            item: named(item),
            link: None,
            stale_holder: stale.map(handle),
        }
    }

    fn waiting(item: ItemRecord, reasons: Vec<NamedNotReady>) -> ExcludedEntry {
        ExcludedEntry {
            item: named(item),
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

    /// The text a command's pieces read together.
    fn read(command: &Command) -> String {
        command
            .shown
            .pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect()
    }

    #[test]
    fn a_safe_word_is_left_as_it_is() {
        for word in [
            "gh:acme/widgets#42",
            "api:billing",
            "7",
            "jira:ACME-12",
            "x:a_b-c.d@e+f=g,h%i",
        ] {
            assert_eq!(shell_word(word), word);
        }
    }

    #[test]
    fn a_word_a_shell_would_read_otherwise_is_single_quoted() {
        for (word, quoted) in [
            ("x:$(touch pwned)", "'x:$(touch pwned)'"),
            ("x:`touch pwned`", "'x:`touch pwned`'"),
            ("x:a;touch pwned", "'x:a;touch pwned'"),
            ("x:a|sh", "'x:a|sh'"),
            ("x:a b", "'x:a b'"),
            ("x:$HOME", "'x:$HOME'"),
            ("x:a&b", "'x:a&b'"),
            ("x:a>b", "'x:a>b'"),
            ("x:a*", "'x:a*'"),
            ("x:~a", "'x:~a'"),
            ("x:é", "'x:é'"),
            ("#7", "'#7'"),
            ("", "''"),
        ] {
            assert_eq!(shell_word(word), quoted, "{word}");
        }
    }

    #[test]
    fn a_single_quote_inside_a_word_closes_the_quotes_escapes_itself_and_reopens_them() {
        assert_eq!(shell_word("x:it's"), r"'x:it'\''s'");
        assert_eq!(shell_word("'"), r"''\'''");
        assert_eq!(
            shell_word("x:'; touch pwned; '"),
            r"'x:'\''; touch pwned; '\'''"
        );
    }

    #[test]
    fn the_command_is_the_full_reference_then_an_area_each_and_reads_as_it_is_copied() {
        let none = context(Vec::new());
        let gh = external("gh:acme/widgets#42");
        let ticket = TicketView::of(ItemId(7), Some(&gh), &none);
        let key = ItemKey::External(gh);
        let areas = [
            AreaView::of(&area("api:billing")),
            AreaView::of(&area("web:search")),
        ];
        let with = Command::of(&key, &ticket, &areas);
        assert_eq!(
            with.text,
            "dibs claim gh:acme/widgets#42 --area api:billing --area web:search"
        );
        assert_eq!(read(&with), with.text);
        let links: Vec<(&str, Option<&str>)> = with
            .shown
            .pieces
            .iter()
            .map(|piece| {
                (
                    piece.text.as_str(),
                    piece.link.as_ref().map(|link| link.href.as_str()),
                )
            })
            .collect();
        assert_eq!(
            links,
            [
                ("dibs claim ", None),
                ("gh:acme/widgets#42", Some(ticket.main.href.as_str())),
                (" --area ", None),
                ("api:billing", Some("/ui/areas/api:billing")),
                (" --area ", None),
                ("web:search", Some("/ui/areas/web:search")),
            ]
        );
        assert_eq!(
            Command::of(&key, &ticket, &[]).text,
            "dibs claim gh:acme/widgets#42"
        );
    }

    #[test]
    fn a_ticket_without_a_reference_is_claimed_by_its_bare_id() {
        let none = context(Vec::new());
        let ticket = TicketView::of(ItemId(7), None, &none);
        let key = ItemKey::Local(ItemId(7));
        let areas = [AreaView::of(&area("api:billing"))];
        let with = Command::of(&key, &ticket, &areas);
        assert_eq!(with.text, "dibs claim 7 --area api:billing");
        assert_eq!(read(&with), with.text);
        assert_eq!(Command::of(&key, &ticket, &[]).text, "dibs claim 7");
        assert_eq!(ticket.label, "#7", "the ticket is still shown as #7");
    }

    #[test]
    fn a_reference_a_shell_would_run_is_quoted_in_the_command_and_in_what_the_page_prints() {
        let none = context(Vec::new());
        let hostile = external("x:$(touch pwned)");
        let ticket = TicketView::of(ItemId(7), Some(&hostile), &none);
        let with = Command::of(&ItemKey::External(hostile), &ticket, &[]);
        assert_eq!(with.text, "dibs claim 'x:$(touch pwned)'");
        assert_eq!(read(&with), with.text);
    }

    #[test]
    fn a_ready_ticket_remembers_the_member_whose_claim_went_silent() {
        let none = context(Vec::new());
        let mut item = record(7, Some("gh:acme/widgets#7"), "Rates", &["api:billing"]);
        item.priority = Priority::new(2).unwrap();
        let project = status(vec![ready(item, Some("carol"))], Vec::new());
        let [view] = &queue_items(&project, &none)[..] else {
            panic!("one ticket is listed");
        };
        assert!(view.is_ready());
        assert_eq!(view.priority, 2);
        assert_eq!(
            view.command.text,
            "dibs claim gh:acme/widgets#7 --area api:billing"
        );
        assert!(matches!(
            &view.readiness,
            Readiness::Ready { was_held_by: Some(person) } if person.handle == "carol"
        ));
    }

    #[test]
    fn a_waiting_ticket_names_each_reason_in_the_status_order() {
        let none = context(Vec::new());
        let held = NamedNotReady::AreaHeld {
            area: area("api:billing"),
            holder: handle("bob"),
            item: name(9, Some("gh:acme/widgets#9")),
        };
        let blocked = NamedNotReady::BlockedBy {
            items: vec![name(3, Some("gh:acme/widgets#3"))],
        };
        let item = record(7, Some("gh:acme/widgets#7"), "Rates", &["api:billing"]);
        let project = status(Vec::new(), vec![waiting(item, vec![blocked, held])]);
        let [view] = &queue_items(&project, &none)[..] else {
            panic!("one ticket is listed");
        };
        assert!(!view.is_ready());
        let Readiness::NotReady { reasons } = &view.readiness else {
            panic!("an excluded ticket is not ready");
        };
        assert!(matches!(
            reasons[..],
            [ReasonView::BlockedBy { .. }, ReasonView::AreaHeld { .. }]
        ));
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
        let waits = status(Vec::new(), vec![waiting(item.clone(), vec![blocked, held])]);
        let text = queue_items(&waits, &none)[0].text();
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
        let silent = status(vec![ready(item, Some("carol"))], Vec::new());
        let text = queue_items(&silent, &none)[0].text();
        assert!(!text.contains("carol"), "{text}");
    }

    #[test]
    fn the_queue_lists_ready_tickets_then_waiting_ones_and_leaves_out_the_claimed() {
        let known = context(Vec::new());
        let project = status(
            vec![ready(
                record(1, Some("gh:acme/widgets#1"), "First", &[]),
                None,
            )],
            vec![
                waiting(
                    record(2, Some("gh:acme/widgets#2"), "Second", &[]),
                    vec![NamedNotReady::Claimed {
                        holder: handle("bob"),
                    }],
                ),
                waiting(
                    record(3, Some("gh:acme/widgets#3"), "Third", &[]),
                    vec![NamedNotReady::BlockedBy {
                        items: vec![name(1, Some("gh:acme/widgets#1"))],
                    }],
                ),
            ],
        );
        let ids: Vec<ItemId> = queue_items(&project, &known)
            .iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(ids, [ItemId(1), ItemId(3)]);
    }
}
