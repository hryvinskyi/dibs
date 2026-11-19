//! `GET /ui/queue` and `GET /ui/queue/{item}`: Queue, and a ticket waiting to be claimed in the inspector beside it,
//! in the frame of the pages; the list and the inspector say that nothing is shown there yet. The
//! segment names the work item; one that does not read as that answers `404` with the whole page and
//! nothing selected.

use askama::Template;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;

use crate::error::ApiError;
use crate::fields::item_in_path;
use crate::http::AppState;
use crate::ui::render::html;
use crate::ui::session::{PageSession, UiSession};
use crate::ui::shell::{self, Here, Screen, ShellView};
use crate::ui::signed_page;

#[derive(Template)]
#[template(path = "screens/queue.html")]
struct QueuePage {
    shell: ShellView,
    selected: bool,
}

/// Queue, nothing selected.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn list(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
) -> Result<Response, ApiError> {
    render(&state, &session, &here, StatusCode::OK, false).await
}

/// Queue with a ticket waiting to be claimed selected.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn inspect(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
    path: Result<Path<String>, PathRejection>,
) -> Result<Response, ApiError> {
    let selected = path.is_ok_and(|Path(segment)| item_in_path(&segment).is_ok());
    let status = if selected {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    render(&state, &session, &here, status, selected).await
}

/// The page in its frame, answered with `status`.
async fn render(
    state: &AppState,
    session: &UiSession,
    here: &Here,
    status: StatusCode,
    selected: bool,
) -> Result<Response, ApiError> {
    let (shell, _, _) = shell::load(state, session, Screen::Queue, here).await?;
    let page = QueuePage { shell, selected };
    Ok(signed_page(session, html(status, &page)))
}
