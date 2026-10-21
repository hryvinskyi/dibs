//! `GET /v1/slots`, `GET /v1/slots/{name}` and `POST /v1/slots/{name}/take`, `/release`,
//! `/leave`, `/heartbeat`, `/takeover`, `/run-finished`, `/offer/deliver`, and the admin's
//! `POST /v1/admin/slots/{name}/force-release`: taking, keeping and ending holds on shared actions,
//! and claiming the delivery of an offer to one of the caller's sessions.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use dibs_app::use_cases::{
    DeliveryAsk, ForceReleaseSlotInput, HoldMode, ReleaseSlotInput, RunFinishedInput,
    TakeOverSlotInput, TakeSlotInput, TookKind,
};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{RunId, ThreadId};
use dibs_domain::message::Body;
use dibs_domain::slot::{CommitSha, EndWord, Purpose, RunExit, SlotName, WorkflowName};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{
    item_in_body, parse, parse_required, record_id_text, required, session_in_body,
};
use crate::http::AppState;
use crate::versions::ClientReads;
use crate::views::slots::{HoldView, SlotView};
use crate::views::{DeliveryView, reply};

/// The slot a path segment names. Text that is no slot name names no declared slot.
fn slot_in_path(path: Result<Path<String>, PathRejection>) -> Result<SlotName, ApiError> {
    let Path(text) = path?;
    SlotName::parse(&text).map_err(|_| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_slot",
            "the project declares no slot of that name",
        )
        .with_detail("slot", &text)
    })
}

#[derive(Serialize)]
struct SlotsAnswer {
    slots: Vec<SlotView>,
}

#[derive(Serialize)]
struct SlotAnswer {
    slot: SlotView,
}

/// Every declared slot, with its group's hold, queue and offer. An integration may read them.
///
/// # Errors
/// `unauthorized` without a valid token.
pub async fn list(
    State(state): State<AppState>,
    Authed(actor): Authed,
    reads: ClientReads,
) -> Result<Response, ApiError> {
    let lines = state.app.slots.execute(&actor).await?;
    Ok(reply(
        StatusCode::OK,
        &SlotsAnswer {
            slots: lines
                .into_iter()
                .map(|line| SlotView::of(line, reads))
                .collect(),
        },
    ))
}

/// One slot, fresh: what time changed is applied before it is read.
///
/// # Errors
/// `unknown_slot` (`404`) for a slot the settings do not declare.
pub async fn get(
    State(state): State<AppState>,
    Authed(actor): Authed,
    reads: ClientReads,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let slot = slot_in_path(path)?;
    let line = state.app.slots.one(&actor, &slot).await?;
    Ok(reply(
        StatusCode::OK,
        &SlotAnswer {
            slot: SlotView::of(line, reads),
        },
    ))
}

/// The body of `POST /v1/slots/{name}/take`.
#[derive(Deserialize)]
pub struct TakeBody {
    session_id: Option<u64>,
    purpose: Option<String>,
    item: Option<u64>,
    mode: Option<String>,
    workflow: Option<String>,
    sha: Option<String>,
}

#[derive(Serialize)]
struct TakeAnswer {
    /// `taken`, `renewed` or `queued`.
    outcome: &'static str,
    /// The caller's place in the queue, from 1, when queued.
    position: Option<usize>,
    /// The run whose heartbeats keep the hold, for `mode: run`.
    run_id: Option<RunId>,
    slot: SlotView,
}

/// Takes the slot for the caller: a free slot, or one on offer to them, is theirs; their own hold
/// is renewed (up to `max_hold` from now, and while somebody waits no further than twice
/// `max_hold` from when it was taken); otherwise they join its group's queue, once. `mode` is
/// `session` (the default: kept by the member's heartbeats), `run` (kept by the heartbeats of
/// `POST …/heartbeat` with the `run_id` answered) or `until_run` with `workflow` and `sha`.
///
/// # Errors
/// `unknown_slot` (`404`); `holds_other_slot` (`409`) when the caller holds another slot of the
/// group; `not_found` for a session or item not theirs; `forbidden` for an integration;
/// `invalid_request` for a missing or invalid field.
pub async fn take(
    State(state): State<AppState>,
    Authed(actor): Authed,
    reads: ClientReads,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<TakeBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let slot = slot_in_path(path)?;
    let Json(body) = body?;
    let mode = match body.mode.as_deref().unwrap_or("session") {
        "session" => HoldMode::Session,
        "run" => HoldMode::Run,
        "until_run" => HoldMode::UntilRun {
            workflow: parse_required("workflow", body.workflow.as_deref(), WorkflowName::parse)?,
            sha: parse_required("sha", body.sha.as_deref(), CommitSha::parse)?,
        },
        _ => {
            return Err(ApiError::invalid_field(
                "mode",
                &"a mode is session, run or until_run",
            ));
        }
    };
    let input = TakeSlotInput {
        slot,
        session: session_in_body("session_id", body.session_id)?,
        purpose: parse_required("purpose", body.purpose.as_deref(), Purpose::parse)?,
        item: body
            .item
            .map(|item| item_in_body("item", Some(item)))
            .transpose()?,
        mode,
    };
    let took = state.app.take_slot.execute(&actor, input).await?;
    let (outcome, position) = match took.kind {
        TookKind::Taken => ("taken", None),
        TookKind::Renewed => ("renewed", None),
        TookKind::Queued { position } => ("queued", Some(position)),
    };
    Ok(reply(
        StatusCode::OK,
        &TakeAnswer {
            outcome,
            position,
            run_id: took.run,
            slot: SlotView::of(took.line, reads),
        },
    ))
}

