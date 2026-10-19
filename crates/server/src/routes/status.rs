//! `GET /v1/status`: who holds what right now.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::http::AppState;
use crate::views::{HeldItemView, ReadyView, reply};

#[derive(Serialize)]
struct StatusAnswer {
    live: Vec<HeldItemView>,
    stale: Vec<HeldItemView>,
    holds: Vec<HoldView>,
    ready: ReadyView,
}

#[derive(Serialize)]
struct HoldView {
    area: QualifiedArea,
    holder: Handle,
    item: ItemId,
}

/// The project's live claims, its stale claims, the areas the live ones hold, and the caller's ready
/// list, each ordered as the use case orders it.
///
/// # Errors
/// `forbidden` for an integration; the answer for the use case's failure.
pub async fn status(
    State(state): State<AppState>,
    Authed(actor): Authed,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let view = state.app.status.execute(&actor).await?;
    let answer = StatusAnswer {
        live: view.live.into_iter().map(HeldItemView::of).collect(),
        stale: view.stale.into_iter().map(HeldItemView::of).collect(),
        holds: view
            .holds
            .into_iter()
            .map(|hold| HoldView {
                holder: hold.holder,
                area: hold.area,
                item: hold.item,
            })
            .collect(),
        ready: ReadyView::of(view.ready),
    };
    Ok(reply(StatusCode::OK, &answer))
}
