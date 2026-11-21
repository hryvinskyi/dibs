//! Inbox's tabs and their rows: the conversations from people; the grant requests waiting for the
//! viewer's decision; and dibs's notices, the conversations that hold only what dibs wrote, one row
//! per item they are about, then the notices about no item. Each tab counts what waits on it, and
//! knows which conversations are its own.

use std::collections::{BTreeMap, BTreeSet};

use time::OffsetDateTime;

use dibs_app::named::NamedSummary;
use dibs_app::use_cases::PendingGrantAsk;
use dibs_domain::ids::{ItemId, ThreadId};

use crate::ui::inbox::query::ListQuery;
use crate::ui::inbox::summary::{ConversationSummary, from_people, notice, unread_from_people};
use crate::ui::links::{AreaView, LinkContext, PersonView, TicketView};
use crate::ui::matching::matches;
use crate::ui::moments::{Moment, moment};

/// A tab of the list: the name its address gives it, what it counts, which conversations are its
/// own, and its rows.
#[derive(Debug)]
pub(crate) struct Tab {
    /// Its name in the address and in the page.
    pub(crate) code: &'static str,
    /// Whether its rows name the items dibs's notices are about, by their titles.
    pub(crate) names_items: bool,
    count: fn(&Mailbox<'_>) -> usize,
    owns: fn(&Mailbox<'_>, &NamedSummary) -> bool,
    rows: fn(&Mailbox<'_>, &ListQuery, Option<ThreadId>) -> Body,
}

/// The tabs, in the order the page offers them; the first is the default.
pub(crate) static TABS: [Tab; 3] = [
    Tab {
        code: "people",
        names_items: false,
        count: |mail| unread_from_people(mail.threads, mail.waiting),
        owns: |mail, summary| from_people(summary, mail.waiting),
        rows: people,
    },
    Tab {
        code: "grants",
        names_items: false,
        count: |mail| mail.grants.len(),
        owns: |mail, summary| mail.waiting.contains(&summary.thread.record.id),
        rows: grants,
    },
    Tab {
        code: "dibs",
        names_items: true,
        count: |mail| {
            mail.threads
                .iter()
                .filter(|summary| summary.unread_from_dibs > 0 && notice(summary, mail.waiting))
                .count()
        },
        owns: |mail, summary| notice(summary, mail.waiting),
        rows: notices,
    },
];

impl Tab {
    /// Whether it is the default tab, which the address leaves out.
    pub(crate) fn is_default(&self) -> bool {
        std::ptr::eq(self, &TABS[0])
    }

    /// The tab a conversation belongs to: grant requests waiting for the viewer's decision to
    /// Grants, the conversations from people to People, every other one to From dibs.
    pub(crate) fn of(mail: &Mailbox<'_>, summary: &NamedSummary) -> &'static Self {
        TABS.iter()
            .find(|tab| (tab.owns)(mail, summary))
            .unwrap_or(&TABS[0])
    }
}

/// An item dibs's notices are about, as a row of From dibs names it.
#[derive(Debug, Clone)]
pub(crate) struct ItemFacts {
    /// Its title, when the project still has it.
    pub(crate) title: Option<String>,
    /// Its ticket, whose name leads to its claim, its place in the queue, or else its tracker.
    pub(crate) ticket: TicketView,
    /// The name of its claim's state mark, while a live or stale claim holds it.
    pub(crate) mark: Option<&'static str>,
}

/// What the tabs are built from.
pub(crate) struct Mailbox<'a> {
    /// The conversations the viewer may read, newest activity first.
    pub(crate) threads: &'a [NamedSummary],
    /// The same, as a list shows them.
    pub(crate) summaries: &'a [ConversationSummary],
    /// The conversations of the grant requests waiting for the viewer's decision.
    pub(crate) waiting: &'a BTreeSet<ThreadId>,
    /// Those grant requests, newest first.
    pub(crate) grants: &'a [PendingGrantAsk],
    /// The items dibs's notices are about, when the tab shown names them.
    pub(crate) items: &'a BTreeMap<ItemId, ItemFacts>,
    /// How the references link.
    pub(crate) links: &'a LinkContext,
    /// The time the page is rendered at.
    pub(crate) now: OffsetDateTime,
}

/// The rows of the tab shown.
#[derive(Debug)]
pub(crate) enum Body {
    /// The messages from people.
    People(Vec<TalkRow>),
    /// The grant requests.
    Grants(Vec<GrantRow>),
    /// dibs's notices: one row per item they are about, then those about no item.
    Notices(Vec<FoldRow>, Vec<TalkRow>),
}

impl Body {
    /// Whether it has no row.
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::People(rows) => rows.is_empty(),
            Self::Grants(rows) => rows.is_empty(),
            Self::Notices(folded, loose) => folded.is_empty() && loose.is_empty(),
        }
    }
}

/// A conversation in a list.
#[derive(Debug)]
pub(crate) struct TalkRow {
    /// The conversation.
    pub(crate) summary: ConversationSummary,
    /// The address of the conversation in the inspector, with the list's state.
    pub(crate) href: String,
    /// Whether it is the conversation in the inspector.
    pub(crate) current: bool,
    /// When its last message was written.
    pub(crate) last: Moment,
}

/// A grant request waiting for the viewer's decision.
#[derive(Debug)]
pub(crate) struct GrantRow {
    /// Who asks.
    pub(crate) asker: PersonView,
    /// The area asked for.
    pub(crate) area: AreaView,
    /// The asker's ticket.
    pub(crate) ticket: TicketView,
    /// The ticket of the viewer's claim that holds the area.
    pub(crate) holder: TicketView,
    /// The subject of the request's conversation, when the list holds it.
    pub(crate) subject: Option<String>,
    /// The address of that conversation in the inspector, with the list's state.
    pub(crate) href: String,
    /// Whether it is the conversation in the inspector.
    pub(crate) current: bool,
    /// When it was asked.
    pub(crate) asked: Moment,
}

/// dibs's notices about one item.
#[derive(Debug)]
pub(crate) struct FoldRow {
    /// The item.
    pub(crate) item: ItemFacts,
    /// How many messages dibs wrote in its notices.
    pub(crate) count: u64,
    /// How many of those the viewer has not read.
    pub(crate) unread: u64,
    /// How many of their messages wait for the viewer's acknowledgement.
    pub(crate) awaiting_ack: u64,
    /// The newest notice.
    pub(crate) newest: ConversationSummary,
    /// The address of the newest notice in the inspector, with the list's state.
    pub(crate) href: String,
    /// Whether one of its notices is the conversation in the inspector.
    pub(crate) current: bool,
    /// When the newest notice's last message was written.
    pub(crate) last: Moment,
}

/// One tab as the list offers it.
#[derive(Debug)]
pub(crate) struct TabEntry {
    /// Its name.
    pub(crate) code: &'static str,
    /// How many conversations wait on it.
    pub(crate) count: usize,
    /// Whether it is the tab shown.
    pub(crate) current: bool,
    /// The address of the list on it.
    pub(crate) href: String,
}

impl Mailbox<'_> {
    /// Every tab, counted, with `shown` current; each address keeps the text of `query`.
    pub(crate) fn tabs(&self, query: &ListQuery, shown: &'static Tab) -> Vec<TabEntry> {
        TABS.iter()
            .map(|tab| TabEntry {
                code: tab.code,
                count: (tab.count)(self),
                current: std::ptr::eq(tab, shown),
                href: query.showing(tab).address(None),
            })
            .collect()
    }

