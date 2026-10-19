//! `GET /v1/events` and `GET /v1/events/stream`: the project's event log, page by page or as it
//! grows.

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::{Deserialize, Serialize};

use dibs_app::use_cases::{EventCursor, MAX_PAGE};
use dibs_domain::ids::EventId;

use crate::auth::Authed;
use crate::error::ApiError;
use crate::fields::{event_cursor, parse};
use crate::http::AppState;
use crate::views::{EventView, reply};

/// The header a reconnecting `EventSource` sends with the id of the last event it received.
pub const LAST_EVENT_ID: &str = "last-event-id";

/// The query of `GET /v1/events`.
#[derive(Deserialize)]
pub struct ListQuery {
    after: Option<String>,
    limit: Option<String>,
}

#[derive(Serialize)]
struct EventsAnswer {
    events: Vec<EventView>,
    /// The cursor to pass as `after` for the next page; `null` while the log is empty.
    next: Option<EventId>,
}

/// One page of the project's log, oldest first: the events after `after` (from the first when it
/// is absent or `0`), at most `limit` of them (1 to 500, 500 when absent). Every event carries ids
/// and the handles of the members it names, never a subject or a message's text.
///
/// # Errors
/// `invalid_request` for an `after` that is not a cursor or a `limit` that is not a number from 1
/// to 500; `invalid_query` for a malformed query string.
pub async fn list(
    State(state): State<AppState>,
    Authed(actor): Authed,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query?;
    let from = query
        .after
        .as_deref()
        .map(|text| parse("after", text, event_cursor))
        .transpose()?
        .unwrap_or(EventCursor::Start);
    let limit = query
        .limit
        .as_deref()
        .map(|text| parse("limit", text, page_size))
        .transpose()?
        .unwrap_or(MAX_PAGE);
    let page = state.app.list_events.execute(&actor, from, limit).await?;
    let answer = EventsAnswer {
        events: page.events.into_iter().map(EventView::of).collect(),
        next: page.next,
    };
    Ok(reply(StatusCode::OK, &answer))
}

/// A page size as a client writes it.
fn page_size(text: &str) -> Result<usize, &'static str> {
    text.trim()
        .parse::<usize>()
        .ok()
        .filter(|size| (1..=MAX_PAGE).contains(size))
        .ok_or("a limit is a whole number from 1 to 500")
}

/// The query of `GET /v1/events/stream`.
#[derive(Deserialize)]
pub struct StreamQuery {
    after: Option<String>,
}

/// The project's log as server-sent events, from where the client left off: after the
/// `Last-Event-ID` header's event when the header is present, else after `after`, else from the
/// newest event on, with nothing replayed. Each event's `id` is its id in the log and its `data`
/// the event as `GET /v1/events` writes it; a `message_posted` event the caller may read also
/// carries the message. Events about a conversation the caller may not read are left out.
///
/// # Errors
/// `forbidden` for an integration; `invalid_request` for a cursor that is not one;
/// `too_many_streams` (`503`) when the service has as many streams open as it allows.
pub async fn stream(
    State(state): State<AppState>,
    Authed(actor): Authed,
    headers: HeaderMap,
    query: Result<Query<StreamQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    actor.require_person()?;
    let Query(query) = query?;
    let from = stream_from(&headers, &query)?;
    state.streams.open(state.app.clone(), actor, from).await
}

/// Where a stream asked for with these headers and this query starts; see [`stream_start`].
///
/// # Errors
/// `invalid_request` naming the header or `after` when it is not a cursor.
pub fn stream_from(headers: &HeaderMap, query: &StreamQuery) -> Result<EventCursor, ApiError> {
    let header = match headers.get(LAST_EVENT_ID) {
        Some(value) => Some(value.to_str().map_err(|_| {
            ApiError::invalid_field("Last-Event-ID", &"a cursor is written in ASCII digits")
        })?),
        None => None,
    };
    stream_start(header, query.after.as_deref())
}

/// Where a stream starts: the header wins over the query; with neither, at the newest event. An
/// empty header counts as absent, as `EventSource` sends none before it received an id.
///
/// # Errors
/// `invalid_request` naming the header or `after` when it is not a cursor.
pub fn stream_start(header: Option<&str>, after: Option<&str>) -> Result<EventCursor, ApiError> {
    match (header.filter(|text| !text.trim().is_empty()), after) {
        (Some(text), _) => parse("Last-Event-ID", text, event_cursor),
        (None, Some(text)) => parse("after", text, event_cursor),
        (None, None) => Ok(EventCursor::Latest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_wins_and_nothing_means_from_now_on() {
        assert_eq!(
            stream_start(Some("7"), Some("3")).unwrap(),
            EventCursor::After(EventId(7))
        );
        assert_eq!(
            stream_start(None, Some("3")).unwrap(),
            EventCursor::After(EventId(3))
        );
        assert_eq!(stream_start(None, None).unwrap(), EventCursor::Latest);
        assert_eq!(stream_start(Some(""), None).unwrap(), EventCursor::Latest);
        assert_eq!(
            stream_start(Some(" "), Some("0")).unwrap(),
            EventCursor::Start
        );
    }

    #[test]
    fn a_bad_cursor_names_where_it_came_from() {
        let header = stream_start(Some("x"), Some("3")).unwrap_err();
        assert_eq!(header.code(), "invalid_request");
        assert_eq!(header.details()["field"], "Last-Event-ID");
        let query = stream_start(None, Some("-1")).unwrap_err();
        assert_eq!(query.details()["field"], "after");
    }

    #[test]
    fn a_page_holds_one_to_five_hundred() {
        assert_eq!(page_size("1"), Ok(1));
        assert_eq!(page_size("500"), Ok(500));
        for text in ["0", "501", "-1", "ten", ""] {
            assert!(page_size(text).is_err(), "{text:?}");
        }
    }
}
