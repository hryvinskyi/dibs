//! `POST /ui/items/{id}/takeover`: a person takes over another member's stale claim from the status
//! page or the board, with the reason the previous holder is told.
//!
//! The browser is one of the person's sessions: the take-over is made from their session labelled
//! `web` on the host `web`, registered on first use and reused afterwards.
//!
//! The form names the page to return to: `status` for the status page, or a path of this service,
//! such as the board with its filter. Anything else — an address of another site, one a browser
//! would read as another site's (`//host`, `/\host`), or a value no header can carry — returns
//! to the board, so the form can never send the person elsewhere.

use axum::Form;
use axum::extract::rejection::{FormRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use dibs_app::use_cases::{RegisterSessionInput, TakeOverInput};
use dibs_domain::message::{Body, SessionLabel};
use dibs_domain::session::{AgentLabel, HostName};

use crate::error::ApiError;
use crate::fields::item_in_path;
use crate::http::AppState;
use crate::ui::notice::{Problem, notice, refused};
use crate::ui::render::see_other;
use crate::ui::session::{PageSession, UiSession};
use crate::ui::signed_page;

/// The host, label and agent of the browser's session.
const WEB_HOST: &str = "web";
const WEB_LABEL: &str = "web";
const WEB_AGENT: &str = "browser";

/// The fields of the take-over form.
#[derive(Deserialize)]
pub struct TakeOverForm {
    csrf: Option<String>,
    reason: Option<String>,
    back: Option<String>,
}

/// The page a form that names no page of its own returns to.
const BOARD: &str = "/ui/board";

/// The page a take-over form returns to: the status page for `status` or no value, the path the
/// form names when it is a path of this service, and the board for anything else.
fn back_to(back: Option<&str>) -> &str {
    match back {
        None | Some("status") => "/ui/",
        Some(path) if is_local_path(path) => path,
        Some(_) => BOARD,
    }
}

/// Whether a value is a path of this service that a browser cannot read as another site's: it
/// starts with one `/`, holds no `\` (which browsers read as `/`), and only visible ASCII.
fn is_local_path(value: &str) -> bool {
    value.starts_with('/')
        && !value.starts_with("//")
        && !value.contains('\\')
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Takes over the claim and returns to the page the form was on. A refusal — the claim is live
/// again, an area is held, the item is closed — is shown with the service's reason.
pub async fn take_over(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    path: Result<Path<String>, PathRejection>,
    form: Result<Form<TakeOverForm>, FormRejection>,
) -> Response {
    let form = form.ok().map(|Form(form)| form);
    let back = back_to(form.as_ref().and_then(|form| form.back.as_deref())).to_owned();
    let back = back.as_str();
    if !session.accepts(form.as_ref().and_then(|form| form.csrf.as_deref())) {
        return notice(
            &state,
            &session,
            StatusCode::FORBIDDEN,
            Problem::FormExpired,
            back,
        )
        .await;
    }
    let reason = form
        .and_then(|form| form.reason)
        .and_then(|text| Body::parse(&text).ok());
    let Some(reason) = reason else {
        return notice(
            &state,
            &session,
            StatusCode::UNPROCESSABLE_ENTITY,
            Problem::ReasonMissing,
            back,
        )
        .await;
    };
    let made = match path
        .map_err(ApiError::from)
        .and_then(|Path(id)| item_in_path(&id))
    {
        Ok(item) => take(&state, &session, item, reason).await,
        Err(error) => Err(error),
    };
    match made {
        Ok(()) => signed_page(&session, see_other(back)),
        Err(error) => refused(&state, &session, &error, back).await,
    }
}

/// Registers the browser's session and makes the take-over from it.
async fn take(
    state: &AppState,
    session: &UiSession,
    item: dibs_domain::ids::ItemId,
    reason: Body,
) -> Result<(), ApiError> {
    let actor = session.actor();
    let input = RegisterSessionInput {
        host: HostName::parse(WEB_HOST).map_err(|_| ApiError::internal())?,
        label: SessionLabel::parse(WEB_LABEL).map_err(|_| ApiError::internal())?,
        agent: AgentLabel::parse(WEB_AGENT).map_err(|_| ApiError::internal())?,
        push: false,
    };
    let browser = state.app.register_session.execute(actor, input).await?;
    state
        .app
        .take_over
        .execute(
            actor,
            TakeOverInput {
                item,
                session: browser.id,
                areas: Vec::new(),
                reason,
                kind: None,
            },
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_returns_to_the_status_page_or_the_path_it_names() {
        assert_eq!(back_to(None), "/ui/");
        assert_eq!(back_to(Some("status")), "/ui/");
        assert_eq!(back_to(Some("/ui/board")), "/ui/board");
        assert_eq!(
            back_to(Some("/ui/board?area=api%3Abilling&member=bob")),
            "/ui/board?area=api%3Abilling&member=bob"
        );
    }

    #[test]
    fn an_address_of_another_site_returns_to_the_board() {
        for foreign in [
            "https://example.com/",
            "//example.com/ui/board",
            "/\\example.com",
            "/\\/example.com",
            "javascript:alert(1)",
            "ui/board",
            "board",
            "",
            "/ui/board\r\nSet-Cookie: x=1",
            "/ui/ board",
        ] {
            assert_eq!(back_to(Some(foreign)), "/ui/board", "{foreign:?}");
        }
    }
}
