//! `POST /v1/escalations`: asking the project's arbiters to settle what agents do not agree on.

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::EscalateInput;
use dibs_domain::escalation::{EscalationReason, EscalationSubject};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{MessageId, ThreadId};
use dibs_domain::slot::SlotName;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_body, parse, parse_required, thread_in_body};
use crate::http::AppState;
use crate::views::reply;

/// The body of `POST /v1/escalations`: one subject, and the reason.
#[derive(Deserialize)]
pub struct EscalateBody {
    item: Option<u64>,
    slot: Option<String>,
    thread: Option<u64>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct EscalationView {
    thread_id: ThreadId,
    message_id: MessageId,
    arbiters: Vec<Handle>,
    reached: Vec<Handle>,
}

#[derive(Serialize)]
struct EscalatedAnswer {
    escalation: EscalationView,
}

/// Sends an `urgent` message, written by dibs and asking to be acknowledged, to the project's
/// arbiters and to the subject's holder and requester, and answers `201` with whom it reached. It
/// stands until an arbiter acknowledges it, and counts toward no limit on raised messages.
///
/// # Errors
/// `forbidden` for an integration; `invalid_request` for a body that names no subject or more than
/// one, or no reason; `not_found` for an unknown item or a conversation the caller may not read;
/// `unknown_slot` (`404`); `no_arbiters` (`409`) when the project names no arbiter but the caller.
pub async fn escalate(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<EscalateBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let subject = match (body.item, body.slot.as_deref(), body.thread) {
        (Some(item), None, None) => EscalationSubject::Item(item_in_body("item", Some(item))?),
        (None, Some(slot), None) => EscalationSubject::Slot(parse("slot", slot, SlotName::parse)?),
        (None, None, Some(thread)) => {
            EscalationSubject::Thread(thread_in_body("thread", Some(thread))?)
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                "name exactly one of item, slot and thread to escalate",
            ));
        }
    };
    let reason = parse_required("reason", body.reason.as_deref(), EscalationReason::parse)?;
    let sent = state
        .app
        .escalate
        .execute(&actor, EscalateInput { subject, reason })
        .await?;
    Ok(reply(
        StatusCode::CREATED,
        &EscalatedAnswer {
            escalation: EscalationView {
                thread_id: sent.thread,
                message_id: sent.message,
                arbiters: sent.arbiters,
                reached: sent.reached,
            },
        },
    ))
}
