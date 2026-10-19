//! `POST /v1/grants` and `DELETE /v1/grants/{id}`: a holder letting another member edit inside an
//! area they hold, and withdrawing it.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::GrantInput;
use dibs_domain::handle::Handle;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{grant_in_path, item_in_body, parse_required, required};
use crate::http::AppState;
use crate::views::{GrantView, reply};

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
/// `not_grantor` (`403`); `not_found` for a grant the project does not have.
pub async fn revoke(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
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
