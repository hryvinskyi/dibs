//! How a page becomes an answer: a template rendered into HTML, or a redirect after a form.

use askama::Template;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;

/// The template rendered as an HTML answer of this status. A template that fails to render answers
/// the internal error, and the failure is logged.
pub fn html(status: StatusCode, page: &impl Template) -> Response {
    match page.render() {
        Ok(body) => (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            )],
            body,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "a page could not be rendered");
            ApiError::internal().into_response()
        }
    }
}

/// `303 See Other` to a path of this service.
pub fn see_other(location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(value) => (StatusCode::SEE_OTHER, [(header::LOCATION, value)]).into_response(),
        Err(_) => {
            tracing::error!("a redirect named a location no header can carry");
            ApiError::internal().into_response()
        }
    }
}