    /// The rows of `tab`, with the conversation `selected` current; `query` is the list's state.
    pub(crate) fn rows(
        &self,
        tab: &'static Tab,
        query: &ListQuery,
        selected: Option<ThreadId>,
    ) -> Body {
        (tab.rows)(self, query, selected)
    }

    /// The conversations, each as a list shows it, newest activity first.
    fn each(&self) -> impl Iterator<Item = (&NamedSummary, &ConversationSummary)> {
        self.threads.iter().zip(self.summaries)
    }

    /// The row of a conversation.
    fn talk(
        &self,
        summary: &ConversationSummary,
        query: &ListQuery,
        selected: Option<ThreadId>,
    ) -> TalkRow {
        TalkRow {
            href: query.address(Some(summary.id)),
            current: selected == Some(summary.id),
            last: moment(summary.last_at, self.now),
            summary: summary.clone(),
        }
    }
}

/// The conversations from people, read or not, that match the text.
fn people(mail: &Mailbox<'_>, query: &ListQuery, selected: Option<ThreadId>) -> Body {
    Body::People(
        mail.each()
            .filter(|(named, summary)| {
                from_people(named, mail.waiting) && matches(&query.q, &summary.text())
            })
            .map(|(_, summary)| mail.talk(summary, query, selected))
            .collect(),
    )
}

