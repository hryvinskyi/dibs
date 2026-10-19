//! `GET /v1/items`, `POST /v1/items`, `PUT /v1/items` and `GET /v1/items/{id}`: finding, creating,
//! importing and showing work items. Paths address an item by its local id only; a reference
//! travels in the query string or the body, since it holds `/` and `#`.

use axum::Json;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::de::IntoDeserializer;
use serde::de::value::StrDeserializer;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{CreateItemInput, ItemQuery, UpsertReport};
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;
use dibs_domain::import::{
    EdgeRefused, ImportBatch, ItemChange, Listing, PlannedItem, RefusedEdge, ReportedItem,
};
use dibs_domain::item::{
    ExternalRef, HintDropped, ItemStatus, Priority, SourceId, SourceStatus, Title,
};

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

/// The body of `PUT /v1/items`: a batch of an importer.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpsertBody {
    source: Option<String>,
    complete: Option<bool>,
    items: Option<Vec<ReportedBody>>,
}

/// One item of a batch.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedBody {
    #[serde(rename = "ref")]
    external_ref: Option<String>,
    title: Option<String>,
    closed: Option<bool>,
    blocker: Option<bool>,
    areas_hint: Option<Vec<String>>,
    priority: Option<u8>,
    source_status: Option<String>,
    blocked_by: Option<Vec<String>>,
}

impl ReportedBody {
    /// The item as the domain reads it, each refusal naming the field at fault by its place in the
    /// batch.
    fn reported(self, n: usize) -> Result<ReportedItem, ApiError> {
        let field = |name: &str| format!("items[{n}].{name}");
        let reference = parse_required(
            &field("ref"),
            self.external_ref.as_deref(),
            ExternalRef::parse,
        )?;
        let title = parse_required(&field("title"), self.title.as_deref(), Title::parse)?;
        let listing = if self.blocker.unwrap_or(false) {
            None
        } else {
            Some(Listing {
                areas_hint: self.areas_hint.unwrap_or_default(),
                priority: self
                    .priority
                    .map(|number| {
                        Priority::new(number)
                            .map_err(|reason| ApiError::invalid_field(&field("priority"), &reason))
                    })
                    .transpose()?
                    .unwrap_or_default(),
                source_status: self
                    .source_status
                    .as_deref()
                    .map(|text| parse(&field("source_status"), text, SourceStatus::parse))
                    .transpose()?,
                blocked_by: self
                    .blocked_by
                    .unwrap_or_default()
                    .iter()
                    .enumerate()
                    .map(|(m, text)| {
                        parse(
                            &format!("items[{n}].blocked_by[{m}]"),
                            text,
                            ExternalRef::parse,
                        )
                    })
                    .collect::<Result<_, _>>()?,
            })
        };
        Ok(ReportedItem {
            reference,
            title,
            closed: self.closed.unwrap_or(false),
            listing,
        })
    }
}

#[derive(Serialize)]
struct UpsertAnswer {
    items: Vec<UpsertedView>,
    closed: Vec<ItemId>,
    edges_added: usize,
    edges_removed: usize,
    refused_edges: Vec<RefusedEdgeView>,
}

#[derive(Serialize)]
struct UpsertedView {
    #[serde(rename = "ref")]
    external_ref: ExternalRef,
    /// `created`, `updated`, `unchanged`, `skipped_closed` or `refused`.
    outcome: &'static str,
    id: Option<ItemId>,
    dropped_hints: Vec<DroppedHintView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refusal: Option<RefusalView>,
}

#[derive(Serialize)]
struct DroppedHintView {
    area: String,
    reason: HintDropped,
}

#[derive(Serialize)]
struct RefusalView {
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
struct RefusedEdgeView {
    item: ExternalRef,
    blocked_by: ExternalRef,
    reason: &'static str,
    message: String,
}

impl UpsertAnswer {
    fn of(report: UpsertReport) -> Self {
        Self {
            items: report.items.into_iter().map(UpsertedView::of).collect(),
            closed: report.closed,
            edges_added: report.edges_added,
            edges_removed: report.edges_removed,
            refused_edges: report
                .refused_edges
                .into_iter()
                .map(RefusedEdgeView::of)
                .collect(),
        }
    }
}

impl UpsertedView {
    fn of(planned: PlannedItem) -> Self {
        let id = planned.change.id();
        let (outcome, refusal) = match planned.change {
            ItemChange::Create { .. } => ("created", None),
            ItemChange::Update { .. } => ("updated", None),
            ItemChange::Unchanged { .. } => ("unchanged", None),
            ItemChange::SkippedClosed => ("skipped_closed", None),
            ItemChange::Refused(refused) => {
                let error = ApiError::from(dibs_app::AppError::Create(refused));
                (
                    "refused",
                    Some(RefusalView {
                        code: error.code(),
                        message: error.message().to_owned(),
                    }),
                )
            }
        };
        Self {
            external_ref: planned.reference,
            outcome,
            id,
            dropped_hints: planned
                .dropped_hints
                .into_iter()
                .map(|dropped| DroppedHintView {
                    area: dropped.text,
                    reason: dropped.reason,
                })
                .collect(),
            refusal,
        }
    }
}

impl RefusedEdgeView {
    fn of(edge: RefusedEdge) -> Self {
        Self {
            reason: match edge.reason {
                EdgeRefused::UnknownBlocker => "unknown_blocker",
                EdgeRefused::SelfDependency => "self_dependency",
                EdgeRefused::Cycle => "dependency_cycle",
            },
            message: crate::error::sentence(&edge.reason.to_string()),
            item: edge.item,
            blocked_by: edge.blocked_by,
        }
    }
}

/// Creates and updates the items an importer reports, keyed by reference, and replaces each listed
/// item's imported dependencies: `200` with what happened to each item and each refused edge. A
/// batch equal to what is stored changes nothing. Only an admin and the importer may send one; the
/// role is checked before the body is read.
///
/// # Errors
/// `forbidden` for a member; `invalid_request` naming the field at fault; `batch_too_large`,
/// `invalid_batch` or `duplicate_ref` for a batch refused as a whole; the answer for the use case's
/// failure.
pub async fn upsert(
    State(state): State<AppState>,
    Authed(actor): Authed,
    body: Result<Json<UpsertBody>, JsonRejection>,
) -> Result<Response, ApiError> {
    actor.require_import()?;
    let Json(body) = body?;
    let source = body
        .source
        .as_deref()
        .map(|text| parse("source", text, SourceId::parse))
        .transpose()?;
    let items = body
        .items
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(n, item)| item.reported(n))
        .collect::<Result<Vec<_>, _>>()?;
    let batch = ImportBatch::new(source, body.complete.unwrap_or(false), items)
        .map_err(dibs_app::AppError::from)?;
    let report = state.app.upsert_items.execute(&actor, batch).await?;
    Ok(reply(StatusCode::OK, &UpsertAnswer::of(report)))
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
