//! The viewer's UTC offset, as the page's script records it in the `dibs_tz` cookie: whole
//! minutes east of UTC, from -840 to 840. Anything else — no cookie, text, a fraction, a number
//! out of range — means the viewer's day is not known and UTC's is used.

use axum::http::HeaderMap;
use time::UtcOffset;

use crate::ui::cookies;

/// The cookie the script writes the offset into.
pub const ZONE_COOKIE: &str = "dibs_tz";

/// The furthest an offset lies from UTC, in minutes.
const MAX_OFFSET_MINUTES: i32 = 14 * 60;

/// The offset the request's cookie names, if it names one.
pub fn viewer_offset(headers: &HeaderMap) -> Option<UtcOffset> {
    cookies::read(headers, ZONE_COOKIE).and_then(parse_offset)
}

/// An offset in whole minutes written as an optional `-` and ASCII digits.
pub fn parse_offset(text: &str) -> Option<UtcOffset> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let minutes: i32 = text.parse().ok()?;
    if !(-MAX_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(&minutes) {
        return None;
    }
    UtcOffset::from_whole_seconds(minutes * 60).ok()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use axum::http::header::COOKIE;
    use time::macros::offset;

    use super::*;

    #[test]
    fn whole_minutes_within_fourteen_hours_are_an_offset() {
        assert_eq!(parse_offset("0"), Some(UtcOffset::UTC));
        assert_eq!(parse_offset("120"), Some(offset!(+2)));
        assert_eq!(parse_offset("-300"), Some(offset!(-5)));
        assert_eq!(parse_offset("330"), Some(offset!(+5:30)));
        assert_eq!(parse_offset("840"), Some(offset!(+14)));
        assert_eq!(parse_offset("-840"), Some(offset!(-14)));
    }

    #[test]
    fn anything_else_is_no_offset() {
        for text in [
            "", "-", "841", "-841", "+60", "60.5", " 60", "60 ", "1e2", "0x10", "--60", "9999",
            "utc",
        ] {
            assert_eq!(parse_offset(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_offset_is_read_from_its_cookie_only() {
        let mut headers = HeaderMap::new();
        assert_eq!(viewer_offset(&headers), None);
        headers.insert(COOKIE, HeaderValue::from_static("tz=60; dibs_tz=-180"));
        assert_eq!(viewer_offset(&headers), Some(offset!(-3)));
        headers.insert(COOKIE, HeaderValue::from_static("dibs_tz=abc"));
        assert_eq!(viewer_offset(&headers), None);
    }
}
