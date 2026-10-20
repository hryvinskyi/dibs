//! `GET /ui/threads`, `GET /ui/threads/{id}`, `POST /ui/threads/{id}/messages` and
//! `POST /ui/threads/{id}/messages/{message}/ack`: the threads the person may read, newest activity
//! first with the unread, the importance and what awaits their acknowledgement marked; one thread
//! with its messages, oldest first, which opening marks read, each message's importance shown and,
//! for one that asks to be acknowledged, who has and a button for a recipient who has not; a reply,
//! signed by the person with the session label `web`, that may ask everyone to acknowledge it; and
//! the person's acknowledgement of a message.
//!
//! A message's text is shown exactly as written, escaped, its line breaks kept by the stylesheet. A
//! thread the person may not read is answered as one that does not exist.

use std::collections::BTreeMap;

use askama::Template;
use axum::Form;
use axum::extract::rejection::{FormRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use time::OffsetDateTime;

use dibs_app::named::{NamedMessage, NamedSummary, NamedThread};
use dibs_app::use_cases::{ItemQuery, PostMessageInput, RecipientAck};
use dibs_domain::ids::{ItemId, MemberId, MessageId, ThreadId};
use dibs_domain::message::{Body, BodyError, Importance, Recipients, SessionLabel};

use crate::error::ApiError;
use crate::fields::{message_in_path, thread_in_path};
use crate::http::AppState;
use crate::ui::chrome::{self, Chrome, Section};
use crate::ui::live::latest_event;
use crate::ui::moments::{Ago, Moment, moment};
use crate::ui::notice::{Problem, notice, refused};
use crate::ui::render::{html, see_other};
use crate::ui::rows::reference;
use crate::ui::session::{PageSession, UiSession};
use crate::ui::signed_page;

/// The session label of what a person writes from the browser.
const WEB_LABEL: &str = "web";

/// A thread in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThreadRow {
    id: u64,
    subject: String,
    reference: Option<String>,
    people: Vec<String>,
    to_project: bool,
    unread: u64,
    last: Option<Moment>,
    importance: Importance,
    awaiting_ack: u64,
}

#[derive(Template)]
#[template(path = "threads.html")]
struct ThreadsPage {
    chrome: Chrome,
    rows: Vec<ThreadRow>,
    /// How many threads hold a message awaiting the person's acknowledgement.
    awaiting: usize,
}

/// Who a thread is between: its author, then the members it is addressed to.
fn people(thread: &NamedThread) -> (Vec<String>, bool) {
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

/// `GET /ui/threads`: the threads the person may read, newest activity first.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn list(
    State(state): State<AppState>,
    PageSession(session): PageSession,
) -> Result<Response, ApiError> {
    let actor = session.actor();
    let threads = state.app.list_threads.execute(actor, None).await?;
    let items: BTreeMap<ItemId, String> = state
        .app
        .list_items
        .execute(actor, ItemQuery::default())
        .await?
        .into_iter()
        .map(|named| {
            let id = named.item.id;
            (id, reference(id, named.item.external_ref.as_ref()))
        })
        .collect();
    let signed = chrome::load(&state, &session, Section::Threads).await?;
    let now = state.clock.now();
    let rows = threads
        .iter()
        .map(|summary: &NamedSummary| {
            let thread = &summary.thread;
            let (people, to_project) = people(thread);
            ThreadRow {
                id: thread.record.id.0,
                subject: thread.record.subject.to_string(),
                reference: thread.record.item.map(|id| {
                    items
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| reference(id, None))
                }),
                people,
                to_project,
                unread: summary.unread,
                last: summary.last_message_at.map(|at| moment(at, now)),
                importance: summary.importance,
                awaiting_ack: summary.awaiting_ack,
            }
        })
        .collect::<Vec<_>>();
    let page = ThreadsPage {
        chrome: signed.chrome,
        awaiting: rows.iter().filter(|row| row.awaiting_ack > 0).count(),
        rows,
    };
    Ok(signed_page(&session, html(StatusCode::OK, &page)))
}

/// One recipient of a message that asks to be acknowledged, as the thread page lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AckEntry {
    handle: String,
    you: bool,
    acked: Option<Moment>,
}

/// The acknowledgements of a message that asks for them: who has and who has not, and whether the
/// person may acknowledge it now.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AckBlock {
    entries: Vec<AckEntry>,
    may_ack: bool,
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

