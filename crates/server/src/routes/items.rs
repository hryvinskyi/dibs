//! `GET /v1/items`, `POST /v1/items` and `GET /v1/items/{id}`: finding, creating and showing work
//! items. Paths address an item by its local id only; a reference travels in the query string or
//! the body, since it holds `/` and `#`.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::de::IntoDeserializer;
use serde::de::value::StrDeserializer;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{CreateItemInput, ItemQuery};
use dibs_domain::handle::Handle;
use dibs_domain::item::{ExternalRef, ItemStatus, Title};

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{item_in_path, parse, parse_required};
use crate::http::AppState;
use crate::views::{ClaimView, ItemView, reply};

/// The query of `GET /v1/items`.
#[derive(Deserialize)]
pub struct ListQuery {
    status: Option<String>,
    holder: Option<String>,
    #[serde(rename = "ref")]
    external_ref: Option<String>,
}

#[derive(Serialize)]
struct ItemsAnswer {
    items: Vec<ItemView>,
}

/// Lists the caller's project's items, ordered by id. Every filter given must match: `status`
/// (`open`, `claimed`, `done`, `dropped`), `holder` (a handle; one no member has matches nothing)
/// and `ref` (an exact lookup of one reference).
///
/// # Errors
/// `invalid_query` or `invalid_request` for a malformed filter; the answer for the use case's
/// failure.
pub async fn list(
    State(state): State<AppState>,
    Authed(actor): Authed,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query?;
    let list = ItemQuery {
        status: query
            .status
            .as_deref()
            .map(|text| parse("status", text, parse_status))
            .transpose()?,
        holder: query
            .holder
            .as_deref()
            .map(|text| parse("holder", text, Handle::parse))
            .transpose()?,
        external_ref: query
            .external_ref
            .as_deref()
            .map(|text| parse("ref", text, ExternalRef::parse))
            .transpose()?,
    };
    let items = state.app.list_items.execute(&actor, list).await?;
    let answer = ItemsAnswer {
        items: items.into_iter().map(ItemView::of).collect(),
    };
    Ok(reply(StatusCode::OK, &answer))
}

/// A status by the name it has in every answer.
fn parse_status(text: &str) -> Result<ItemStatus, &'static str> {
    let names: StrDeserializer<'_, serde::de::value::Error> = text.into_deserializer();
    ItemStatus::deserialize(names).map_err(|_| "a status is open, claimed, done or dropped")
}

/// The body of `POST /v1/items`.
#[derive(Deserialize)]
pub struct CreateBody {
    #[serde(rename = "ref")]
    external_ref: Option<String>,
    title: Option<String>,
}

#[derive(Serialize)]
struct CreatedAnswer {
    item: ItemView,
    created: bool,
}

/// Creates a work item, or answers the one its reference already names: `201` with `created: true`
/// when this call created it, `200` with `created: false` when it existed. An item without `ref`
/// is local.
///
/// # Errors
/// `invalid_request` naming a missing or invalid field; the answer for the use case's refusal
/// (`unlisted_repository`) or failure.
pub async fn create(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<CreateBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = body?;
    let input = CreateItemInput {
        external_ref: body
            .external_ref
            .as_deref()
            .map(|text| parse("ref", text, ExternalRef::parse))
            .transpose()?,
        title: parse_required("title", body.title.as_deref(), Title::parse)?,
    };
    let created = state.app.create_item.execute(&actor, input).await?;
    let status = if created.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let answer = CreatedAnswer {
        item: ItemView::of(created.item),
        created: created.created,
    };
    Ok(reply(status, &answer))
}

#[derive(Serialize)]
struct ItemAnswer {
    item: ItemView,
    claim: Option<ClaimView>,
}

/// Shows one work item and its claim, live or stale. An item of another project is reported as
/// missing.
///
/// # Errors
/// `not_found` for an id the project has no item of; the answer for the use case's failure.
pub async fn get(
    State(state): State<AppState>,
    Authed(actor): Authed,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    let item = item_in_path(&id)?;
    let view = state.app.get_item.execute(&actor, item).await?;
    let answer = ItemAnswer {
        item: ItemView::of(view.item),
        claim: view.claim.map(ClaimView::of),
    };
    Ok(reply(StatusCode::OK, &answer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_status_has_its_name() {
        for (text, status) in [
            ("open", ItemStatus::Open),
            ("claimed", ItemStatus::Claimed),
            ("done", ItemStatus::Done),
            ("dropped", ItemStatus::Dropped),
        ] {
            assert_eq!(parse_status(text), Ok(status));
            assert_eq!(serde_json::to_value(status).unwrap(), text);
        }
        assert!(parse_status("Open").is_err());
    }
}
