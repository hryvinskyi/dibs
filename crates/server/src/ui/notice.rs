//! The page that tells a signed-in person why what they asked for was not done, with a way back.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::chrome::{self, Chrome, Section};
use crate::ui::render::html;
use crate::ui::session::UiSession;

/// Why it was not done. The wording of each is the template's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The form's token is not the session's: an old page, or a form another site sent.
    FormExpired,
    /// A take-over without a reason.
    ReasonMissing,
    /// What was asked for does not exist, or the person may not see it.
    NotFound,
    /// The service refused, saying why in a sentence written for the person.
    Refused(String),
}

impl Problem {
    /// The problem a refusal of the service is shown as.
    pub fn of(error: &ApiError) -> Self {
        if error.status() == StatusCode::NOT_FOUND {
            Self::NotFound
        } else {
            Self::Refused(error.message().to_owned())
        }
    }
}

#[derive(Template)]
#[template(path = "notice.html")]
struct NoticePage {
    chrome: Chrome,
    problem: Problem,
    back: &'static str,
}

/// The notice page for `problem`, of this status, linking back to `back`. A failure to read the
/// page's frame answers that failure instead.
pub async fn notice(
    state: &AppState,
    session: &UiSession,
    status: StatusCode,
    problem: Problem,
    back: &'static str,
) -> Response {
    match chrome::load(state, session, Section::Other).await {
        Ok(signed) => html(
            status,
            &NoticePage {
                chrome: signed.chrome,
                problem,
                back,
            },
        ),
        Err(error) => error.into_response(),
    }
}

/// The notice page for a refusal of the service, with the refusal's status.
pub async fn refused(
    state: &AppState,
    session: &UiSession,
    error: &ApiError,
    back: &'static str,
) -> Response {
    if error.status().is_server_error() {
        return error.clone().into_response();
    }
    notice(state, session, error.status(), Problem::of(error), back).await
}
