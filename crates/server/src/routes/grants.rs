//! `POST /v1/grants`, `DELETE /v1/grants/{id}` and `POST /v1/grant-requests`: a holder letting
//! another member edit inside an area they hold, withdrawing it, and a member asking the holder
//! for it.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{AskGrantInput, GrantAsked, GrantInput};
use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::ids::ThreadId;

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

/// Withdraws a grant the caller gave.
///
/// # Errors
/// `forbidden` for an integration;
/// `not_grantor` (`403`); `not_found` for a grant the project does not have.
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
    /// The holder asked, or who granted; `null` for an area that is free or the caller's own.
    holder: Option<Handle>,
    /// The item of the holder's claim that holds the area; `null` as `holder` is.
    holder_item: Option<ItemNameView>,
    /// The conversation that asks the holder; `null` unless `asked`.
    thread_id: Option<ThreadId>,
    /// The caller's item.
    item: ItemNameView,
    /// The area, resolved.
    area: QualifiedArea,
}

/// Asks whoever holds `area` through a live claim for a grant there, for the caller's live claim
/// on `item`: dibs starts a conversation about the holder's item, addressed to the holder alone,
/// whose first message (`high`) says how to give the grant. Answers `201` with `outcome: "asked"`,
/// or `200` with `free` (no live claim holds the area), `own` (a claim of the caller's holds it) or
/// `granted` (the holder's grant to the caller is already in force).
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
    let (status, outcome, holder, holder_item, thread_id) = match result.outcome {
        GrantAsked::Asked {
            holder,
            holder_item,
            thread,
        } => (
            StatusCode::CREATED,
            "asked",
            Some(holder),
            Some(ItemNameView::of(holder_item)),
            Some(thread),
        ),
        GrantAsked::Free => (StatusCode::OK, "free", None, None, None),
        GrantAsked::Own => (StatusCode::OK, "own", None, None, None),
        GrantAsked::Granted {
            holder,
            holder_item,
        } => (
            StatusCode::OK,
            "granted",
            Some(holder),
            Some(ItemNameView::of(holder_item)),
            None,
        ),
    };
    let answer = AskAnswer {
        outcome,
        holder,
        holder_item,
        thread_id,
        item: ItemNameView::of(result.item),
        area: result.area,
    };
    Ok(reply(status, &answer))
}
