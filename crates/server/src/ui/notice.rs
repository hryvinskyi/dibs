//! The page that tells a signed-in person why what they asked for was not done, with a way back,
//! in the frame of the screen the request belongs to. A sentence of the service's refusal is
//! printed with the references in it linked.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::links::LinkedText;
use crate::ui::render::html;
use crate::ui::session::UiSession;
use crate::ui::shell::{self, Here, Screen, ShellView};

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

/// A problem as the page prints it.
#[derive(Debug, Clone)]
enum Shown {
    /// The form expired.
    FormExpired,
    /// The take-over had no reason.
    ReasonMissing,
    /// Nothing to show.
    NotFound,
    /// The service's sentence, its references linked.
    Refused(LinkedText),
}

#[derive(Template)]
#[template(path = "notice.html")]
struct NoticePage<'a> {
    shell: ShellView,
    problem: Shown,
    back: &'a str,
}

/// The notice page for `problem`, of this status, in the frame of `screen`, linking back to
/// `back`. A failure to read the page's frame answers that failure instead.
pub async fn notice(
    state: &AppState,
    session: &UiSession,
    status: StatusCode,
    problem: Problem,
    screen: Screen,
    back: &str,
) -> Response {
    let here = Here::at(back.to_owned());
    let (shell, _, links) = match shell::load(state, session, screen, &here).await {
        Ok(loaded) => loaded,
        Err(error) => return error.into_response(),
    };
    let problem = match problem {
        Problem::FormExpired => Shown::FormExpired,
        Problem::ReasonMissing => Shown::ReasonMissing,
        Problem::NotFound => Shown::NotFound,
        Problem::Refused(sentence) => Shown::Refused(LinkedText::of(&sentence, None, &links)),
    };
    html(
        status,
        &NoticePage {
            shell: shell.answering_a_form(),
            problem,
            back,
        },
    )
}

/// The notice page for a refusal of the service, with the refusal's status, in the frame of
/// `screen`.
pub async fn refused(
    state: &AppState,
    session: &UiSession,
    error: &ApiError,
    screen: Screen,
    back: &str,
) -> Response {
    if error.status().is_server_error() {
        return error.clone().into_response();
    }
    notice(
        state,
        session,
        error.status(),
        Problem::of(error),
        screen,
        back,
    )
    .await
}
