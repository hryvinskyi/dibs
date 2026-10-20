//! `POST /v1/items/{id}/claim`, `/claim/narrow`, `/takeover`, `/release` and `/done`: taking,
//! narrowing, taking over and ending a claim on a work item.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::named::{EndedClaim, NamedClaim};
use dibs_app::use_cases::{
    Announced, ClaimItemInput, NarrowClaimInput, PendingNote, TakeOverInput, WideInput, WideNote,
};
use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::ClaimOutcome;
use dibs_domain::handle::Handle;
use dibs_domain::handover::ClaimKind;
use dibs_domain::ids::{ItemId, MessageId, ThreadId};
use dibs_domain::message::Body;
use dibs_domain::wide::WideReason;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_path, parse, parse_required, required, session_in_body};
use crate::http::AppState;
use crate::views::{ClaimView, ItemNameView, reply};

/// The body of `POST /v1/items/{id}/claim`.
#[derive(Deserialize)]
pub struct ClaimBody {
    session_id: Option<u64>,
    areas: Option<Vec<String>>,
    wide: Option<WideBody>,
    kind: Option<String>,
}

/// The `wide` object of a claim's body.
#[derive(Deserialize)]
pub struct WideBody {
    areas: Option<Vec<String>>,
    reason: Option<String>,
}

impl WideBody {
    /// The scope as the use case takes it.
    fn input(self) -> Result<WideInput, ApiError> {
        Ok(WideInput {
            areas: required("wide.areas", self.areas)?,
            reason: parse_required("wide.reason", self.reason.as_deref(), WideReason::parse)?,
        })
    }
}

/// An announcement a wide claim sent.
#[derive(Serialize)]
struct AnnouncedView {
    /// The holder asked.
    to: Handle,
    /// The areas of theirs it asks about.
    areas: BTreeSet<QualifiedArea>,
    /// Its message, which the holder acknowledges.
    message_id: MessageId,
    /// Its thread.
    thread_id: ThreadId,
}

impl AnnouncedView {
    fn of(announced: Announced) -> Self {
        Self {
            to: announced.to,
            areas: announced.ask.areas,
            message_id: announced.ask.message,
            thread_id: announced.thread,
        }
    }
}

/// An area the claim holds that another member's live wide claim names.
#[derive(Serialize)]
struct InsideWideView {
    area: QualifiedArea,
    /// The wide claim's member.
    holder: Handle,
    /// The wide claim's item.
    item: ItemNameView,
    /// The announcement that asks the caller about the area, when one waits for them.
    message_id: Option<MessageId>,
}

impl InsideWideView {
    fn of(note: WideNote) -> Self {
        Self {
            area: note.area,
            holder: note.holder,
            item: ItemNameView::of(note.item),
            message_id: note.message,
        }
    }
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
    /// For a wide claim, the announcements it sent to the holders of its areas.
    announced: Vec<AnnouncedView>,
    /// The areas the claim holds that other members' live wide claims name.
    inside_wide: Vec<InsideWideView>,
    /// The areas the claim waits for another member's debt claim to hand over.
    pending: Vec<PendingNoteView>,
}

/// An area the claim waits for.
#[derive(Serialize)]
struct PendingNoteView {
    area: QualifiedArea,
    /// The debt claim's holder.
    holder: Handle,
    /// The debt claim's item.
    item: ItemNameView,
    /// Whether the holder was asked to hand it over; not while their claim is in review.
    asked: bool,
}

impl PendingNoteView {
    fn of(note: PendingNote) -> Self {
        Self {
            area: note.area,
            holder: note.holder,
            item: ItemNameView::of(note.item),
            asked: note.asked,
        }
    }
}

