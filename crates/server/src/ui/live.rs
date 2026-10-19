//! What keeps a page live: the event stream over the browser's session cookie, the check a page's
//! script makes to learn whether its session ended, and the position in the log a page was
//! rendered at, from which its script follows the stream.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use dibs_app::Actor;
use dibs_app::use_cases::EventCursor;

use crate::error::ApiError;
use crate::http::AppState;
use crate::routes::events::{StreamQuery, stream_from};
use crate::ui::session::ScriptSession;

/// `GET /ui/events/stream`: the project's log as server-sent events for the signed-in browser,
/// exactly as `GET /v1/events/stream` sends it to the same member, from the `Last-Event-ID` header,
/// else `?after=`, else from now on. The stream ends at its next read once the browser signs out
/// or its session ends.
///
/// # Errors
/// `unauthorized` without a live session; the refusals of the bearer-token stream otherwise.
pub async fn stream(
    State(state): State<AppState>,
    ScriptSession(session): ScriptSession,
    headers: HeaderMap,
    query: Result<Query<StreamQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query?;
    let from = stream_from(&headers, &query)?;
    state
        .streams
        .open_for_browser(
            state.app.clone(),
            session.actor().clone(),
            from,
            session.secret().to_owned(),
        )
        .await
}

/// `GET /ui/session`: `204` while the browser's session lives, `401` once it ended.
pub async fn session(ScriptSession(_): ScriptSession) -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// The id of the newest event of the actor's log, or 0 while it is empty: what a page read the log
/// up to before it read what it shows.
///
/// # Errors
/// The answer for the use case's failure.
pub async fn latest_event(state: &AppState, actor: &Actor) -> Result<u64, ApiError> {
    let page = state
        .app
        .list_events
        .execute(actor, EventCursor::Latest, 1)
        .await?;
    Ok(page.next.map_or(0, |id| id.0))
}
