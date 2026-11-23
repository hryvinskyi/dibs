//! `POST /v1/grants`, `DELETE /v1/grants/{id}` and `POST /v1/grant-requests`: a holder letting
//! another member edit inside an area they hold, its grantor or an admin withdrawing it, and a
//! member asking the holder for it, which dibs may grant on the holder's behalf.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{AskGrantInput, GrantAsked, GrantInput};
use dibs_domain::area::QualifiedArea;
use dibs_domain::grant::GrantDecision;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{GrantId, ThreadId};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{grant_in_path, item_in_body, parse_required, required};
use crate::http::AppState;
use crate::views::{GrantView, ItemNameView, reply};

/// The body of `POST /v1/grants`.
#[derive(Deserialize)]
pub struct GrantBody {
    item: Option<u64>,
    area: Option<String>,
    to: Option<String>,
}

#[derive(Serialize)]
struct GrantAnswer {
    grant: GrantView,
    created: bool,
    /// The grantee's item.
    item: ItemNameView,
}

/// Grants the member `to` edits inside `area`, which the caller holds, for `to`'s live claim on
/// `item` (the grantee's work item). `201` with `created: true` for a new grant, `200` with
/// `created: false` when the same grant exists.
///
/// # Errors
/// `not_holder` (`403`), `self_grant`, `grantee_has_no_claim`, area refusals, `not_found` for an
/// unknown item or grantee, `forbidden` for an integration; `invalid_request` for a malformed body.
pub async fn grant(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<GrantBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let input = GrantInput {
        item: item_in_body("item", body.item)?,
        area: required("area", body.area)?,
        to: parse_required("to", body.to.as_deref(), Handle::parse)?,
    };
    let result = state.app.grant.execute(&actor, input).await?;
    let status = if result.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let answer = GrantAnswer {
        grant: GrantView::of(result.grant),
        created: result.created,
        item: ItemNameView::of(result.item),
    };
    Ok(reply(status, &answer))
}

#[derive(Serialize)]
struct RevokedAnswer {
    grant: GrantView,
}

/// Withdraws a grant. Its grantor may, and so may an admin of the project for any grant, one dibs
/// gave on the holder's behalf included. dibs then never gives the same member that area on the
/// grantor's behalf again while both claims last.
///
/// # Errors
/// `forbidden` for an integration; `not_grantor` (`403`) for a member who neither gave it nor is
/// an admin; `not_found` for a grant the project does not have.
pub async fn revoke(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Path(id) = path?;
    let grant = grant_in_path(&id)?;
    let record = state.app.revoke_grant.execute(&actor, grant).await?;
    Ok(reply(
        StatusCode::OK,
        &RevokedAnswer {
            grant: GrantView::of(record),
        },
    ))
}

/// The body of `POST /v1/grant-requests`.
#[derive(Deserialize)]
pub struct AskBody {
    item: Option<u64>,
    area: Option<String>,
}

#[derive(Serialize)]
struct AskAnswer {
    /// `asked`, `free`, `own` or `granted`.
    outcome: &'static str,
    /// The holder asked, or whose grant is in force; `null` for an area that is free or the
    /// caller's own.
    holder: Option<Handle>,
    /// The item of the holder's claim that holds the area; `null` as `holder` is.
    holder_item: Option<ItemNameView>,
    /// The conversation that asks the holder: set when `asked`, and when dibs granted the request
    /// on the holder's behalf; `null` otherwise.
    thread_id: Option<ThreadId>,
    /// The grant dibs gave on the holder's behalf as it was asked; `null` otherwise, a grant the
    /// holder had given already included.
    grant_id: Option<GrantId>,
    /// Why dibs gave that grant (`untouched`, `in_review`, `idle`, `waited`); `null` as
    /// `grant_id` is.
    decided: Option<GrantDecision>,
    /// Whether dibs left the request to the holder for good, when `asked`: a revoke of the
    /// holder's grant to the caller in the area stands, so dibs never grants it on the holder's
    /// behalf. `false` for every other outcome.
    left_to_holder: bool,
    /// The caller's item.
    item: ItemNameView,
    /// The area, resolved.
    area: QualifiedArea,
}

/// Asks whoever holds `area` through a live claim for a grant there, for the caller's live claim
/// on `item`: dibs decides the request on the holder's behalf, then starts a conversation about
/// the holder's item, addressed to the holder alone, whose first message says what came of it.
/// When dibs grants at once, that message is `normal` and counts toward no limit, and the answer
/// is `201` with `outcome: "granted"`, the conversation, `grant_id` and `decided`. Otherwise the
/// first message (`high`) says how to give the grant, and the answer is `201` with
/// `outcome: "asked"` and `left_to_holder` saying whether a revoke of the holder's leaves the
/// request to them for good; or `200` with `free` (no live claim holds the area), `own` (a claim of
/// the caller's holds it) or `granted` (the holder's grant to the caller is already in force,
/// `grant_id` and `decided` `null`).
///
/// # Errors
/// `grantee_has_no_claim` (`409`) when the caller holds no live claim on `item`;
/// `importance_limited` (`429`); area refusals; `not_found` for an unknown item; `forbidden` for an
/// integration; `invalid_request` for a malformed body.
pub async fn ask(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<AskBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Json(body) = body?;
    let input = AskGrantInput {
        item: item_in_body("item", body.item)?,
        area: required("area", body.area)?,
    };
    let result = state.app.ask_grant.execute(&actor, input).await?;
    let item = ItemNameView::of(result.item);
    let area = result.area;
    let (status, answer) = match result.outcome {
        GrantAsked::Asked {
            holder,
            holder_item,
            thread,
            left_to_holder,
        } => (
            StatusCode::CREATED,
            AskAnswer {
                outcome: "asked",
                holder: Some(holder),
                holder_item: Some(ItemNameView::of(holder_item)),
                thread_id: Some(thread),
                grant_id: None,
                decided: None,
                left_to_holder,
                item,
                area,
            },
        ),
        GrantAsked::Free => (StatusCode::OK, AskAnswer::nobody("free", item, area)),
        GrantAsked::Own => (StatusCode::OK, AskAnswer::nobody("own", item, area)),
        GrantAsked::Granted {
            holder,
            holder_item,
            by_dibs,
        } => (
            if by_dibs.is_some() {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            AskAnswer {
                outcome: "granted",
                holder: Some(holder),
                holder_item: Some(ItemNameView::of(holder_item)),
                thread_id: by_dibs.map(|given| given.thread),
                grant_id: by_dibs.map(|given| given.grant),
                decided: by_dibs.map(|given| given.decided),
                left_to_holder: false,
                item,
                area,
            },
        ),
    };
    Ok(reply(status, &answer))
}

impl AskAnswer {
    /// The answer for an area no other member holds: nobody was asked and nothing was granted.
    const fn nobody(outcome: &'static str, item: ItemNameView, area: QualifiedArea) -> Self {
        Self {
            outcome,
            holder: None,
            holder_item: None,
            thread_id: None,
            grant_id: None,
            decided: None,
            left_to_holder: false,
            item,
            area,
        }
    }
}