/// Claims the item for the caller, or renews the caller's own claim. `areas` are named as a person
/// names them: `repo:area`, or a bare name exactly one repository declares. A blocked item is
/// claimed all the same; `blocked_by` names what it waits for. With `wide: {areas, reason}` the
/// claim is wide: it holds none of those areas, and each holder of one is sent an announcement to
/// acknowledge, which `announced` lists; a renewal adds to the scope it had. `kind` is `feature`
/// (the default) or `debt`, for a new claim; a renewal keeps the kind it has. A feature claim over
/// an area other members hold only under debt claims waits for it, which `pending` lists; a
/// renewal naming another kind than the claim has is refused.
///
/// # Errors
/// `held`, `stale`, `area_held` or `wide_overlap` (`409`, naming the member in the way; an area
/// another claim waits for is `area_held` with `waiting`); `kind_fixed` (`409`);
/// `wide_and_held`, `invalid_wide`, `item_closed`, `unknown_area`, `invalid_area`,
/// `ambiguous_area`, `not_found`, `forbidden` for an integration; `invalid_request` for a malformed
/// body.
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
        wide: body.wide.map(WideBody::input).transpose()?,
        kind: kind_in_body(body.kind.as_deref())?,
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
        announced: made.announced.into_iter().map(AnnouncedView::of).collect(),
        inside_wide: made
            .inside_wide
            .into_iter()
            .map(InsideWideView::of)
            .collect(),
        pending: made.pending.into_iter().map(PendingNoteView::of).collect(),
    };
    Ok(reply(StatusCode::OK, &answer))
}

/// The body of `POST /v1/items/{id}/claim/narrow`.
#[derive(Deserialize)]
pub struct NarrowBody {
    areas: Option<Vec<String>>,
}

/// An area a narrowing handed over.
#[derive(Serialize)]
struct HandedOverView {
    area: QualifiedArea,
    /// The item of the claim that holds it now.
    to: ItemNameView,
    /// That claim's holder.
    holder: Handle,
}

#[derive(Serialize)]
struct NarrowAnswer {
    /// The claim as it stands now.
    claim: ClaimView,
    /// The item.
    item: ItemNameView,
    /// The areas it gave up.
    areas: BTreeSet<QualifiedArea>,
    /// The areas given up that another claim waited for, each with where it went.
    handed_over: Vec<HandedOverView>,
}

/// Gives `areas` of the caller's claim up, live or stale: the claim neither holds nor waits for
/// them any more, and an area another claim waited for is handed over to it in the same
/// transaction. An area the claim neither holds nor waits for changes nothing, so a repeat answers
/// the same claim and gives nothing up.
///
/// # Errors
/// `forbidden` for an integration; `not_holder` (`403`, naming the holder), `no_claim` (`409`),
/// `unknown_area`, `invalid_area`, `ambiguous_area`, `not_found`; `invalid_request` for a missing
/// `areas`.
pub async fn narrow(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<NarrowBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let item = path_item(path)?;
    let Json(body) = body?;
    let input = NarrowClaimInput {
        item,
        areas: required("areas", body.areas)?,
    };
    let narrowed = state.app.narrow_claim.execute(&actor, input).await?;
    let answer = NarrowAnswer {
        claim: ClaimView::of(narrowed.claim),
        item: ItemNameView::of(narrowed.item),
        areas: narrowed.areas,
        handed_over: narrowed
            .handed_over
            .into_iter()
            .map(|handed| HandedOverView {
                area: handed.area,
                to: ItemNameView::of(handed.to),
                holder: handed.holder,
            })
            .collect(),
    };
    Ok(reply(StatusCode::OK, &answer))
}

/// The body of `POST /v1/items/{id}/takeover`.
#[derive(Deserialize)]
pub struct TakeOverBody {
    session_id: Option<u64>,
    reason: Option<String>,
    areas: Option<Vec<String>>,
    kind: Option<String>,
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
    /// The announcements the carried-on wide scope sent, in the caller's name.
    announced: Vec<AnnouncedView>,
}

/// The `kind` of a body, when it names one.
fn kind_in_body(text: Option<&str>) -> Result<Option<ClaimKind>, ApiError> {
    text.map(|text| parse("kind", text, ClaimKind::parse))
        .transpose()
}

/// Takes over another member's stale claim; the previous holder is sent `reason` in a thread about
/// the item. `kind` names the new claim's kind; without it, the previous claim's kind carries on.
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
        kind: kind_in_body(body.kind.as_deref())?,
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
        announced: taken.announced.into_iter().map(AnnouncedView::of).collect(),
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
