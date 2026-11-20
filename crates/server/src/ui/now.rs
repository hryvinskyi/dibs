//! `GET /ui/`: Now, the screen the pages open on, in the frame of the pages: the cards of what waits
//! for a person's look, every claim in the column of how long it has been held, one row per person
//! in the rail's order, and what happened since the start of the viewer's day.
//!
//! The viewer's day starts at midnight of the offset their browser recorded, or of UTC when it
//! recorded none. Now has no inspector.

mod ages;
mod cards;
mod today;

use std::collections::BTreeSet;

use askama::Template;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use time::UtcOffset;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::claims::view::{Activity, ClaimView, Scene};
use crate::ui::links::PersonView;
use crate::ui::moments::start_of_day;
use crate::ui::now::ages::Age;
use crate::ui::now::cards::{Card, Facts};
use crate::ui::now::today::{Happening, date, happenings};
use crate::ui::render::html;
use crate::ui::session::PageSession;
use crate::ui::shell::{self, Here, Loaded, Screen, ShellView};
use crate::ui::signed_page;
use crate::ui::zone::viewer_offset;

#[derive(Template)]
#[template(path = "screens/now.html")]
struct NowPage {
    shell: ShellView,
    head: Head,
    cards: Vec<Card>,
    grid: Vec<GridRow>,
    today: Vec<Happening>,
}

/// What the heading's line says about the project.
#[derive(Debug)]
struct Head {
    /// The viewer's date, `YYYY-MM-DD`.
    date: String,
    /// How many live and stale claims there are.
    claims: usize,
    /// How many members hold them.
    people: usize,
    /// How many tickets are ready to be claimed.
    ready: usize,
}

/// One person's claims in the age grid.
#[derive(Debug)]
struct GridRow {
    /// The person.
    person: PersonView,
    /// Their claims in each column, in column order.
    cells: Vec<GridCell>,
}

/// One column of a person's row in the age grid.
#[derive(Debug)]
struct GridCell {
    /// The column.
    age: Age,
    /// The claims in it, the longest held first.
    claims: Vec<ClaimView>,
}

/// How many cards Now shows for what the frame loaded.
pub(crate) fn count(loaded: &Loaded) -> usize {
    cards::count(&Facts::of(loaded))
}

/// Now.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn page(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (shell, loaded, links) = shell::load(&state, &session, Screen::Now, &here).await?;
    let offset = viewer_offset(&headers).unwrap_or(UtcOffset::UTC);
    let since = start_of_day(loaded.now, offset);
    let completed = state
        .app
        .list_completed
        .execute(session.actor(), since)
        .await?;
    let status = &loaded.status;
    let scene = Scene {
        status,
        viewer: &loaded.me.member.handle,
        now: loaded.now,
        stale_after: loaded.me.settings.stale_after(),
        links: &links,
    };
    let mut claims: Vec<ClaimView> = status
        .live
        .iter()
        .chain(&status.stale)
        .map(|held| ClaimView::of(held, &scene))
        .collect();
    claims.sort_by_key(|claim| (claim.claimed_at, claim.id));
    let head = Head {
        date: date(loaded.now, offset),
        claims: claims.len(),
        people: claims
            .iter()
            .map(|claim| claim.holder.handle.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        ready: status.ready.ready.len(),
    };
    let page = NowPage {
        cards: cards::cards(&Facts::of(&loaded), &links),
        today: happenings(&claims, &completed, since, loaded.now, &links),
        grid: grid(&shell, &claims, &loaded),
        head,
        shell,
    };
    Ok(signed_page(&session, html(StatusCode::OK, &page)))
}

/// The age grid: a row per person of the rail, in its order, each claim in its column.
fn grid(shell: &ShellView, claims: &[ClaimView], loaded: &Loaded) -> Vec<GridRow> {
    shell
        .rail
        .people
        .iter()
        .map(|row| GridRow {
            person: row.person.clone(),
            cells: Age::ALL
                .into_iter()
                .map(|age| GridCell {
                    age,
                    claims: claims
                        .iter()
                        .filter(|claim| {
                            claim.holder.handle == row.person.handle
                                && Age::of(
                                    claim.activity == Activity::Review,
                                    loaded.now - claim.claimed_at,
                                ) == age
                        })
                        .cloned()
                        .collect(),
                })
                .collect(),
        })
        .collect()
}