/// A message on the thread page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageRow {
    id: u64,
    author: String,
    label: String,
    body: String,
    at: Moment,
    mine: bool,
    /// Whether dibs wrote it, in the project's name.
    system: bool,
    importance: Importance,
    ack: Option<AckBlock>,
}

impl MessageRow {
    fn of(
        named: &NamedMessage,
        acks: Option<&Vec<RecipientAck>>,
        asking: bool,
        viewer: MemberId,
        now: OffsetDateTime,
    ) -> Self {
        let message = &named.record;
        Self {
            id: message.id.0,
            author: named.author.to_string(),
            label: message.label.to_string(),
            body: message.body.to_string(),
            at: moment(message.created_at, now),
            mine: message.author.is(viewer),
            system: named.author.is_dibs(),
            importance: message.importance,
            ack: acks.map(|acks| AckBlock::of(acks, viewer, asking, now)),
        }
    }
}

/// Why a reply was not sent. The wording of each is the template's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplyProblem {
    /// Nothing but whitespace.
    Empty,
    /// More than a message may hold.
    TooLong,
    /// A character no message may hold.
    Nul,
}

impl ReplyProblem {
    const fn of(error: &BodyError) -> Self {
        match error {
            BodyError::Empty => Self::Empty,
            BodyError::TooLong => Self::TooLong,
            BodyError::Nul => Self::Nul,
        }
    }
}

#[derive(Template)]
#[template(path = "thread.html")]
struct ThreadPage {
    chrome: Chrome,
    id: u64,
    subject: String,
    reference: Option<String>,
    people: Vec<String>,
    to_project: bool,
    /// The highest importance among the messages.
    importance: Importance,
    messages: Vec<MessageRow>,
    /// The handle of the member the thread's author is, on whose behalf what dibs writes in it
    /// speaks; empty for a thread dibs started.
    sender: String,
    after: u64,
    rendered: Moment,
    problem: Option<ReplyProblem>,
    draft: String,
    /// Whether the draft asked everyone to acknowledge it.
    draft_asks_ack: bool,
}

/// A reply that was not sent, shown again in the form.
struct Draft {
    text: String,
    asks_ack: bool,
    problem: ReplyProblem,
}

/// The thread page, marking the thread read; with a draft, its text and the reason it was not sent.
async fn thread_page(
    state: &AppState,
    session: &UiSession,
    thread: ThreadId,
    draft: Option<Draft>,
) -> Result<Response, ApiError> {
    let actor = session.actor();
    let after = latest_event(state, actor).await?;
    let view = state.app.read_thread.execute(actor, thread).await?;
    state.app.mark_read.execute(actor, thread).await?;
    let item_reference = match view.thread.record.item {
        Some(id) => {
            let item = state.app.get_item.execute(actor, id).await?;
            Some(reference(id, item.item.item.external_ref.as_ref()))
        }
        None => None,
    };
    let signed = chrome::load(state, session, Section::Threads).await?;
    let now = state.clock.now();
    let (people, to_project) = people(&view.thread);
    let (status, problem, draft, draft_asks_ack) = match draft {
        Some(draft) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some(draft.problem),
            draft.text,
            draft.asks_ack,
        ),
        None => (StatusCode::OK, None, String::new(), false),
    };
    let page = ThreadPage {
        chrome: signed.chrome,
        id: view.thread.record.id.0,
        subject: view.thread.record.subject.to_string(),
        reference: item_reference,
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
                MessageRow::of(message, acks, asking, actor.member, now)
            })
            .collect(),
        sender: view
            .thread
            .author
            .handle()
            .map(ToString::to_string)
            .unwrap_or_default(),
        after,
        rendered: moment(now, now),
        problem,
        draft,
        draft_asks_ack,
    };
    Ok(signed_page(session, html(status, &page)))
}

/// `GET /ui/threads/{id}`: one thread and its messages, oldest first. Opening it marks it read.
pub async fn page(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    path: Result<Path<String>, PathRejection>,
) -> Response {
    let thread = match path
        .map_err(ApiError::from)
        .and_then(|Path(id)| thread_in_path(&id))
    {
        Ok(thread) => thread,
        Err(error) => return refused(&state, &session, &error, "/ui/threads").await,
    };
    match thread_page(&state, &session, thread, None).await {
        Ok(response) => response,
        Err(error) => refused(&state, &session, &error, "/ui/threads").await,
    }
}

