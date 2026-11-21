//! One conversation as a list shows it — its subject, who started it and for whom, the ticket it is
//! about, how many messages it holds, how many the viewer has not read and how many wait for their
//! acknowledgement, how pressing it is and when its last message was written — and which
//! conversations are messages from people.
//!
//! A conversation is from people when it holds a message a member wrote, the viewer included, or a
//! message waiting for the viewer's acknowledgement, unless it is a grant request still waiting for
//! the viewer's decision: the viewer's agent decides that one, so it never raises the count of what
//! a person has to read. Who started the conversation, and in whose name, does not matter: what
//! dibs wrote in a member's name is a notice all the same.
//!
//! A conversation from people waits on the viewer while it holds any message they have not read,
//! whoever wrote it: what dibs adds to it is read there, among people's, as no notice list shows
//! it.

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
    /// How many of them wait for the viewer's acknowledgement, read or not; their own never do.
    pub(crate) awaiting_ack: u64,
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
            awaiting_ack: summary.awaiting_ack,
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

/// The conversations of the grant requests waiting for the viewer's decision, every one of them
/// and not only those an agent is handed.
pub(crate) fn waiting_grants(inbox: &InboxView) -> BTreeSet<ThreadId> {
    inbox.every_grant_ask.iter().map(|ask| ask.thread).collect()
}

/// Whether the conversation is from people: it holds a message a member wrote, the viewer
/// included, or one waiting for the viewer's acknowledgement, and it is not one of
/// `waiting_grants`.
pub(crate) fn from_people(summary: &NamedSummary, waiting_grants: &BTreeSet<ThreadId>) -> bool {
    (summary.from_people > 0 || summary.awaiting_ack > 0)
        && !waiting_grants.contains(&summary.thread.record.id)
}

/// Whether the conversation is one of dibs's notices: neither from people nor one of
/// `waiting_grants`.
pub(crate) fn notice(summary: &NamedSummary, waiting_grants: &BTreeSet<ThreadId>) -> bool {
    !from_people(summary, waiting_grants) && !waiting_grants.contains(&summary.thread.record.id)
}

/// Whether the conversation has messages a member wrote that the viewer has not read, and is not
/// one of `waiting_grants`.
pub(crate) fn unread_people(summary: &NamedSummary, waiting_grants: &BTreeSet<ThreadId>) -> bool {
    summary.unread_from_people > 0 && from_people(summary, waiting_grants)
}

/// Whether the conversation is from people and holds a message the viewer has not read, a
/// member's or dibs's.
pub(crate) fn unread_among_people(
    summary: &NamedSummary,
    waiting_grants: &BTreeSet<ThreadId>,
) -> bool {
    summary.unread > 0 && from_people(summary, waiting_grants)
}

/// How many of the conversations from people hold a message the viewer has not read, a member's
/// or dibs's, leaving out `waiting_grants`.
pub(crate) fn count_unread_among_people<'a>(
    summaries: impl IntoIterator<Item = &'a NamedSummary>,
    waiting_grants: &BTreeSet<ThreadId>,
) -> usize {
    summaries
        .into_iter()
        .filter(|summary| unread_among_people(summary, waiting_grants))
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

    /// Conversation `id`, started by `author` (`None`: dibs), with `unread` messages unread, each
    /// written by its author: a member's conversation holds a member's messages, dibs's only
    /// dibs's.
    fn summary(id: u64, author: Option<(u64, &str)>, unread: u64) -> NamedSummary {
        let (record_author, named_author) = match author {
            Some((member, name)) => (
                Author::Member(MemberId(member)),
                AuthorName::Member(handle(name)),
            ),
            None => (Author::System, AuthorName::Dibs),
        };
        let by_member = author.is_some();
        let (people, dibs) = if by_member { (2, 0) } else { (0, 2) };
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
            unread_from_people: if by_member { unread } else { 0 },
            unread_from_dibs: if by_member { 0 } else { unread },
            from_people: people,
            from_dibs: dibs,
            last_from: author.map(|(_, name)| handle(name)),
            last_unread_from: author.filter(|_| unread > 0).map(|(_, name)| handle(name)),
            last_message_at: None,
            importance: Importance::Normal,
            awaiting_ack: 0,
        }
    }

    /// `summary` with `members` of its messages written by bob, the rest by dibs.
    fn written(mut summary: NamedSummary, members: u64, unread_from_people: u64) -> NamedSummary {
        summary.from_people = members;
        summary.from_dibs = summary.messages - members;
        summary.unread_from_people = unread_from_people;
        summary.unread_from_dibs = summary.unread - unread_from_people;
        if unread_from_people == 0 {
            summary.last_unread_from = None;
        }
        summary
    }

    #[test]
    fn a_conversation_holding_a_members_message_is_from_people_and_one_of_dibss_only_is_not() {
        let none = BTreeSet::new();
        assert!(from_people(&summary(1, Some((2, "bob")), 1), &none));
        assert!(
            from_people(&summary(2, Some((1, "alice")), 1), &none),
            "the viewer's own conversation counts once someone answers it"
        );
        assert!(!from_people(&summary(3, None, 1), &none));
        assert!(
            !from_people(&written(summary(4, Some((1, "alice")), 2), 0, 0), &none),
            "dibs writing in the viewer's name makes no message from people"
        );
        assert!(
            from_people(&written(summary(5, None, 2), 1, 1), &none),
            "a member's reply in a conversation dibs started"
        );
    }

    #[test]
    fn a_message_waiting_for_the_viewers_acknowledgement_keeps_a_conversation_among_peoples() {
        let mut notice = summary(6, None, 0);
        notice.awaiting_ack = 1;
        assert!(from_people(&notice, &BTreeSet::new()));
        assert_eq!(
            count_unread_among_people([&notice], &BTreeSet::new()),
            0,
            "nothing in it is unread"
        );
    }

    #[test]
    fn a_grant_request_waiting_for_the_viewer_is_not_from_people_until_it_is_decided() {
        let request = summary(4, Some((2, "bob")), 1);
        let waiting = BTreeSet::from([ThreadId(4)]);
        assert!(!from_people(&request, &waiting));
        assert!(from_people(&request, &BTreeSet::new()), "once decided");
    }

    #[test]
    fn the_unread_count_counts_the_conversations_from_people_holding_any_unread_message() {
        let waiting = BTreeSet::from([ThreadId(4)]);
        let all = [
            summary(1, Some((2, "bob")), 1),
            summary(2, Some((1, "alice")), 1),
            summary(3, None, 2),
            summary(4, Some((2, "bob")), 1),
            summary(5, Some((2, "bob")), 0),
            written(summary(6, None, 2), 1, 0),
            written(summary(7, Some((2, "bob")), 2), 1, 0),
        ];
        assert_eq!(
            count_unread_among_people(&all, &waiting),
            4,
            "a member's message read and dibs's unread still waits among people's"
        );
        assert_eq!(count_unread_among_people(&all, &BTreeSet::new()), 5);
    }

    #[test]
    fn dibss_unread_message_beside_a_read_members_message_is_unread_among_people_but_not_from_them()
    {
        let none = BTreeSet::new();
        let read_reply = written(summary(8, None, 1), 1, 0);
        assert!(unread_among_people(&read_reply, &none));
        assert!(!unread_people(&read_reply, &none));
        let read = written(summary(9, Some((2, "bob")), 0), 2, 0);
        assert!(!unread_among_people(&read, &none));
        let notice = summary(10, None, 1);
        assert!(
            !unread_among_people(&notice, &none),
            "a notice is not among people's"
        );
    }
}