/// The grant requests that match the text: by the asker, the area, either ticket or the
/// conversation's subject.
fn grants(mail: &Mailbox<'_>, query: &ListQuery, selected: Option<ThreadId>) -> Body {
    Body::Grants(
        mail.grants
            .iter()
            .filter_map(|ask| {
                let conversation = mail
                    .summaries
                    .iter()
                    .find(|summary| summary.id == ask.thread);
                let subject: Option<String> = conversation.map(|summary| {
                    summary
                        .subject
                        .subject
                        .pieces
                        .iter()
                        .map(|piece| piece.text.as_str())
                        .collect()
                });
                let ticket =
                    TicketView::of(ask.item.id, ask.item.external_ref.as_ref(), mail.links);
                let holder = TicketView::of(
                    ask.holder_item.id,
                    ask.holder_item.external_ref.as_ref(),
                    mail.links,
                );
                let text = [
                    ask.asker.as_str(),
                    &ask.area.to_string(),
                    &ticket.title,
                    &holder.title,
                    subject.as_deref().unwrap_or_default(),
                ]
                .join(" ");
                matches(&query.q, &text).then(|| GrantRow {
                    asker: PersonView::of(&ask.asker),
                    area: AreaView::of(&ask.area),
                    ticket,
                    holder,
                    subject,
                    href: query.address(Some(ask.thread)),
                    current: selected == Some(ask.thread),
                    asked: moment(ask.asked_at, mail.now),
                })
            })
            .collect(),
    )
}

