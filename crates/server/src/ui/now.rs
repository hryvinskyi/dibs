//! `GET /ui/`: Now, the screen the pages open on, in the frame of the pages; its list says that nothing is shown
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
#[template(path = "screens/now.html")]
struct NowPage {
    shell: ShellView,
}

/// Now.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn page(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
) -> Result<Response, ApiError> {
    let (shell, _, _) = shell::load(&state, &session, Screen::Now, &here).await?;
    Ok(signed_page(
        &session,
        html(StatusCode::OK, &NowPage { shell }),
    ))
}
