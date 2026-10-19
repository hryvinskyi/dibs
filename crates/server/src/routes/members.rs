//! `GET /v1/members`: the members of the caller's project.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use serde::Serialize;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::http::AppState;
use crate::views::{MemberView, reply};

#[derive(Serialize)]
struct MembersAnswer {
    members: Vec<MemberView>,
}

/// Lists the members of the caller's project, ordered by handle.
///
/// # Errors
/// The answer for the use case's failure.
pub async fn list(
    State(state): State<AppState>,
    Authed(actor): Authed,
) -> Result<Response, ApiError> {
    let members = state.app.list_members.execute(&actor).await?;
    let answer = MembersAnswer {
        members: members.iter().map(MemberView::of).collect(),
    };
    Ok(reply(StatusCode::OK, &answer))
}