/// The body of `POST /v1/slots/{name}/release`.
#[derive(Default, Deserialize)]
pub struct ReleaseBody {
    run_id: Option<u64>,
    exit: Option<RunExit>,
}

#[derive(Serialize)]
struct EndedAnswer {
    /// The hold that ended.
    released: HoldView,
    slot: SlotView,
}

/// The holder releases the slot; its group's first waiter is offered it. A run releasing the hold
/// it took names its `run_id` and may give its `exit` (`{"code": 3}` or `{"signal": "SIGTERM"}`);
/// then only that run's hold is released.
///
/// # Errors
/// `unknown_slot` (`404`); `slot_not_held` or `run_ended` (`409`); `not_holder` (`403`, naming
/// the holder); `forbidden` for an integration; `invalid_request` for an invalid field.
pub async fn release(
    State(state): State<AppState>,
    Authed(actor): Authed,
    reads: ClientReads,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<ReleaseBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let slot = slot_in_path(path)?;
    let Json(body) = body?;
    if matches!(body.exit, Some(RunExit::Conclusion(_))) {
        return Err(ApiError::invalid_field(
            "exit",
            &"an exit is a code or a signal",
        ));
    }
    let input = ReleaseSlotInput {
        slot,
        run: body.run_id.map(run_id).transpose()?,
        exit: body.exit,
    };
    let ended = state.app.release_slot.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::OK,
        &EndedAnswer {
            released: HoldView::of(ended.hold),
            slot: SlotView::of(ended.line, reads),
        },
    ))
}

/// The caller leaves the slot group's queue, or gives up the offer to them, which passes to the
/// next waiter.
///
/// # Errors
/// `unknown_slot` (`404`); `not_waiting` (`409`); `forbidden` for an integration.
pub async fn leave(
    State(state): State<AppState>,
    Authed(actor): Authed,
    reads: ClientReads,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let slot = slot_in_path(path)?;
    let line = state.app.leave_slot_queue.execute(&actor, &slot).await?;
    Ok(reply(
        StatusCode::OK,
        &SlotAnswer {
            slot: SlotView::of(line, reads),
        },
    ))
}

/// The body of `POST /v1/slots/{name}/heartbeat`.
#[derive(Deserialize)]
pub struct BeatBody {
    run_id: Option<u64>,
}

/// The heartbeat of the run that holds the slot: its hold is seen now, and a stalled hold is live
/// again.
///
/// # Errors
/// `unknown_slot` (`404`); `taken_over` (`409`, naming who holds it now) or `run_ended` (`409`)
/// when the run's hold is gone; `forbidden` for an integration; `invalid_request` without a
/// `run_id`.
pub async fn heartbeat(
    State(state): State<AppState>,
    Authed(actor): Authed,
    reads: ClientReads,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<BeatBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let slot = slot_in_path(path)?;
    let Json(body) = body?;
    let run = run_id(required("run_id", body.run_id)?)?;
    let line = state.app.slot_run_beat.execute(&actor, &slot, run).await?;
    Ok(reply(
        StatusCode::OK,
        &SlotAnswer {
            slot: SlotView::of(line, reads),
        },
    ))
}

/// The body of `POST /v1/slots/{name}/takeover`.
#[derive(Deserialize)]
pub struct TakeOverBody {
    session_id: Option<u64>,
    purpose: Option<String>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct TakeOverAnswer {
    previous: HoldView,
    /// The thread that told the previous holder the reason.
    thread_id: ThreadId,
    slot: SlotView,
}

/// Takes over another member's stalled hold, telling them `reason` — what the caller checked —
/// in a thread of its own.
///
/// # Errors
/// `unknown_slot` (`404`); `slot_not_held`, `own_hold` or `not_stalled` (`409`, naming the
/// holder); `forbidden` for an integration; `invalid_request` for a missing or invalid field.
pub async fn take_over(
    State(state): State<AppState>,
    Authed(actor): Authed,
    reads: ClientReads,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<TakeOverBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let slot = slot_in_path(path)?;
    let Json(body) = body?;
    let input = TakeOverSlotInput {
        slot,
        session: session_in_body("session_id", body.session_id)?,
        purpose: parse_required("purpose", body.purpose.as_deref(), Purpose::parse)?,
        reason: parse_required("reason", body.reason.as_deref(), Body::parse)?,
    };
    let taken = state.app.take_over_slot.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::OK,
        &TakeOverAnswer {
            previous: HoldView::of(taken.previous),
            thread_id: taken.thread,
            slot: SlotView::of(taken.line, reads),
        },
    ))
}

