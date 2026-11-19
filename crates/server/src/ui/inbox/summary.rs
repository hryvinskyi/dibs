//! One conversation as a list shows it — its subject, who started it and for whom, the ticket it is
//! about, how many messages it holds and how many the viewer has not read, how pressing it is and
//! when its last message was written — and which conversations are messages from people.
//!
//! A message from people is a conversation a member started, the viewer included, unless it is a
//! grant request still waiting for the viewer's decision: the viewer's agent decides that one, so it
//! never raises the count of what a person has to read. What dibs started is a notice.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use dibs_app::named::{AuthorName, NamedSummary};
use dibs_app::use_cases::InboxView;
use dibs_domain::ids::ThreadId;
use dibs_domain::message::{Importance, Recipients};

use crate::ui::links::{ConversationView, LinkContext, PersonView, TicketView};

/// One conversation in a list.
#[derive(Debug, Clone)]
pub(crate) struct ConversationSummary {
    /// The thread's id.
    pub(crate) id: ThreadId,
    /// Its subject and address.
    pub(crate) subject: ConversationView,
    /// The member who started it; `None` when dibs did.
    pub(crate) author: Option<PersonView>,
    /// The members it is addressed to; `None` when it is addressed to everyone.
    pub(crate) recipients: Option<Vec<PersonView>>,
    /// The ticket of the item it is about, when it is about one.
    pub(crate) ticket: Option<TicketView>,
    /// How many messages it holds.
    pub(crate) messages: u64,
    /// How many of them the viewer has not read; their own never count.
    pub(crate) unread: u64,
    /// The highest importance among its messages.
    pub(crate) importance: Importance,
    /// When its last message was written, or when it started.
    pub(crate) last_at: OffsetDateTime,
}

impl ConversationSummary {
    /// The view of the conversation `summary`, its references linked by `links`.
    pub(crate) fn of(summary: &NamedSummary, links: &LinkContext) -> Self {
        let thread = &summary.thread;
        let record = &thread.record;
        Self {
            id: record.id,
            subject: ConversationView::of(record.id, record.subject.as_str(), links),
            author: thread.author.handle().map(PersonView::of),
            recipients: match record.recipients {
                Recipients::Project => None,
                Recipients::Members(_) => {
                    Some(thread.recipients.iter().map(PersonView::of).collect())
                }
            },
            ticket: record.item.map(|item| TicketView::of_item(item, links)),
            messages: summary.messages,
            unread: summary.unread,
            importance: summary.importance,
            last_at: summary.last_message_at.unwrap_or(record.created_at),
        }
    }

    /// The text a list's filter matches against: the subject, the author, the recipients and the
    /// full reference of the ticket.
    pub(crate) fn text(&self) -> String {
        let subject: String = self
            .subject
            .subject
            .pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect();
        let author = self
            .author
            .as_ref()
            .map_or(AuthorName::DIBS, |person| person.handle.as_str());
        [subject.as_str(), author]
            .into_iter()
            .map(str::to_owned)
            .chain(
                self.recipients
                    .iter()
                    .flatten()
                    .map(|person| person.handle.clone()),
            )
            .chain(self.ticket.iter().map(|ticket| ticket.title.clone()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The conversations of the grant requests waiting for the viewer's decision.
pub(crate) fn waiting_grants(inbox: &InboxView) -> BTreeSet<ThreadId> {
    inbox.grant_asks.iter().map(|ask| ask.thread).collect()
}

/// Whether the conversation is a message from people: a member started it, the viewer included,
/// and it is not one of `waiting_grants`.
pub(crate) fn from_people(summary: &NamedSummary, waiting_grants: &BTreeSet<ThreadId>) -> bool {
    matches!(summary.thread.author, AuthorName::Member(_))
        && !waiting_grants.contains(&summary.thread.record.id)
}

/// How many of the conversations are messages from people with messages the viewer has not read.
pub(crate) fn unread_from_people<'a>(
    summaries: impl IntoIterator<Item = &'a NamedSummary>,
    waiting_grants: &BTreeSet<ThreadId>,
) -> usize {
    summaries
        .into_iter()
        .filter(|summary| summary.unread > 0 && from_people(summary, waiting_grants))
        .count()
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use dibs_app::named::NamedThread;
    use dibs_app::records::ThreadRecord;
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::MemberId;
    use dibs_domain::message::{Author, Subject};

    use super::*;

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    /// Conversation `id`, started by `author` (`None`: dibs), with `unread` messages unread.
    fn summary(id: u64, author: Option<(u64, &str)>, unread: u64) -> NamedSummary {
        let (record_author, named_author) = match author {
            Some((member, name)) => (
                Author::Member(MemberId(member)),
                AuthorName::Member(handle(name)),
            ),
            None => (Author::System, AuthorName::Dibs),
        };
        NamedSummary {
            thread: NamedThread {
                record: ThreadRecord {
                    id: ThreadId(id),
                    subject: Subject::parse("About billing").unwrap(),
                    item: None,
                    author: record_author,
                    recipients: Recipients::Project,
                    created_at: datetime!(2026-03-02 09:00 UTC),
                },
                author: named_author,
                recipients: Vec::new(),
            },
            messages: 2,
            unread,
            last_message_at: None,
            importance: Importance::Normal,
            awaiting_ack: 0,
        }
    }

    #[test]
    fn a_member_started_conversation_is_from_people_and_dibss_is_not() {
        let none = BTreeSet::new();
        assert!(from_people(&summary(1, Some((2, "bob")), 1), &none));
        assert!(
            from_people(&summary(2, Some((1, "alice")), 1), &none),
            "the viewer's own conversation counts once someone answers it"
        );
        assert!(!from_people(&summary(3, None, 1), &none));
    }

    #[test]
    fn a_grant_request_waiting_for_the_viewer_is_not_from_people_until_it_is_decided() {
        let request = summary(4, Some((2, "bob")), 1);
        let waiting = BTreeSet::from([ThreadId(4)]);
        assert!(!from_people(&request, &waiting));
        assert!(from_people(&request, &BTreeSet::new()), "once decided");
    }

    #[test]
    fn the_unread_count_counts_only_unread_conversations_from_people() {
        let waiting = BTreeSet::from([ThreadId(4)]);
        let all = [
            summary(1, Some((2, "bob")), 1),
            summary(2, Some((1, "alice")), 1),
            summary(3, None, 2),
            summary(4, Some((2, "bob")), 1),
            summary(5, Some((2, "bob")), 0),
        ];
        assert_eq!(unread_from_people(&all, &waiting), 2);
        assert_eq!(unread_from_people(&all, &BTreeSet::new()), 3);
    }
}