/// dibs's notices that match the text: those about an item folded into one row per item, newest
/// first, counting the messages dibs wrote in them, then those about no item.
fn notices(mail: &Mailbox<'_>, query: &ListQuery, selected: Option<ThreadId>) -> Body {
    let mut folded: Vec<(ItemId, FoldRow)> = Vec::new();
    let mut loose = Vec::new();
    for (named, summary) in mail.each() {
        if !notice(named, mail.waiting) || !matches(&query.q, &summary.text()) {
            continue;
        }
        let current = selected == Some(summary.id);
        let Some(item) = named.thread.record.item else {
            loose.push(mail.talk(summary, query, selected));
            continue;
        };
        if let Some((_, row)) = folded.iter_mut().find(|(id, _)| *id == item) {
            row.count += named.from_dibs;
            row.unread += named.unread_from_dibs;
            row.awaiting_ack += summary.awaiting_ack;
            row.current |= current;
            continue;
        }
        let facts = mail.items.get(&item).cloned().unwrap_or_else(|| ItemFacts {
            title: None,
            ticket: TicketView::of_item(item, mail.links),
            mark: None,
        });
        folded.push((
            item,
            FoldRow {
                item: facts,
                count: named.from_dibs,
                unread: named.unread_from_dibs,
                awaiting_ack: summary.awaiting_ack,
                newest: summary.clone(),
                href: query.address(Some(summary.id)),
                current,
                last: moment(summary.last_at, mail.now),
            },
        ));
    }
    Body::Notices(folded.into_iter().map(|(_, row)| row).collect(), loose)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use time::macros::datetime;

    use dibs_app::named::{AuthorName, NamedThread};
    use dibs_app::records::ThreadRecord;
    use dibs_domain::forge::ForgeRegistry;
    use dibs_domain::handle::Handle;
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::ids::MemberId;
    use dibs_domain::message::{Author, Importance, Recipients, Subject};
    use dibs_domain::ref_scheme::RefSchemeRegistry;
    use dibs_domain::settings::{ProjectSettings, SettingsDocument};

    use super::*;

    fn links() -> LinkContext {
        let document: SettingsDocument = serde_json::from_value(serde_json::json!({
            "repos": [{"id": "api", "remote": "https://github.com/acme/widgets.git",
                       "areas": [{"name": "billing", "paths": ["billing/**"]}]}]
        }))
        .unwrap();
        LinkContext::new(
            ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap(),
            Arc::new(RefSchemeRegistry::standard()),
            Arc::new(ForgeRegistry::standard()),
            Vec::new(),
        )
    }

    /// Conversation `id` about `item`, started by bob, or by dibs when `by_dibs`, holding one
    /// message by whoever started it.
    fn thread(id: u64, by_dibs: bool, item: Option<u64>, unread: u64) -> NamedSummary {
        let (author, named) = if by_dibs {
            (Author::System, AuthorName::Dibs)
        } else {
            (
                Author::Member(MemberId(2)),
                AuthorName::Member(Handle::parse("bob").unwrap()),
            )
        };
        NamedSummary {
            thread: NamedThread {
                record: ThreadRecord {
                    id: ThreadId(id),
                    subject: Subject::parse(&format!("Notice {id}")).unwrap(),
                    item: item.map(ItemId),
                    author,
                    recipients: Recipients::Project,
                    created_at: datetime!(2026-03-02 09:00 UTC),
                },
                author: named,
                recipients: Vec::new(),
            },
            messages: 1,
            unread,
            unread_from_people: if by_dibs { 0 } else { unread },
            unread_from_dibs: if by_dibs { unread } else { 0 },
            from_people: u64::from(!by_dibs),
            from_dibs: u64::from(by_dibs),
            last_from: (!by_dibs).then(|| Handle::parse("bob").unwrap()),
            last_unread_from: (!by_dibs && unread > 0).then(|| Handle::parse("bob").unwrap()),
            last_message_at: None,
            importance: Importance::Normal,
            awaiting_ack: 0,
        }
    }

    /// `summary` with `more` messages dibs wrote, `unread` of them unread.
    fn more_notices(mut summary: NamedSummary, more: u64, unread: u64) -> NamedSummary {
        summary.messages += more;
        summary.from_dibs += more;
        summary.unread += unread;
        summary.unread_from_dibs += unread;
        summary
    }

    fn with<T>(
        threads: &[NamedSummary],
        waiting: &[u64],
        check: impl FnOnce(&Mailbox<'_>) -> T,
    ) -> T {
        let links = links();
        let summaries: Vec<ConversationSummary> = threads
            .iter()
            .map(|summary| ConversationSummary::of(summary, &links))
            .collect();
        let waiting: BTreeSet<ThreadId> = waiting.iter().copied().map(ThreadId).collect();
        let items = BTreeMap::new();
        let mail = Mailbox {
            threads,
            summaries: &summaries,
            waiting: &waiting,
            grants: &[],
            items: &items,
            links: &links,
            now: datetime!(2026-03-02 10:00 UTC),
        };
        check(&mail)
    }

    #[test]
    fn a_conversation_belongs_to_grants_while_it_waits_to_people_when_a_member_wrote_in_it_else_to_from_dibs()
     {
        let mut reply = thread(4, true, None, 1);
        reply.from_people = 1;
        reply.unread_from_people = 1;
        reply.unread_from_dibs = 0;
        let mut in_bobs_name = thread(5, false, None, 0);
        in_bobs_name.from_people = 0;
        in_bobs_name.from_dibs = 1;
        let mut waiting_ack = thread(6, true, None, 0);
        waiting_ack.awaiting_ack = 1;
        let threads = [
            thread(1, false, None, 1),
            thread(2, false, None, 1),
            thread(3, true, None, 1),
            reply,
            in_bobs_name,
            waiting_ack,
        ];
        let tabs: Vec<&str> = with(&threads, &[2], |mail| {
            threads
                .iter()
                .map(|summary| Tab::of(mail, summary).code)
                .collect()
        });
        assert_eq!(
            tabs,
            ["people", "grants", "dibs", "people", "dibs", "people"],
            "a member's reply makes dibs's conversation people's; what dibs wrote in a member's name does not; a message waiting for an acknowledgement does"
        );
    }

    #[test]
    fn dibss_notices_fold_into_one_row_per_item_newest_first_then_those_about_no_item() {
        let threads = [
            thread(5, true, Some(7), 0),
            thread(4, true, Some(8), 1),
            thread(3, true, Some(7), 1),
            thread(2, false, Some(7), 1),
            thread(1, true, None, 1),
        ];
        let body = with(&threads, &[], |mail| {
            let shown = ListQuery::default().showing(&TABS[2]);
            mail.rows(&TABS[2], &shown, Some(ThreadId(3)))
        });
        let Body::Notices(folded, loose) = body else {
            panic!("{body:?}");
        };
        let rows: Vec<(String, u64, u64, bool)> = folded
            .iter()
            .map(|row| {
                (
                    row.item.ticket.label.clone(),
                    row.count,
                    row.unread,
                    row.current,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("#7".to_owned(), 2, 1, true),
                ("#8".to_owned(), 1, 1, false)
            ]
        );
        assert_eq!(folded[0].newest.id, ThreadId(5));
        assert_eq!(folded[0].href, "/ui/inbox/5?tab=dibs");
        let loose: Vec<ThreadId> = loose.iter().map(|row| row.summary.id).collect();
        assert_eq!(loose, [ThreadId(1)]);
    }

    #[test]
    fn a_folded_row_counts_the_messages_dibs_wrote_and_how_many_are_unread() {
        let threads = [
            more_notices(thread(5, true, Some(7), 0), 3, 2),
            thread(4, true, Some(8), 1),
            more_notices(thread(3, true, Some(7), 1), 1, 0),
        ];
        let body = with(&threads, &[], |mail| {
            let shown = ListQuery::default().showing(&TABS[2]);
            mail.rows(&TABS[2], &shown, None)
        });
        let Body::Notices(folded, _) = body else {
            panic!("{body:?}");
        };
        let counts: Vec<(u64, u64)> = folded.iter().map(|row| (row.count, row.unread)).collect();
        assert_eq!(counts, [(6, 3), (1, 1)]);
    }

    #[test]
    fn the_tabs_count_unread_people_waiting_grants_and_unread_notices() {
        let read_reply = more_notices(thread(5, false, None, 0), 1, 1);
        let threads = [
            thread(1, false, None, 1),
            thread(2, false, None, 1),
            thread(3, true, None, 1),
            thread(4, true, None, 0),
            read_reply,
        ];
        let counts: Vec<usize> = with(&threads, &[2], |mail| {
            mail.tabs(&ListQuery::default(), &TABS[0])
                .iter()
                .map(|tab| tab.count)
                .collect()
        });
        assert_eq!(
            counts,
            [1, 0, 1],
            "no grant request is listed, only marked as waiting"
        );
    }
}