/// The body of `POST /v1/slots/{name}/run-finished`.
#[derive(Deserialize)]
pub struct RunFinishedBody {
    workflow: Option<String>,
    sha: Option<String>,
    conclusion: Option<String>,
}

#[derive(Serialize)]
struct RunFinishedAnswer {
    /// Whether the report ended a hold; a repeat answers `false`.
    matched: bool,
}

/// An integration reports that a forge workflow run finished: the hold taken until exactly that
/// workflow's run for that commit ends, with the conclusion as its exit. A report that matches no
/// hold — a repeat included — changes nothing and answers `matched: false`.
///
/// # Errors
/// `forbidden` for anyone but the importer or an admin; `invalid_request` for a missing or
/// invalid field.
pub async fn run_finished(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<RunFinishedBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_import()?;
    let slot = slot_in_path(path)?;
    let Json(body) = body?;
    let input = RunFinishedInput {
        slot,
        workflow: parse_required("workflow", body.workflow.as_deref(), WorkflowName::parse)?,
        sha: parse_required("sha", body.sha.as_deref(), CommitSha::parse)?,
        conclusion: parse_required("conclusion", body.conclusion.as_deref(), EndWord::parse)?,
    };
    let matched = state.app.slot_run_finished.execute(&actor, input).await?;
    Ok(reply(StatusCode::OK, &RunFinishedAnswer { matched }))
}

/// The body of `POST /v1/admin/slots/{name}/force-release`.
#[derive(Deserialize)]
pub struct ForceBody {
    session_id: Option<u64>,
    reason: Option<String>,
}

#[derive(Serialize)]
struct ForcedAnswer {
    released: HoldView,
    /// The member who held it.
    holder: Handle,
    /// The thread that told the holder the reason; `null` when the admin held it.
    thread_id: Option<ThreadId>,
    slot: SlotView,
}

/// An admin ends any member's hold; the holder is sent `reason`, and the group's first waiter is
/// offered the slot.
///
/// # Errors
/// `forbidden` unless the caller is an admin; `unknown_slot` (`404`); `slot_not_held` (`409`);
/// `invalid_request` for a missing or invalid field.
pub async fn force_release(
    State(state): State<AppState>,
    Authed(actor): Authed,
    reads: ClientReads,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<ForceBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_admin()?;
    let slot = slot_in_path(path)?;
    let Json(body) = body?;
    let input = ForceReleaseSlotInput {
        slot,
        session: session_in_body("session_id", body.session_id)?,
        reason: parse_required("reason", body.reason.as_deref(), Body::parse)?,
    };
    let forced = state.app.force_release_slot.execute(&actor, input).await?;
    Ok(reply(
        StatusCode::OK,
        &ForcedAnswer {
            holder: forced.ended.hold.holder.clone(),
            released: HoldView::of(forced.ended.hold),
            thread_id: forced.thread,
            slot: SlotView::of(forced.ended.line, reads),
        },
    ))
}

fn run_id(number: u64) -> Result<RunId, ApiError> {
    let text = number.to_string();
    parse("run_id", &text, record_id_text).map(RunId)
}

/// The body of `POST /v1/slots/{name}/offer/deliver`.
#[derive(Deserialize)]
pub struct OfferDeliverBody {
    session_id: Option<u64>,
    until: Option<String>,
}

/// Claims the delivery of the slot's offer to the caller, ending at `until` (RFC 3339, as the
/// inbox gave it), to the agent of their session `session_id`: the first of the caller's sessions
/// to claim it shows it, answered `201` with the delivery; every later claim is refused. A new
/// offer after one lapsed has another deadline and is claimed anew.
///
/// # Errors
/// `forbidden` for an integration; `not_found` for a session that is not the caller's;
/// `offer_gone` (`409`) when the slot is not on offer to the caller until that time, after what
/// time changed is applied; `delivered` (`409`) when it was claimed before; `invalid_request` for
/// a missing `session_id` or a missing or invalid `until`.
pub async fn deliver_offer(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<OfferDeliverBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let slot = slot_in_path(path)?;
    let Json(body) = body?;
    let session = session_in_body("session_id", body.session_id)?;
    let until = parse_required("until", body.until.as_deref(), |text| {
        OffsetDateTime::parse(text, &Rfc3339).map_err(|_| "it is a time in RFC 3339")
    })?;
    let delivery = state
        .app
        .claim_delivery
        .execute(&actor, session, DeliveryAsk::Offer { slot, until })
        .await?;
    Ok(reply(
        StatusCode::CREATED,
        &crate::routes::messages::DeliveredAnswer {
            delivery: DeliveryView::of(&delivery),
        },
    ))
}
