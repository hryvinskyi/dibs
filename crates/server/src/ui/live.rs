//! What keeps a page live: the event stream over the browser's session cookie, the check a page's
//! script makes to learn whether its session ended, and the position in the log a page was
//! rendered at, from which its script follows the stream.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use serde::Deserialize;

use dibs_app::use_cases::EventCursor;
use dibs_app::{Actor, AppError};

use crate::error::ApiError;
use crate::fields::event_cursor;
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

/// The query of `GET /ui/session`.
#[derive(Deserialize)]
pub struct SessionQuery {
    after: Option<String>,
}

/// `GET /ui/session`: `204` while the browser's session lives, `401` once it ended. With
/// `?after=<cursor>`, the cursor the page follows the stream from, a cursor behind the events the
/// retention removed is answered `410 cursor_expired` with `details.latest`, the newest event now,
/// so the page reads its state again and follows the stream from there instead of asking for what
/// no longer exists, again and again.
///
/// # Errors
/// `unauthorized` without a live session; `invalid_query` for a cursor that is none;
/// `cursor_expired` as above.
pub async fn session(
    State(state): State<AppState>,
    ScriptSession(session): ScriptSession,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query?;
    if let Some(text) = query.after.as_deref() {
        let cursor = event_cursor(text).map_err(|rule| ApiError::invalid_field("after", &rule))?;
        let actor = session.actor();
        match state.app.list_events.execute(actor, cursor, 1).await {
            Ok(_) => {}
            Err(error @ AppError::CursorExpired { .. }) => {
                let latest = latest_event(&state, actor).await?;
                return Err(ApiError::from(error).with_detail("latest", latest));
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(StatusCode::NO_CONTENT.into_response())
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
