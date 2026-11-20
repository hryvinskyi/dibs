//! `GET /ui/search`: Search, the screen that finds anything of the project by its name or by words
//! from it — claims, pull requests, tickets of the queue, branches, areas, people, conversations and
//! slots — each hit leading to its page here and, when it has one, to its page outside.
//!
//! With `palette=1` the answer is the results alone, without the frame, for the search box every
//! page carries: nothing but what the hits are built from is read, and a kind lists fewer hits. An
//! empty query reads nothing and answers the hint.

mod index;
mod rank;

use askama::Template;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;
use time::OffsetDateTime;

use dibs_app::use_cases::{MeView, StatusView};

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::links::{LinkContext, places};
use crate::ui::render::html;
use crate::ui::search::index::{Entry, Sources, entries};
use crate::ui::search::rank::{Group, rank};
use crate::ui::session::{PageSession, UiSession};
use crate::ui::shell::{self, Here, Screen, ShellView};
use crate::ui::signed_page;

/// How many hits of one kind the page lists.
const PAGE_CAP: usize = 50;

/// How many hits of one kind the palette lists.
const PALETTE_CAP: usize = 5;

#[derive(Template)]
#[template(path = "screens/search.html")]
struct SearchPage {
    shell: ShellView,
    results: ResultsView,
}

#[derive(Template)]
#[template(path = "screens/search.html", block = "results")]
struct PaletteAnswer {
    results: ResultsView,
}

/// The query of the search's address as it arrives.
#[derive(Debug, Default, Deserialize)]
pub struct RawSearch {
    q: Option<String>,
    palette: Option<String>,
}

/// The results of a query.
#[derive(Debug)]
struct ResultsView {
    /// The query.
    q: String,
    /// Whether they are the palette's, whose hits are the options of a list box.
    palette: bool,
    /// What they show.
    shown: Shown,
}

/// What the results show.
#[derive(Debug)]
enum Shown {
    /// What a query can find, for an empty one.
    Hint,
    /// That nothing matches.
    Nothing,
    /// The hits, by kind.
    Groups(Vec<GroupView>),
}

/// The hits of one kind.
#[derive(Debug)]
struct GroupView {
    /// The kind's heading.
    heading: &'static str,
    /// The name the heading is identified by.
    code: &'static str,
    /// The hits.
    hits: Vec<HitView>,
    /// How many more hits of the kind there are than are listed.
    more: usize,
}

/// One hit.
#[derive(Debug)]
struct HitView {
    /// Its place among every hit listed, from 0.
    n: usize,
    /// How it is named.
    title: String,
    /// The line under its name.
    sub: String,
    /// Its page here.
    here: String,
    /// Its page outside, when it has one.
    out: Option<OutView>,
}

/// A page outside this service.
#[derive(Debug)]
struct OutView {
    /// Its address.
    href: String,
    /// What its owner is called.
    owner: &'static str,
}

impl ResultsView {
    /// The results of `q` among `entries`, at most `cap` per kind.
    fn of(q: String, entries: &[Entry], cap: usize, palette: bool) -> Self {
        let shown = if q.trim().is_empty() {
            Shown::Hint
        } else {
            let groups = rank(entries, &q, cap);
            if groups.is_empty() {
                Shown::Nothing
            } else {
                Shown::Groups(views(&groups))
            }
        };
        Self { q, palette, shown }
    }
}

/// The views of the groups, their hits numbered in order.
fn views(groups: &[Group<'_>]) -> Vec<GroupView> {
    let mut n = 0;
    groups
        .iter()
        .map(|group| GroupView {
            heading: group.kind.heading(),
            code: group.kind.code(),
            hits: group
                .hits
                .iter()
                .map(|entry| {
                    let hit = HitView {
                        n,
                        title: entry.title.clone(),
                        sub: entry.sub.clone(),
                        here: entry.here.clone(),
                        out: entry.out.as_ref().map(|(href, owner)| OutView {
                            href: href.clone(),
                            owner,
                        }),
                    };
                    n += 1;
                    hit
                })
                .collect(),
            more: group.more,
        })
        .collect()
}

/// Search, or with `palette=1` the palette's results.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn page(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
    query: Result<Query<RawSearch>, QueryRejection>,
) -> Result<Response, ApiError> {
    let raw = query.map(|Query(raw)| raw).unwrap_or_default();
    let q = raw.q.unwrap_or_default();
    if raw.palette.as_deref() == Some("1") {
        return palette(&state, &session, q).await;
    }
    let (shell, loaded, links) = shell::load(&state, &session, Screen::Search, &here).await?;
    let found = if q.trim().is_empty() {
        Vec::new()
    } else {
        read_entries(
            &state,
            &session,
            &loaded.me,
            &loaded.status,
            loaded.now,
            &links,
        )
        .await?
    };
    let page = SearchPage {
        shell,
        results: ResultsView::of(q, &found, PAGE_CAP, false),
    };
    Ok(signed_page(&session, html(StatusCode::OK, &page)))
}

/// The palette's results of `q`, without the frame.
///
/// # Errors
/// The answer for a use case's failure.
async fn palette(state: &AppState, session: &UiSession, q: String) -> Result<Response, ApiError> {
    let found = if q.trim().is_empty() {
        Vec::new()
    } else {
        let actor = session.actor();
        let me = state.app.me.execute(actor).await?;
        let status = state.app.status.execute(actor).await?;
        let links = LinkContext::new(
            me.settings.clone(),
            state.ref_schemes.clone(),
            state.forges.clone(),
            places(&status),
        );
        read_entries(state, session, &me, &status, state.clock.now(), &links).await?
    };
    let answer = PaletteAnswer {
        results: ResultsView::of(q, &found, PALETTE_CAP, true),
    };
    Ok(signed_page(session, html(StatusCode::OK, &answer)))
}

/// Reads the members and the conversations, and builds every entry.
///
/// # Errors
/// The answer for a use case's failure.
async fn read_entries(
    state: &AppState,
    session: &UiSession,
    me: &MeView,
    status: &StatusView,
    now: OffsetDateTime,
    links: &LinkContext,
) -> Result<Vec<Entry>, ApiError> {
    let actor = session.actor();
    let members = state.app.list_members.execute(actor).await?;
    let threads = state.app.list_threads.execute(actor, None).await?;
    Ok(entries(&Sources {
        status,
        settings: &me.settings,
        viewer: &me.member.handle,
        members: &members,
        threads: &threads,
        now,
        links,
        forges: &state.forges,
    }))
}
