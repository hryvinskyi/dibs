//! `POST /ui/threads/{id}/messages` and `POST /ui/threads/{id}/messages/{message}/ack`: a reply to
//! a conversation, signed by the person with the session label `web`, that may ask everyone to
//! acknowledge it; and the person's acknowledgement of a message. A reply and an acknowledgement
//! land on the message in Inbox; a reply refused for its text is shown again there, in the
//! conversation's reply form. Any other refusal is shown in Inbox's frame. A conversation the person
//! may not read is answered as one that does not exist.

use axum::Form;
use axum::extract::rejection::{FormRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use dibs_app::use_cases::PostMessageInput;
use dibs_domain::ids::{MessageId, ThreadId};
use dibs_domain::message::{Body, Importance, SessionLabel};

use crate::error::ApiError;
use crate::fields::{message_in_path, thread_in_path};
use crate::http::AppState;
use crate::ui::conversation::{Draft, ReplyProblem};
use crate::ui::inbox;
use crate::ui::notice::{Problem, notice, refused};
use crate::ui::render::see_other;
use crate::ui::session::{PageSession, UiSession};
use crate::ui::shell::Screen;
use crate::ui::signed_page;

/// The session label of what a person writes from the browser.
const WEB_LABEL: &str = "web";

/// Where a refusal leads back to.
const INBOX: &str = "/ui/inbox";

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
            Screen::Inbox,
            INBOX,
        )
        .await;
    }
    let thread = match path
        .map_err(ApiError::from)
        .and_then(|Path(id)| thread_in_path(&id))
    {
        Ok(thread) => thread,
        Err(error) => return refused(&state, &session, &error, Screen::Inbox, INBOX).await,
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
        Err(error) => refused(&state, &session, &error, Screen::Inbox, INBOX).await,
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
    let posted = state
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
        see_other(&format!(
            "/ui/inbox/{thread}#m-{}",
            posted.message.record.id
        )),
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
            Screen::Inbox,
            INBOX,
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
        Err(error) => return refused(&state, &session, &error, Screen::Inbox, INBOX).await,
    };
    match acknowledge(&state, &session, message).await {
        Ok(response) => response,
        Err(error) => refused(&state, &session, &error, Screen::Inbox, INBOX).await,
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
