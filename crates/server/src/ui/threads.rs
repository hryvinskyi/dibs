//! `GET /ui/threads`, `GET /ui/threads/{id}`, `POST /ui/threads/{id}/messages` and
//! `POST /ui/threads/{id}/messages/{message}/ack`: the threads the person may read, newest activity
//! first with the unread, the importance and what awaits their acknowledgement marked; one thread
//! with its messages, oldest first, which opening marks read, each message's importance shown and,
//! for one that asks to be acknowledged, who has and a button for a recipient who has not; a reply,
//! signed by the person with the session label `web`, that may ask everyone to acknowledge it; and
//! the person's acknowledgement of a message. A reply and an acknowledgement land on the message in
//! Inbox; a reply refused for its text is shown again there, in the conversation's reply form.
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

use dibs_app::named::NamedSummary;
use dibs_app::use_cases::{ItemQuery, PostMessageInput};
use dibs_domain::ids::{ItemId, MessageId, ThreadId};
use dibs_domain::message::{Body, Importance, SessionLabel};

use crate::error::ApiError;
use crate::fields::{message_in_path, thread_in_path};
use crate::http::AppState;
use crate::ui::chrome::{self, Chrome, Section};
use crate::ui::conversation::{Conversation, Draft, MessageRow, ReplyProblem, item_of, people};
use crate::ui::inbox;
use crate::ui::links::LinkContext;
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

/// The thread page, marking the thread read.
async fn thread_page(
    state: &AppState,
    session: &UiSession,
    thread: ThreadId,
) -> Result<Response, ApiError> {
    let actor = session.actor();
    let after = latest_event(state, actor).await?;
    let view = state.app.read_thread.execute(actor, thread).await?;
    state.app.mark_read.execute(actor, thread).await?;
    let item = item_of(state, session, &view).await?;
    let signed = chrome::load(state, session, Section::Threads).await?;
    let links = LinkContext::new(
        signed.settings,
        state.ref_schemes.clone(),
        state.forges.clone(),
        Vec::new(),
    );
    let now = state.clock.now();
    let Conversation {
        id,
        subject,
        reference,
        people,
        to_project,
        importance,
        messages,
        sender,
        problem,
        draft,
        draft_asks_ack,
        ..
    } = Conversation::of(&view, item.as_ref(), actor.member, now, &links, None);
    let page = ThreadPage {
        chrome: signed.chrome,
        id,
        subject,
        reference,
        people,
        to_project,
        importance,
        messages,
        sender,
        after,
        rendered: moment(now, now),
        problem,
        draft,
        draft_asks_ack,
    };
    Ok(signed_page(session, html(StatusCode::OK, &page)))
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
    match thread_page(&state, &session, thread).await {
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

/// `POST /ui/threads/{id}/messages`: posts the person's reply and shows it in its conversation in
/// Inbox. A reply whose text the rules refuse is shown again in that conversation's form with the
/// reason.
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
            inbox::refused_reply(&state, &session, thread, draft).await
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
        see_other(&format!("/ui/inbox/{thread}#m-{}", message.record.id)),
    ))
}

/// The fields of the acknowledge form.
#[derive(Deserialize)]
pub struct AckForm {
    csrf: Option<String>,
}

/// `POST /ui/threads/{id}/messages/{message}/ack`: records the person's acknowledgement of a
/// message and shows it in its conversation in Inbox. An acknowledgement given already, from another
/// tab or a second click, changes nothing and shows the conversation the same way.
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

/// Acknowledges the message and answers the way to it in its conversation.
async fn acknowledge(
    state: &AppState,
    session: &UiSession,
    message: MessageId,
) -> Result<Response, ApiError> {
    let acknowledged = state.app.ack.execute(session.actor(), message).await?;
    let thread = acknowledged.thread;
    Ok(signed_page(
        session,
        see_other(&format!("/ui/inbox/{thread}#m-{message}")),
    ))
}
