//! `GET /ui/search`: Search, the screen that finds anything by its name, in the frame of the pages; its list says that nothing is shown
//! there yet.

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::render::html;
use crate::ui::session::PageSession;
use crate::ui::shell::{self, Here, Screen, ShellView};
use crate::ui::signed_page;

#[derive(Template)]
#[template(path = "screens/search.html")]
struct SearchPage {
    shell: ShellView,
}

/// Search.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn page(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
) -> Result<Response, ApiError> {
    let (shell, _, _) = shell::load(&state, &session, Screen::Search, &here).await?;
    Ok(signed_page(
        &session,
        html(StatusCode::OK, &SearchPage { shell }),
    ))
}
