//! `POST /v1/threads`, `GET /v1/threads`, `GET /v1/threads/{id}`, `POST /v1/threads/{id}/messages`,
//! `POST /v1/threads/{id}/read` and `GET /v1/inbox`: conversations between the people of a
//! project. A thread the caller may not read is answered as missing, the same as one that does not
//! exist. Integrations take no part: every route refuses them before reading the request.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{Addressees, PostMessageInput, StartThreadInput};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::message::{Body, SessionLabel, Subject};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_body, parse, parse_required, record_id_text, switch, thread_in_path};
use crate::http::AppState;
use crate::views::{MessageView, ThreadSummaryView, ThreadView, reply};

/// The body of `POST /v1/threads`.
#[derive(Deserialize)]
pub struct StartBody {
    subject: Option<String>,
    to: Option<Vec<String>>,
    to_project: Option<bool>,
    item: Option<u64>,
    label: Option<String>,
    body: Option<String>,
}

#[derive(Serialize)]
struct StartedAnswer {
    thread: ThreadView,
    message: MessageView,
}

/// Starts a thread with its first message, addressed either to the members `to` names by handle
/// or, with `to_project: true`, to every person in the project; `item` is the work item it is
/// about, `label` the session label it is signed with. Answers `201` with both.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for an unknown recipient or item;
/// `not_a_recipient` for an integration named in `to`; `no_recipients` for an empty `to`;
/// `invalid_request` for a missing or invalid field, or for both `to` and `to_project`.
pub async fn start(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<StartBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let input = StartThreadInput {
        subject: parse_required("subject", body.subject.as_deref(), Subject::parse)?,
        to: addressees(body.to, body.to_project)?,
        item: body
            .item
            .map(|number| item_in_body("item", Some(number)))
            .transpose()?,
        label: parse_required("label", body.label.as_deref(), SessionLabel::parse)?,
        body: parse_required("body", body.body.as_deref(), Body::parse)?,
    };
    let started = state.app.start_thread.execute(&actor, input).await?;
    let answer = StartedAnswer {
        thread: ThreadView::of(started.thread),
        message: MessageView::of(started.message),
    };
    Ok(reply(StatusCode::CREATED, &answer))
}

/// Whom a new thread goes to: the handles of `to`, or the project when `to_project` is true.
fn addressees(to: Option<Vec<String>>, to_project: Option<bool>) -> Result<Addressees, ApiError> {
    match (to, to_project.unwrap_or(false)) {
        (Some(_), true) => Err(ApiError::invalid_field(
            "to",
            &"name members in 'to' or set 'to_project', not both",
        )),
        (None, true) => Ok(Addressees::Project),
        (Some(handles), false) => handles
            .iter()
            .map(|text| parse("to", text, Handle::parse))
            .collect::<Result<_, _>>()
            .map(Addressees::Members),
        (None, false) => Err(ApiError::missing_field("to")),
    }
}

/// The body of `POST /v1/threads/{id}/messages`.
#[derive(Deserialize)]
pub struct PostBody {
    label: Option<String>,
    body: Option<String>,
}

#[derive(Serialize)]
struct PostedAnswer {
    message: MessageView,
}

/// Posts a message to a thread the caller may read, signed with the session label `label`.
/// Answers `201` with the message.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a thread the caller may not read;
/// `invalid_request` for a missing or invalid field.
pub async fn post(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<PostBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let thread = path_thread(path)?;
    let Json(body) = body?;
    let input = PostMessageInput {
        thread,
        label: parse_required("label", body.label.as_deref(), SessionLabel::parse)?,
        body: parse_required("body", body.body.as_deref(), Body::parse)?,
    };
    let message = state.app.post_message.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::CREATED,
        &PostedAnswer {
            message: MessageView::of(message),
        },
    ))
}

/// The query of `GET /v1/threads`.
#[derive(Deserialize)]
pub struct ListQuery {
    item: Option<String>,
}

