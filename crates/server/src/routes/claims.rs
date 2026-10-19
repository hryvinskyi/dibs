//! `POST /v1/items/{id}/claim`, `/takeover`, `/release` and `/done`: taking, taking over and ending
//! a claim on a work item.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::named::{EndedClaim, NamedClaim};
use dibs_app::use_cases::{ClaimItemInput, TakeOverInput};
use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::ClaimOutcome;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::message::Body;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_path, parse_required, session_in_body};
use crate::http::AppState;
use crate::views::{ClaimView, ItemNameView, reply};

/// The body of `POST /v1/items/{id}/claim`.
#[derive(Deserialize)]
pub struct ClaimBody {
    session_id: Option<u64>,
    areas: Option<Vec<String>>,
}

#[derive(Serialize)]
struct ClaimAnswer {
    /// `new` for a first claim, `renewed` when the caller already held the item.
    outcome: &'static str,
    claim: ClaimView,
    /// Areas a renewal no longer holds: another member holds them now, or the settings dropped
    /// them.
    dropped_areas: BTreeSet<QualifiedArea>,
    /// The items the claimed item waits for that are neither done nor closed: the claim is made,
    /// and the caller is warned.
    blocked_by: Vec<ItemNameView>,
    /// The item claimed.
    item: ItemNameView,
}

/// Claims the item for the caller, or renews the caller's own claim. `areas` are named as a person
/// names them: `repo:area`, or a bare name exactly one repository declares. A blocked item is
/// claimed all the same; `blocked_by` names what it waits for.
///
/// # Errors
/// `held`, `stale` or `area_held` (`409`, naming the holder) when another member is in the way;
/// `item_closed`, `unknown_area`, `invalid_area`, `ambiguous_area`, `not_found`, `forbidden` for an
/// integration; `invalid_request` for a malformed body.
pub async fn claim(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<ClaimBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let Json(body) = body?;
    let input = ClaimItemInput {
        item,
        session: session_in_body("session_id", body.session_id)?,
        areas: body.areas.unwrap_or_default(),
    };
    let made = state.app.claim_item.execute(&actor, input).await?;
    let (name, claim, dropped_areas) = match made.outcome {
        ClaimOutcome::New { claim } => ("new", claim, BTreeSet::new()),
        ClaimOutcome::Renewed {
            claim,
            dropped_areas,
        } => ("renewed", claim, dropped_areas),
    };
    let answer = ClaimAnswer {
        outcome: name,
        claim: ClaimView::of(NamedClaim {
            claim,
            holder: made.holder,
        }),
        dropped_areas,
        blocked_by: made.blocked_by.into_iter().map(ItemNameView::of).collect(),
        item: ItemNameView::of(made.item),
    };
    Ok(reply(StatusCode::OK, &answer))
}

/// The body of `POST /v1/items/{id}/takeover`.
#[derive(Deserialize)]
pub struct TakeOverBody {
    session_id: Option<u64>,
    reason: Option<String>,
    areas: Option<Vec<String>>,
}

#[derive(Serialize)]
struct TakeOverAnswer {
    claim: ClaimView,
    previous_holder: Handle,
    dropped_areas: BTreeSet<QualifiedArea>,
    /// The thread that told the previous holder the reason.
    thread_id: ThreadId,
    /// The item taken over.
    item: ItemNameView,
}

/// Takes over another member's stale claim; the previous holder is sent `reason` in a thread about
/// the item.
///
/// # Errors
/// `not_stale` (naming the holder), `own_claim`, `nothing_to_take`, `area_held` (`409`);
/// `item_closed`, area refusals, `not_found`, `forbidden` for an integration; `invalid_request`
/// for a missing or invalid reason.
pub async fn take_over(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<TakeOverBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let Json(body) = body?;
    let input = TakeOverInput {
        item,
        session: session_in_body("session_id", body.session_id)?,
        areas: body.areas.unwrap_or_default(),
        reason: parse_required("reason", body.reason.as_deref(), Body::parse)?,
    };
    let taken = state.app.take_over.execute(&actor, input).await?;
    let answer = TakeOverAnswer {
        previous_holder: taken.previous_holder,
        claim: ClaimView::of(NamedClaim {
            claim: taken.takeover.claim,
            holder: taken.holder,
        }),
        dropped_areas: taken.takeover.dropped_areas,
        thread_id: taken.thread,
        item: ItemNameView::of(taken.item),
    };
    Ok(reply(StatusCode::OK, &answer))
}

#[derive(Serialize)]
struct EndedAnswer {
    /// The claim that ended.
    claim: ClaimView,
    /// The item it was on.
    item: ItemNameView,
}

/// Releases the caller's claim, live or stale; the item is open again.
///
/// # Errors
/// `forbidden` for an integration;
/// `not_holder` (`403`, naming the holder), `no_claim` (`409`), `not_found`.
pub async fn release(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let item = path_item(path)?;
    let ended = state.app.release.execute(&actor, item).await?;
    Ok(answer_ended(ended))
}

/// Completes the caller's claim; the item is done.
///
/// # Errors
/// `forbidden` for an integration;
/// `not_holder` (`403`, naming the holder), `no_claim` (`409`), `not_found`.
pub async fn done(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let item = path_item(path)?;
    let ended = state.app.complete.execute(&actor, item).await?;
    Ok(answer_ended(ended))
}

fn path_item(path: Result<Path<String>, PathRejection>) -> Result<ItemId, ApiError> {
    let Path(id) = path?;
    item_in_path(&id)
}

fn answer_ended(ended: EndedClaim) -> Response {
    let answer = EndedAnswer {
        claim: ClaimView::of(ended.claim),
        item: ItemNameView::of(ended.item),
    };
    reply(StatusCode::OK, &answer)
}
