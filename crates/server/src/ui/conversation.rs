//! One conversation as a page shows it: its subject, the item it is about and the people in it, its
//! messages oldest first — each body as written and with the references in it linked — the
//! acknowledgement of each message that asks for one and whether the viewer may give theirs, and
//! the reply form with a draft that was not sent and why.

use time::OffsetDateTime;

use dibs_app::named::{NamedMessage, NamedThread};
use dibs_app::records::ItemRecord;
use dibs_app::use_cases::{RecipientAck, ThreadView};
use dibs_domain::ids::MemberId;
use dibs_domain::item::ItemRepository;
use dibs_domain::message::{BodyError, Importance, Recipients};
use dibs_domain::repo::RepoId;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::links::{LinkContext, LinkedText, PersonView, TicketView};
use crate::ui::moments::{Moment, moment};
use crate::ui::rows::reference;
use crate::ui::session::UiSession;

/// One conversation.
#[derive(Debug, Clone)]
pub(crate) struct Conversation {
    /// The thread's id.
    pub(crate) id: u64,
    /// The subject, as written.
    pub(crate) subject: String,
    /// The subject, with the references in it linked.
    pub(crate) title: LinkedText,
    /// How the item it is about is named, when it is about one.
    pub(crate) reference: Option<String>,
    /// The ticket of the item it is about, when it is about one.
    pub(crate) ticket: Option<TicketView>,
    /// Its author, then the members it is addressed to.
    pub(crate) people: Vec<String>,
    /// Whether it is addressed to the whole project.
    pub(crate) to_project: bool,
    /// The highest importance among the messages.
    pub(crate) importance: Importance,
    /// The messages, oldest first.
    pub(crate) messages: Vec<MessageRow>,
    /// The handle of the member the thread's author is, on whose behalf what dibs writes in it
    /// speaks; empty for a thread dibs started.
    pub(crate) sender: String,
    /// Why the draft was not sent, when there is one.
    pub(crate) problem: Option<ReplyProblem>,
    /// The text of a reply that was not sent.
    pub(crate) draft: String,
    /// Whether that reply asked everyone to acknowledge it.
    pub(crate) draft_asks_ack: bool,
}

/// One recipient of a message that asks to be acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AckEntry {
    /// Their handle.
    pub(crate) handle: String,
    /// Whether they are the viewer.
    pub(crate) you: bool,
    /// When they acknowledged it, once they did.
    pub(crate) acked: Option<Moment>,
}

/// The acknowledgements of a message that asks for them: who has and who has not, and whether the
/// viewer may acknowledge it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AckBlock {
    /// Each recipient, by handle.
    pub(crate) entries: Vec<AckEntry>,
    /// Whether the viewer may acknowledge it.
    pub(crate) may_ack: bool,
}

impl AckBlock {
    /// The block of a message; `asking` says whether it still asks for an acknowledgement, which a
    /// wide claim's announcement does only while its wide claim stands.
    fn of(acks: &[RecipientAck], viewer: MemberId, asking: bool, now: OffsetDateTime) -> Self {
        Self {
            entries: acks
                .iter()
                .map(|ack| AckEntry {
                    handle: ack.handle.to_string(),
                    you: ack.member == viewer,
                    acked: ack.acked_at.map(|at| moment(at, now)),
                })
                .collect(),
            may_ack: asking
                && acks
                    .iter()
                    .any(|ack| ack.member == viewer && ack.acked_at.is_none()),
        }
    }
}

/// A message of a conversation.
#[derive(Debug, Clone)]
pub(crate) struct MessageRow {
    /// Its id.
    pub(crate) id: u64,
    /// Who wrote it: a handle, or dibs.
    pub(crate) author: String,
    /// The member who wrote it; `None` when dibs did.
    pub(crate) person: Option<PersonView>,
    /// The label of the session it was written from.
    pub(crate) label: String,
    /// The body, as written.
    pub(crate) body: String,
    /// The body, with the references in it linked.
    pub(crate) text: LinkedText,
    /// When it was written.
    pub(crate) at: Moment,
    /// Whether the viewer wrote it.
    pub(crate) mine: bool,
    /// Whether dibs wrote it, in the project's name.
    pub(crate) system: bool,
    /// Its importance.
    pub(crate) importance: Importance,
    /// Its acknowledgements, when it asks for them.
    pub(crate) ack: Option<AckBlock>,
}

