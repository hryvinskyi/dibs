//! `POST /v1/messages/{id}/ack` and `GET /v1/messages/{id}/status`: a recipient acknowledging a
//! message, and its author seeing who read and acknowledged it. A message in a thread the caller
//! may not read is answered as missing, the same as one that does not exist. Integrations take no
//! part.

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;

use dibs_domain::ids::MessageId;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::message_in_path;
use crate::http::AppState;
use crate::views::{AckView, GrantView, MessageView, RecipientStatusView, ThreadView, reply};

#[derive(Serialize)]
struct AckedAnswer {
    ack: AckView,
    created: bool,
    /// The grants the acknowledgement made: for a wide claim's announcement, one per area it asked
    /// about that the caller still holds; empty otherwise and for a repeat.
    granted: Vec<GrantView>,
}

/// Records that the caller acknowledged the message. Answers `201` with the acknowledgement and
/// `created: true`; a caller who acknowledged it before is answered `200` with their first
/// acknowledgement and `created: false`, and nothing is recorded again. Acknowledging a wide
/// claim's announcement grants its member edits in the areas it asked about, listed in `granted`.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a message the caller may not read;
/// `not_a_recipient` (`403`) for its author or a member it did not reach; `ack_not_required`
/// (`409`) for a message that asks for no acknowledgement.
pub async fn ack(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let message = path_message(path)?;
    let acknowledged = state.app.ack.execute(&actor, message).await?;
    let status = if acknowledged.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(reply(
        status,
        &AckedAnswer {
            created: acknowledged.created,
            granted: acknowledged
                .granted
                .iter()
                .cloned()
                .map(GrantView::of)
                .collect(),
            ack: AckView::of(acknowledged),
        },
    ))
}

#[derive(Serialize)]
struct StatusAnswer {
    thread: ThreadView,
    message: MessageView,
    recipients: Vec<RecipientStatusView>,
}

/// Shows the author of a message each of its recipients, ordered by handle, with when they read it
/// and when they acknowledged it.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a message the caller may not read;
/// `not_author` (`403`) for a message somebody else wrote.
pub async fn status(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let message = path_message(path)?;
    let view = state.app.sent_status.execute(&actor, message).await?;
    let answer = StatusAnswer {
        thread: ThreadView::of(view.thread),
        message: MessageView::of(view.message),
        recipients: view
            .recipients
            .into_iter()
            .map(RecipientStatusView::of)
            .collect(),
    };
    Ok(reply(StatusCode::OK, &answer))
}

fn path_message(path: Result<Path<String>, PathRejection>) -> Result<MessageId, ApiError> {
    let Path(id) = path?;
    message_in_path(&id)
}
