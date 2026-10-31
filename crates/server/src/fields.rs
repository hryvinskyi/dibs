//! Turning the raw fields of a request into the values the use cases take, each refusal naming the
//! field at fault.

use std::fmt::Display;

use dibs_app::AppError;
use dibs_app::error::Missing;
use dibs_app::use_cases::EventCursor;
use dibs_domain::ids::{EventId, GrantId, ItemId, MessageId, SessionId, ThreadId};

use crate::error::ApiError;

/// The largest number a record id may be: ids lie in `1..=i64::MAX`.
pub const MAX_RECORD_ID: u64 = i64::MAX.unsigned_abs();

/// The field's value, or the refusal saying it is required.
///
/// # Errors
/// [`ApiError::missing_field`] when it is absent.
pub fn required<T>(field: &str, value: Option<T>) -> Result<T, ApiError> {
    value.ok_or_else(|| ApiError::missing_field(field))
}

/// The field's text parsed by `parse`, or the refusal naming the field and the rule it broke.
///
/// # Errors
/// [`ApiError::invalid_field`] when `parse` refuses the text.
pub fn parse<T, E: Display>(
    field: &str,
    text: &str,
    parse: impl FnOnce(&str) -> Result<T, E>,
) -> Result<T, ApiError> {
    parse(text).map_err(|reason| ApiError::invalid_field(field, &reason))
}

/// The required field's text parsed by `parse`.
///
/// # Errors
/// As [`required`] and [`parse`].
pub fn parse_required<T, E: Display>(
    field: &str,
    text: Option<&str>,
    parse_text: impl FnOnce(&str) -> Result<T, E>,
) -> Result<T, ApiError> {
    parse(field, required(field, text)?, parse_text)
}

/// A number that can be a record's id: ids lie in `1..=i64::MAX`, so anything else names no record.
fn record_id(number: u64) -> Option<u64> {
    (1..=MAX_RECORD_ID).contains(&number).then_some(number)
}

/// The item a path segment names. Text that is not an id names no item.
///
/// # Errors
/// The not-found refusal for text that is not an id.
pub fn item_in_path(text: &str) -> Result<ItemId, ApiError> {
    text.parse::<u64>()
        .ok()
        .and_then(record_id)
        .map(ItemId)
        .ok_or_else(|| not_found_text(&format!("there is no work item '{text}'")))
}

/// The grant a path segment names. Text that is not an id names no grant.
///
/// # Errors
/// The not-found refusal for text that is not an id.
pub fn grant_in_path(text: &str) -> Result<GrantId, ApiError> {
    text.parse::<u64>()
        .ok()
        .and_then(record_id)
        .map(GrantId)
        .ok_or_else(|| not_found_text(&format!("there is no grant '{text}'")))
}

/// The thread a path segment names. Text that is not an id names no thread.
///
/// # Errors
/// The not-found refusal for text that is not an id.
pub fn thread_in_path(text: &str) -> Result<ThreadId, ApiError> {
    text.parse::<u64>()
        .ok()
        .and_then(record_id)
        .map(ThreadId)
        .ok_or_else(|| not_found_text(&format!("there is no thread '{text}'")))
}

/// The session a path segment names. Text that is not an id names none of the caller's sessions.
///
/// # Errors
/// The not-found refusal for text that is not an id.
pub fn session_in_path(text: &str) -> Result<SessionId, ApiError> {
    text.parse::<u64>()
        .ok()
        .and_then(record_id)
        .map(SessionId)
        .ok_or_else(|| not_found_text(&format!("there is no session '{text}'")))
}

/// The message a path names; text that is not an id names no message.
///
/// # Errors
/// The not-found refusal for text that is not an id.
pub fn message_in_path(text: &str) -> Result<MessageId, ApiError> {
    text.parse::<u64>()
        .ok()
        .and_then(record_id)
        .map(MessageId)
        .ok_or_else(|| not_found_text(&format!("there is no message '{text}'")))
}

/// A record id written in a query string or a header.
///
/// # Errors
/// The rule an id follows, for text that is not one.
pub fn record_id_text(text: &str) -> Result<u64, &'static str> {
    text.trim()
        .parse::<u64>()
        .ok()
        .and_then(record_id)
        .ok_or("an id is a whole number from 1 to 9223372036854775807")
}

/// A position in the event log as a client writes it: the id of the last event it has, or `0`
/// for none yet, so reading starts at the first event.
///
/// # Errors
/// The rule a cursor follows, for text that is not one.
pub fn event_cursor(text: &str) -> Result<EventCursor, &'static str> {
    const RULE: &str = "a cursor is the id of an event, or 0 for the start of the log";
    match text.trim().parse::<u64>() {
        Ok(0) => Ok(EventCursor::Start),
        Ok(number) => record_id(number)
            .map(|id| EventCursor::After(EventId(id)))
            .ok_or(RULE),
        Err(_) => Err(RULE),
    }
}