#[derive(Serialize)]
struct ThreadsAnswer {
    threads: Vec<ThreadSummaryView>,
}

/// Lists the threads the caller may read, newest activity first; only those about the work item
/// `item`, when given.
///
/// # Errors
/// `forbidden` for an integration; `invalid_request` for an `item` that is not an id;
/// `invalid_query` for a malformed query string.
pub async fn list(
    State(state): State<AppState>,
    Authed(actor): Authed,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Query(query) = query?;
    let item = query
        .item
        .as_deref()
        .map(|text| parse("item", text, record_id_text).map(ItemId))
        .transpose()?;
    let threads = state.app.list_threads.execute(&actor, item).await?;
    Ok(reply(StatusCode::OK, &threads_answer(threads)))
}

/// The query of `GET /v1/inbox`.
#[derive(Deserialize)]
pub struct InboxQuery {
    unread: Option<String>,
}

/// Lists the threads waiting for the caller, newest activity first, each with how many of its
/// messages they have not read; with `unread=true`, only the threads that have such messages.
///
/// # Errors
/// `forbidden` for an integration; `invalid_request` for an `unread` that is not a switch;
/// `invalid_query` for a malformed query string.
pub async fn inbox(
    State(state): State<AppState>,
    Authed(actor): Authed,
    query: Result<Query<InboxQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Query(query) = query?;
    let unread_only = query
        .unread
        .as_deref()
        .map(|text| parse("unread", text, switch))
        .transpose()?
        .unwrap_or(false);
    let threads = state.app.inbox.execute(&actor, unread_only).await?;
    Ok(reply(StatusCode::OK, &threads_answer(threads)))
}

fn threads_answer(threads: Vec<dibs_app::named::NamedSummary>) -> ThreadsAnswer {
    ThreadsAnswer {
        threads: threads.into_iter().map(ThreadSummaryView::of).collect(),
    }
}

#[derive(Serialize)]
struct ThreadAnswer {
    thread: ThreadView,
    messages: Vec<MessageView>,
}

/// Shows a thread the caller may read with its messages, oldest first. Reading does not mark it
/// read.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a thread the caller may not read.
pub async fn read(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let thread = path_thread(path)?;
    let view = state.app.read_thread.execute(&actor, thread).await?;
    let answer = ThreadAnswer {
        thread: ThreadView::of(view.thread),
        messages: view.messages.into_iter().map(MessageView::of).collect(),
    };
    Ok(reply(StatusCode::OK, &answer))
}

#[derive(Serialize)]
struct MarkedAnswer {
    /// How many messages were unread before.
    newly_read: u64,
}

/// Marks every message of a thread the caller may read as read by them.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a thread the caller may not read.
pub async fn mark_read(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let thread = path_thread(path)?;
    let newly_read = state.app.mark_read.execute(&actor, thread).await?;
    Ok(reply(StatusCode::OK, &MarkedAnswer { newly_read }))
}

fn path_thread(path: Result<Path<String>, PathRejection>) -> Result<ThreadId, ApiError> {
    let Path(id) = path?;
    thread_in_path(&id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_goes_to_members_or_to_the_project() {
        assert_eq!(
            addressees(Some(vec!["bob".to_owned()]), None).unwrap(),
            Addressees::Members(vec![Handle::parse("bob").unwrap()])
        );
        assert_eq!(addressees(None, Some(true)).unwrap(), Addressees::Project);
        assert_eq!(
            addressees(Some(Vec::new()), Some(false)).unwrap(),
            Addressees::Members(Vec::new())
        );
        for (to, to_project) in [
            (None, None),
            (None, Some(false)),
            (Some(vec!["bob".to_owned()]), Some(true)),
            (Some(vec!["Not A Handle".to_owned()]), None),
        ] {
            assert_eq!(
                addressees(to, to_project).unwrap_err().code(),
                "invalid_request"
            );
        }
    }
}
