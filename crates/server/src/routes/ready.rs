//! `GET /v1/ready`: the work the caller can pick up now.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::http::AppState;
use crate::views::{ReadyView, reply};

/// The ready list as the caller sees it — open items nobody holds live, nothing blocks, and whose
/// hinted areas no other member holds, by priority and then oldest first — and beside it every
/// other open item with the reasons it is not ready. Every role may read it.
///
/// # Errors
/// The answer for the use case's failure.
pub async fn list(
    State(state): State<AppState>,
    Authed(actor): Authed,
) -> Result<Response, ApiError> {
    let view = state.app.ready.execute(&actor).await?;
    Ok(reply(StatusCode::OK, &ReadyView::of(view)))
}
