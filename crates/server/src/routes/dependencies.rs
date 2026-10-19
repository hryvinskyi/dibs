//! `POST` and `DELETE /v1/items/{id}/dependencies`: a person recording or removing that the item
//! is blocked by another. Both items are named by local id.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{DependencyChange, DependencyInput};
use dibs_domain::ids::ItemId;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_body, item_in_path, parse_required, record_id_text};
use crate::http::AppState;
use crate::views::{ItemView, reply};

/// The body of `POST /v1/items/{id}/dependencies`.
#[derive(Deserialize)]
pub struct AddBody {
    blocked_by: Option<u64>,
}

/// The query of `DELETE /v1/items/{id}/dependencies`.
#[derive(Deserialize)]
pub struct RemoveQuery {
    blocked_by: Option<String>,
}

#[derive(Serialize)]
struct ChangeAnswer {
    item: ItemView,
    blocked_by: ItemView,
    /// Whether the call changed the work graph.
    changed: bool,
}

impl ChangeAnswer {
    fn of(change: DependencyChange) -> Self {
        Self {
            item: ItemView::of(change.item),
            blocked_by: ItemView::of(change.blocked_by),
            changed: change.changed,
        }
    }
}

/// Records that the item is blocked by `blocked_by`: `200` with `changed: false` when that edge was
/// already there. The edge is the person's: an import never removes it.
///
/// # Errors
/// `forbidden` for an integration; `dependency_cycle` (`409`) for an edge that would make the two
/// items wait for each other, `self_dependency` for an item blocked by itself; `not_found`;
/// `invalid_request` for a missing field.
pub async fn add(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<AddBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let Json(body) = body?;
    let input = DependencyInput {
        item,
        blocked_by: item_in_body("blocked_by", body.blocked_by)?,
    };
    let change = state.app.add_dependency.execute(&actor, input).await?;
    Ok(reply(StatusCode::OK, &ChangeAnswer::of(change)))
}

/// Removes the edge from the item to `blocked_by`, whoever recorded it: `200` with `changed: false`
/// when there was none.
///
/// # Errors
/// `forbidden` for an integration; `not_found`; `invalid_request` for a missing or malformed
/// `blocked_by`.
pub async fn remove(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RemoveQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let Query(query) = query?;
    let blocked_by = parse_required("blocked_by", query.blocked_by.as_deref(), record_id_text)?;
    let input = DependencyInput {
        item,
        blocked_by: ItemId(blocked_by),
    };
    let change = state.app.remove_dependency.execute(&actor, input).await?;
    Ok(reply(StatusCode::OK, &ChangeAnswer::of(change)))
}
