//! Cookies the pages read and set. Every cookie the server sets is `HttpOnly`, `Secure`,
//! `SameSite=Strict` and scoped to `/ui`, so no script reads it, no other path receives it, and no
//! request started from another site carries it.

use axum::http::header::{COOKIE, InvalidHeaderValue};
use axum::http::{HeaderMap, HeaderValue};

/// The value of the first cookie of this name the request carries, if any.
pub fn read<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|text| text.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.trim())
}

/// The `Set-Cookie` value that keeps `value` under `name` for `max_age_secs` seconds.
///
/// # Errors
/// When the value holds a character a header may not carry.
pub fn set(name: &str, value: &str, max_age_secs: i64) -> Result<HeaderValue, InvalidHeaderValue> {
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/ui; Max-Age={max_age_secs}; HttpOnly; Secure; SameSite=Strict"
    ))
}

/// The `Set-Cookie` value that removes the cookie of this name.
///
/// # Errors
/// When the name holds a character a header may not carry.
pub fn clear(name: &str) -> Result<HeaderValue, InvalidHeaderValue> {
    set(name, "", 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(COOKIE, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn a_cookie_is_found_by_its_exact_name_in_any_cookie_header() {
        let headers = with(&["theme=dark; dibs_ui_old=x", "dibs_ui=abc ; other=1"]);
        assert_eq!(read(&headers, "dibs_ui"), Some("abc"));
        assert_eq!(read(&headers, "theme"), Some("dark"));
        assert_eq!(read(&headers, "dibs"), None);
        assert_eq!(read(&HeaderMap::new(), "dibs_ui"), None);
        assert_eq!(
            read(&with(&["dibs_ui=first; dibs_ui=second"]), "dibs_ui"),
            Some("first")
        );
    }

    #[test]
    fn a_set_cookie_carries_every_flag_and_a_cleared_one_ends_now() {
        let set = set("dibs_ui", "dibs_abc", 43_200).unwrap();
        assert_eq!(
            set,
            "dibs_ui=dibs_abc; Path=/ui; Max-Age=43200; HttpOnly; Secure; SameSite=Strict"
        );
        assert_eq!(
            clear("dibs_ui").unwrap(),
            "dibs_ui=; Path=/ui; Max-Age=0; HttpOnly; Secure; SameSite=Strict"
        );
        assert!(super::set("dibs_ui", "bad\nvalue", 1).is_err());
    }
}
