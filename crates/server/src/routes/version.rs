//! `GET /v1/version`: the server's version, the oldest client it serves and the client it
//! recommends. A client of any version may ask, so one too old for every other route still learns
//! what to update to.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::http::AppState;
use crate::views::reply;

#[derive(Serialize)]
struct VersionAnswer {
    server: &'static str,
    min_client: Option<String>,
    latest_client: Option<String>,
}

/// Answers the versions to any signed-in caller.
///
/// # Errors
/// `unauthorized` without a valid token.
pub async fn version(
    State(state): State<AppState>,
    Authed(_caller): Authed,
) -> Result<Response, ApiError> {
    let answer = VersionAnswer {
        server: env!("CARGO_PKG_VERSION"),
        min_client: state.versions.min().map(ToString::to_string),
        latest_client: state.versions.latest().map(ToString::to_string),
    };
    Ok(reply(StatusCode::OK, &answer))
}
