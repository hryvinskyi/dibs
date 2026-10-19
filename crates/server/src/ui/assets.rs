//! The browser bundle: the scripts and the stylesheet of `web/src`, embedded at build time and served
//! under one directory named by the hash of the whole set, so a module's relative imports resolve
//! beside it. A file is cached for a year: a change to any file moves the whole set to a new
//! directory.

use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;

include!(concat!(env!("OUT_DIR"), "/bundle.rs"));

/// How long a browser may keep a file of the bundle.
const CACHE_FOREVER: &str = "public, max-age=31536000, immutable";

/// The media type of a bundle file, by its extension.
pub fn content_type(name: &str) -> Option<&'static str> {
    let (_, extension) = name.rsplit_once('.')?;
    match extension {
        "js" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        _ => None,
    }
}

/// `GET /ui/static/{hash}/{name}`: one file of the bundle. Another hash, or a name the bundle does
/// not hold, is not found.
pub async fn file(Path((hash, name)): Path<(String, String)>) -> Response {
    let found = BUNDLE
        .iter()
        .find(|(file, _)| *file == name)
        .filter(|_| hash == BUNDLE_HASH)
        .and_then(|(file, bytes)| content_type(file).map(|kind| (kind, *bytes)));
    match found {
        Some((kind, bytes)) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, HeaderValue::from_static(kind)),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static(CACHE_FOREVER),
                ),
            ],
            bytes,
        )
            .into_response(),
        None => ApiError::no_route().into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_and_stylesheets_have_their_types_and_nothing_else_is_served() {
        assert_eq!(
            content_type("main.js"),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(content_type("dibs.css"), Some("text/css; charset=utf-8"));
        assert_eq!(content_type("notes.txt"), None);
        assert_eq!(content_type("js"), None);
    }

    #[test]
    fn the_bundle_holds_the_entry_point_and_the_stylesheet() {
        let names: Vec<&str> = BUNDLE.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&"main.js"), "{names:?}");
        assert!(names.contains(&"dibs.css"), "{names:?}");
        assert_eq!(BUNDLE_HASH.len(), 16);
    }
}