impl MessageRow {
    fn of(
        named: &NamedMessage,
        acks: Option<&Vec<RecipientAck>>,
        asking: bool,
        viewer: MemberId,
        scene: &Scene<'_>,
    ) -> Self {
        let message = &named.record;
        Self {
            id: message.id.0,
            author: named.author.to_string(),
            person: named.author.handle().map(PersonView::of),
            label: message.label.to_string(),
            body: message.body.to_string(),
            text: LinkedText::of(message.body.as_str(), scene.home, scene.links),
            at: moment(message.created_at, scene.now),
            mine: message.author.is(viewer),
            system: named.author.is_dibs(),
            importance: message.importance,
            ack: acks.map(|acks| AckBlock::of(acks, viewer, asking, scene.now)),
        }
    }
}

/// Why a reply was not sent. The wording of each is the template's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplyProblem {
    /// Nothing but whitespace.
    Empty,
    /// More than a message may hold.
    TooLong,
    /// A character no message may hold.
    Nul,
}

impl ReplyProblem {
    /// The problem a refused body is shown as.
    pub(crate) const fn of(error: &BodyError) -> Self {
        match error {
            BodyError::Empty => Self::Empty,
            BodyError::TooLong => Self::TooLong,
            BodyError::Nul => Self::Nul,
        }
    }
}

/// A reply that was not sent, shown again in the form.
#[derive(Debug, Clone)]
pub(crate) struct Draft {
    /// Its text.
    pub(crate) text: String,
    /// Whether it asked everyone to acknowledge it.
    pub(crate) asks_ack: bool,
    /// Why it was not sent.
    pub(crate) problem: ReplyProblem,
}

/// What every message's view is built against.
struct Scene<'a> {
    home: Option<&'a RepoId>,
    links: &'a LinkContext,
    now: OffsetDateTime,
}

impl Conversation {
    /// The conversation `view`, about `item` when it is about one, as `viewer` sees it at `now`,
    /// with `draft` in its reply form. A bare `#N` or a commit hash in a message belongs to the
    /// repository of the item, when the project lists it.
    pub(crate) fn of(
        view: &ThreadView,
        item: Option<&ItemRecord>,
        viewer: MemberId,
        now: OffsetDateTime,
        links: &LinkContext,
        draft: Option<Draft>,
    ) -> Self {
        let home = item.and_then(|item| match &item.repository {
            ItemRepository::Listed(repo) => Some(repo),
            ItemRepository::Unlisted(_) | ItemRepository::Unnamed => None,
        });
        let scene = Scene { home, links, now };
        let record = &view.thread.record;
        let (people, to_project) = people(&view.thread);
        let (problem, draft, draft_asks_ack) = match draft {
            Some(draft) => (Some(draft.problem), draft.text, draft.asks_ack),
            None => (None, String::new(), false),
        };
        Self {
            id: record.id.0,
            subject: record.subject.to_string(),
            title: LinkedText::of(record.subject.as_str(), home, links),
            reference: item.map(|item| reference(item.id, item.external_ref.as_ref())),
            ticket: item.map(|item| TicketView::of(item.id, item.external_ref.as_ref(), links)),
            people,
            to_project,
            importance: view
                .messages
                .iter()
                .map(|message| message.record.importance)
                .max()
                .unwrap_or_default(),
            messages: view
                .messages
                .iter()
                .map(|message| {
                    let acks = view.acks.get(&message.record.id);
                    let asking = view.asking.contains(&message.record.id);
                    MessageRow::of(message, acks, asking, viewer, &scene)
                })
                .collect(),
            sender: view
                .thread
                .author
                .handle()
                .map(ToString::to_string)
                .unwrap_or_default(),
            problem,
            draft,
            draft_asks_ack,
        }
    }
}

/// The item the conversation `view` is about, when it is about one.
///
/// # Errors
/// The answer for the use case's failure.
pub(crate) async fn item_of(
    state: &AppState,
    session: &UiSession,
    view: &ThreadView,
) -> Result<Option<ItemRecord>, ApiError> {
    match view.thread.record.item {
        Some(id) => {
            let item = state.app.get_item.execute(session.actor(), id).await?;
            Ok(Some(item.item.item))
        }
        None => Ok(None),
    }
}

/// Who a thread is between: its author, then the members it is addressed to; and whether it is
/// addressed to the whole project.
pub(crate) fn people(thread: &NamedThread) -> (Vec<String>, bool) {
    let mut people = vec![thread.author.to_string()];
    people.extend(
        thread
            .recipients
            .iter()
            .filter(|handle| thread.author.handle() != Some(*handle))
            .map(ToString::to_string),
    );
    let to_project = matches!(thread.record.recipients, Recipients::Project);
    (people, to_project)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_refusal_of_a_body_has_its_reason() {
        assert_eq!(ReplyProblem::of(&BodyError::Empty), ReplyProblem::Empty);
        assert_eq!(ReplyProblem::of(&BodyError::TooLong), ReplyProblem::TooLong);
        assert_eq!(ReplyProblem::of(&BodyError::Nul), ReplyProblem::Nul);
    }
}
