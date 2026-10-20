//! `GET /ui/`: the status page — live claims, stale claims with a take-over form, the areas held,
//! and the live wide claims with who has acknowledged each — with a reload banner when the claims
//! change.

use askama::Template;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::chrome::{self, Chrome, Section};
use crate::ui::live::latest_event;
use crate::ui::moments::{Ago, Moment, Span, moment, span};
use crate::ui::render::html;
use crate::ui::rows::{ClaimRow, HoldRow, WideRow};
use crate::ui::session::PageSession;
use crate::ui::signed_page;

/// The kinds of event after which the status page offers a reload.
pub const STATUS_CHANGES: &str = "claimed renewed taken_over released completed force_released \
     reserved wide_claimed wide_granted_by_ack grant_revoked review_started pr_merged pr_closed \
     review_ended completed_by_merge";

#[derive(Template)]
#[template(path = "status.html")]
struct StatusPage {
    chrome: Chrome,
    live: Vec<ClaimRow>,
    stale: Vec<ClaimRow>,
    holds: Vec<HoldRow>,
    wide: Vec<WideRow>,
    stale_after: Span,
    updated: Moment,
    after: u64,
    changed_kinds: &'static str,
}

/// The status page.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn page(
    State(state): State<AppState>,
    PageSession(session): PageSession,
) -> Result<Response, ApiError> {
    let actor = session.actor();
    let after = latest_event(&state, actor).await?;
    let view = state.app.status.execute(actor).await?;
    let signed = chrome::load(&state, &session, Section::Status).await?;
    let now = state.clock.now();
    let rows = |held: &[dibs_app::use_cases::HeldItem]| {
        held.iter()
            .map(|held| ClaimRow::of(held, actor.member, now))
            .collect::<Vec<_>>()
    };
    let page = StatusPage {
        chrome: signed.chrome,
        live: rows(&view.live),
        stale: rows(&view.stale),
        holds: view
            .holds
            .iter()
            .map(|hold| HoldRow::of(hold, &view.live))
            .collect(),
        wide: view.wide.iter().map(WideRow::of).collect(),
        stale_after: span(signed.settings.stale_after()),
        updated: moment(now, now),
        after,
        changed_kinds: STATUS_CHANGES,
    };
    Ok(signed_page(&session, html(StatusCode::OK, &page)))
}