/// A yes-or-no switch of a query string: `true`/`1` or `false`/`0`.
///
/// # Errors
/// The rule a switch follows, for any other text.
pub fn switch(text: &str) -> Result<bool, &'static str> {
    match text {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err("a switch is true, false, 1 or 0"),
    }
}

/// The session a body names. A number outside the id range names none of the caller's sessions.
///
/// # Errors
/// [`ApiError::missing_field`] when absent; the not-found refusal for a number no id can have.
pub fn session_in_body(field: &str, value: Option<u64>) -> Result<SessionId, ApiError> {
    let number = required(field, value)?;
    record_id(number)
        .map(SessionId)
        .ok_or_else(|| AppError::NotFound(Missing::Session(SessionId(number))).into())
}

/// The item a body names. A number outside the id range names no item.
///
/// # Errors
/// [`ApiError::missing_field`] when absent; the not-found refusal for a number no id can have.
pub fn item_in_body(field: &str, value: Option<u64>) -> Result<ItemId, ApiError> {
    let number = required(field, value)?;
    record_id(number)
        .map(ItemId)
        .ok_or_else(|| AppError::NotFound(Missing::Item(ItemId(number))).into())
}

/// The conversation a body names. A number outside the id range names no conversation.
///
/// # Errors
/// [`ApiError::missing_field`] when absent; the not-found refusal for a number no id can have.
pub fn thread_in_body(field: &str, value: Option<u64>) -> Result<ThreadId, ApiError> {
    let number = required(field, value)?;
    record_id(number)
        .map(ThreadId)
        .ok_or_else(|| AppError::NotFound(Missing::Thread(ThreadId(number))).into())
}

fn not_found_text(message: &str) -> ApiError {
    ApiError::new(axum::http::StatusCode::NOT_FOUND, "not_found", message)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use dibs_domain::item::Title;

    use super::*;

    #[test]
    fn ids_outside_the_range_name_nothing() {
        assert_eq!(item_in_path("7").unwrap(), ItemId(7));
        assert_eq!(
            item_in_path(&i64::MAX.to_string()).unwrap(),
            ItemId(MAX_RECORD_ID)
        );
        for text in ["0", "9223372036854775808", "-1", "abc", "", "1.5"] {
            let error = item_in_path(text).unwrap_err();
            assert_eq!(
                (error.status(), error.code()),
                (StatusCode::NOT_FOUND, "not_found")
            );
        }
        assert!(grant_in_path("0").is_err());
        assert_eq!(grant_in_path("3").unwrap(), GrantId(3));
    }

    #[test]
    fn a_body_id_is_required_and_in_range() {
        assert_eq!(
            session_in_body("session_id", Some(4)).unwrap(),
            SessionId(4)
        );
        assert_eq!(
            session_in_body("session_id", None).unwrap_err().code(),
            "invalid_request"
        );
        assert_eq!(
            session_in_body("session_id", Some(0)).unwrap_err().code(),
            "not_found"
        );
        assert_eq!(
            item_in_body("item", Some(u64::MAX)).unwrap_err().code(),
            "not_found"
        );
    }

    #[test]
    fn a_cursor_is_an_event_id_or_zero() {
        assert_eq!(event_cursor("0"), Ok(EventCursor::Start));
        assert_eq!(event_cursor("12"), Ok(EventCursor::After(EventId(12))));
        assert_eq!(event_cursor(" 12 "), Ok(EventCursor::After(EventId(12))));
        assert_eq!(
            event_cursor(&i64::MAX.to_string()),
            Ok(EventCursor::After(EventId(MAX_RECORD_ID)))
        );
        for text in ["", "-1", "abc", "1.5", "9223372036854775808", "0x10"] {
            assert!(event_cursor(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn switches_and_query_ids_follow_their_rules() {
        assert_eq!(switch("true"), Ok(true));
        assert_eq!(switch("1"), Ok(true));
        assert_eq!(switch("false"), Ok(false));
        assert_eq!(switch("0"), Ok(false));
        assert!(switch("yes").is_err());
        assert_eq!(record_id_text("7"), Ok(7));
        assert!(record_id_text("0").is_err());
        assert_eq!(thread_in_path("5").unwrap(), ThreadId(5));
        assert_eq!(thread_in_path("x").unwrap_err().code(), "not_found");
        assert_eq!(message_in_path("9").unwrap(), MessageId(9));
        assert_eq!(session_in_path("4").unwrap(), SessionId(4));
        assert_eq!(session_in_path("0").unwrap_err().code(), "not_found");
        assert_eq!(session_in_path("me").unwrap_err().code(), "not_found");
        assert_eq!(message_in_path("0").unwrap_err().code(), "not_found");
    }

    #[test]
    fn a_refused_value_names_its_field_and_rule() {
        let error = parse_required("title", Some("   "), Title::parse).unwrap_err();
        assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            error.message(),
            "The field 'title' is not valid: a title is 1 to 200 characters with no control characters."
        );
        assert_eq!(error.details()["field"], "title");
        let missing = parse_required("title", None, Title::parse).unwrap_err();
        assert_eq!(missing.message(), "The field 'title' is required.");
    }
}