/// The fields of the reply form.
#[derive(Deserialize)]
pub struct ReplyForm {
    csrf: Option<String>,
    body: Option<String>,
    require_ack: Option<String>,
}

/// `POST /ui/threads/{id}/messages`: posts the person's reply and shows it in the thread. A reply
/// the rules refuse is shown again in the form with the reason.
pub async fn reply(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    path: Result<Path<String>, PathRejection>,
    form: Result<Form<ReplyForm>, FormRejection>,
) -> Response {
    let form = form.ok().map(|Form(form)| form);
    let sent = form.as_ref().and_then(|form| form.csrf.as_deref());
    if !session.accepts(sent) {
        return notice(
            &state,
            &session,
            StatusCode::FORBIDDEN,
            Problem::FormExpired,
            "/ui/threads",
        )
        .await;
    }
    let thread = match path
        .map_err(ApiError::from)
        .and_then(|Path(id)| thread_in_path(&id))
    {
        Ok(thread) => thread,
        Err(error) => return refused(&state, &session, &error, "/ui/threads").await,
    };
    let asks_ack = form.as_ref().is_some_and(|form| form.require_ack.is_some());
    let text = form.and_then(|form| form.body).unwrap_or_default();
    let answer = match Body::parse(&text) {
        Ok(body) => post(&state, &session, thread, body, asks_ack).await,
        Err(error) => {
            let draft = Draft {
                text,
                asks_ack,
                problem: ReplyProblem::of(&error),
            };
            thread_page(&state, &session, thread, Some(draft)).await
        }
    };
    match answer {
        Ok(response) => response,
        Err(error) => refused(&state, &session, &error, "/ui/threads").await,
    }
}

/// Posts the reply, asking everyone to acknowledge it when `asks_ack`, and answers the way to it.
async fn post(
    state: &AppState,
    session: &UiSession,
    thread: ThreadId,
    body: Body,
    asks_ack: bool,
) -> Result<Response, ApiError> {
    let label = SessionLabel::parse(WEB_LABEL).map_err(|_| ApiError::internal())?;
    let message = state
        .app
        .post_message
        .execute(
            session.actor(),
            PostMessageInput {
                thread,
                label,
                body,
                importance: Importance::Normal,
                ack_required: asks_ack,
            },
        )
        .await?;
    Ok(signed_page(
        session,
        see_other(&format!("/ui/threads/{thread}#m-{}", message.record.id)),
    ))
}

/// The fields of the acknowledge form.
#[derive(Deserialize)]
pub struct AckForm {
    csrf: Option<String>,
}

/// `POST /ui/threads/{id}/messages/{message}/ack`: records the person's acknowledgement of a
/// message and shows it in its thread. An acknowledgement given already, from another tab or a
/// second click, changes nothing and shows the thread the same way.
pub async fn ack(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    path: Result<Path<(String, String)>, PathRejection>,
    form: Result<Form<AckForm>, FormRejection>,
) -> Response {
    let sent = form
        .as_ref()
        .ok()
        .and_then(|Form(form)| form.csrf.as_deref());
    if !session.accepts(sent) {
        return notice(
            &state,
            &session,
            StatusCode::FORBIDDEN,
            Problem::FormExpired,
            "/ui/threads",
        )
        .await;
    }
    let ids = path
        .map_err(ApiError::from)
        .and_then(|Path((thread, message))| {
            Ok((thread_in_path(&thread)?, message_in_path(&message)?))
        });
    let message = match ids {
        Ok((_, message)) => message,
        Err(error) => return refused(&state, &session, &error, "/ui/threads").await,
    };
    match acknowledge(&state, &session, message).await {
        Ok(response) => response,
        Err(error) => refused(&state, &session, &error, "/ui/threads").await,
    }
}

/// Acknowledges the message and answers the way to it in its thread.
async fn acknowledge(
    state: &AppState,
    session: &UiSession,
    message: MessageId,
) -> Result<Response, ApiError> {
    let acknowledged = state.app.ack.execute(session.actor(), message).await?;
    let thread = acknowledged.thread;
    Ok(signed_page(
        session,
        see_other(&format!("/ui/threads/{thread}#m-{message}")),
    ))
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
