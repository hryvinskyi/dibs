//! `POST /v1/messages/{id}/ack`, `POST /v1/messages/{id}/deliver` and
//! `GET /v1/messages/{id}/status`: a recipient acknowledging a message, one of their sessions
//! claiming to show it to its agent, and its author seeing who read and acknowledged it. A message in a thread the caller
//! may not read is answered as missing, the same as one that does not exist. Integrations take no
//! part.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::DeliveryAsk;
use dibs_domain::ids::MessageId;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{message_in_path, session_in_body};
use crate::http::AppState;
use crate::views::{
    AckView, DeliveryView, GrantView, MessageView, RecipientStatusView, ThreadView, reply,
};

#[derive(Serialize)]
struct AckedAnswer {
    ack: AckView,
    created: bool,
    /// The grants the acknowledgement made: for a wide claim's announcement, one per area it asked
    /// about that the caller still holds; empty otherwise and for a repeat.
    granted: Vec<GrantView>,
    /// `true` when the message announced a wide claim that has ended, so the acknowledgement
    /// granted nothing; left out otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    announcement_ended: bool,
}

/// Records that the caller acknowledged the message. Answers `201` with the acknowledgement and
/// `created: true`; a caller who acknowledged it before is answered `200` with their first
/// acknowledgement and `created: false`, and nothing is recorded again. Acknowledging a wide
/// claim's announcement grants its member edits in the areas it asked about, listed in `granted`;
/// once that wide claim has ended it grants nothing, and the answer says so with
/// `announcement_ended: true`.
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
            announcement_ended: acknowledged.announcement_ended,
            ack: AckView::of(acknowledged),
        },
    ))
}

/// The body of `POST /v1/messages/{id}/deliver`.
#[derive(Deserialize)]
pub struct DeliverBody {
    session_id: Option<u64>,
}

#[derive(Serialize)]
pub(crate) struct DeliveredAnswer {
    pub(crate) delivery: DeliveryView,
}

/// Claims the delivery of a message to the agent of the caller's session `session_id`: the first
/// of the caller's sessions to claim it is the one that shows it, answered `201` with the
/// delivery. Every later claim, from any of their sessions, is refused, so a member is shown a
/// message once however many sessions they run.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a message the caller may not read or a session
/// that is not theirs; `not_a_recipient` (`403`) for a message the caller wrote or was not sent;
/// `delivered` (`409`, with the session that claimed it and when) when it was claimed before;
/// `invalid_request` without a `session_id`.
pub async fn deliver(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<DeliverBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let message = path_message(path)?;
    let Json(body) = body?;
    let session = session_in_body("session_id", body.session_id)?;
    let delivery = state
        .app
        .claim_delivery
        .execute(&actor, session, DeliveryAsk::Message(message))
        .await?;
    Ok(reply(
        StatusCode::CREATED,
        &DeliveredAnswer {
            delivery: DeliveryView::of(&delivery),
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
