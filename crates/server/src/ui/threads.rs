//! `GET /ui/threads`, `GET /ui/threads/{id}` and `POST /ui/threads/{id}/messages`: the threads the
//! person may read, newest activity first with the unread marked; one thread with its messages,
//! oldest first, which opening marks read; and a reply, signed by the person with the session
//! label `web`.
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
use dibs_app::use_cases::{ItemQuery, PostMessageInput};
use dibs_domain::ids::{ItemId, MemberId, ThreadId};
use dibs_domain::message::{Body, BodyError, Recipients, SessionLabel};

use crate::error::ApiError;
use crate::fields::thread_in_path;
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
}

#[derive(Template)]
#[template(path = "threads.html")]
struct ThreadsPage {
    chrome: Chrome,
    rows: Vec<ThreadRow>,
}

/// Who a thread is between: its author, then the members it is addressed to.
fn people(thread: &NamedThread) -> (Vec<String>, bool) {
    let mut people = vec![thread.author.to_string()];
    people.extend(
        thread
            .recipients
            .iter()
            .filter(|handle| **handle != thread.author)
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
            }
        })
        .collect();
    let page = ThreadsPage {
        chrome: signed.chrome,
        rows,
    };
    Ok(signed_page(&session, html(StatusCode::OK, &page)))
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
}

impl MessageRow {
    fn of(named: &NamedMessage, viewer: MemberId, now: OffsetDateTime) -> Self {
        let message = &named.record;
        Self {
            id: message.id.0,
            author: named.author.to_string(),
            label: message.label.to_string(),
            body: message.body.to_string(),
            at: moment(message.created_at, now),
            mine: message.author == viewer,
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
    messages: Vec<MessageRow>,
    after: u64,
    rendered: Moment,
    problem: Option<ReplyProblem>,
    draft: String,
}

/// A reply that was not sent, shown again in the form.
struct Draft {
    text: String,
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
    let (status, problem, draft) = match draft {
        Some(draft) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some(draft.problem),
            draft.text,
        ),
        None => (StatusCode::OK, None, String::new()),
    };
    let page = ThreadPage {
        chrome: signed.chrome,
        id: view.thread.record.id.0,
        subject: view.thread.record.subject.to_string(),
        reference: item_reference,
        people,
        to_project,
        messages: view
            .messages
            .iter()
            .map(|message| MessageRow::of(message, actor.member, now))
            .collect(),
        after,
        rendered: moment(now, now),
        problem,
        draft,
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
    let text = form.and_then(|form| form.body).unwrap_or_default();
    let answer = match Body::parse(&text) {
        Ok(body) => post(&state, &session, thread, body).await,
        Err(error) => {
            let draft = Draft {
                text,
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

/// Posts the reply and answers the way to it.
async fn post(
    state: &AppState,
    session: &UiSession,
    thread: ThreadId,
    body: Body,
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
            },
        )
        .await?;
    Ok(signed_page(
        session,
        see_other(&format!("/ui/threads/{thread}#m-{}", message.record.id)),
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
