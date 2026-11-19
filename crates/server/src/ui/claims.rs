//! `GET /ui/claims` and `GET /ui/claims/{item}`: the live and stale claims, grouped by holder —
//! the viewer first — filtered by how they stand, by holder and by text, and a claim in the
//! inspector beside them.
//!
//! An address whose segment does not read as a work item, or names an item no live or stale claim
//! holds, answers `404` with the whole page and an inspector saying the claim is not open, with the
//! item's ticket and its place in the queue when it has them.

pub(crate) mod classify;
mod inspector;
mod query;
pub(crate) mod view;

use std::collections::BTreeSet;

use askama::Template;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;

use dibs_app::AppError;
use dibs_app::records::ItemRecord;
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;

use crate::error::ApiError;
use crate::fields::item_in_path;
use crate::http::AppState;
use crate::ui::claims::inspector::{GoneView, InspectorView};
use crate::ui::claims::query::{FILTERS, ListQuery, RawQuery};
use crate::ui::claims::view::{ClaimView, Scene};
use crate::ui::links::{Link, PersonView};
use crate::ui::render::html;
use crate::ui::session::{PageSession, UiSession};
use crate::ui::shell::{self, Here, Screen, ShellView};
use crate::ui::signed_page;

#[derive(Template)]
#[template(path = "screens/claims.html")]
struct ClaimsPage {
    shell: ShellView,
    list: ListView,
    inspector: Inspector,
}

/// The list of claims.
#[derive(Debug)]
struct ListView {
    /// The handle whose claims it shows.
    who: Option<String>,
    /// The address of the same page with everyone's claims.
    everyone: String,
    /// The address of the list alone, which closes the inspector.
    close: String,
    /// The filters, each with its count among the claims `who` and `q` leave.
    filters: Vec<FilterEntry>,
    /// The name of the current filter, unless it is the default.
    filter: Option<&'static str>,
    /// The text the claims match.
    q: String,
    /// The claims shown, by holder.
    groups: Vec<Group>,
}

/// A filter as the list offers it.
#[derive(Debug)]
struct FilterEntry {
    /// Its name.
    code: &'static str,
    /// How many claims it keeps.
    count: usize,
    /// Whether it is the current one.
    current: bool,
    /// The address of the list with it.
    href: String,
}

/// One holder's claims in the list.
#[derive(Debug)]
struct Group {
    /// The holder.
    person: PersonView,
    /// The host of the claim among these seen last.
    host: String,
    /// How many areas these claims hold.
    areas: usize,
    /// The claims, longest held first.
    rows: Vec<Row>,
}

/// A claim in the list.
#[derive(Debug)]
struct Row {
    /// The claim.
    claim: ClaimView,
    /// The address of the claim in the inspector, with the list's state.
    href: String,
    /// Where its ticket's name leads: the tracker's page when it has one.
    tracker: Link,
    /// Whether it is the claim in the inspector.
    current: bool,
}

/// What the inspector shows.
#[derive(Debug)]
enum Inspector {
    /// Nothing selected.
    Hint,
    /// An open claim.
    Claim(Box<InspectorView>),
    /// A claim that is not open.
    Gone(GoneView),
}

/// Which claim an address selects.
enum Selected {
    /// None.
    Nothing,
    /// The claim on this item.
    Item(ItemId),
    /// A segment that names no item.
    Unreadable,
}

/// Claims, nothing selected.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn list(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
    query: Result<Query<RawQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    render(&state, &session, &here, state_of(query), Selected::Nothing).await
}

/// Claims with a claim selected.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn inspect(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    here: Here,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<RawQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let selected = path
        .ok()
        .and_then(|Path(segment)| item_in_path(&segment).ok())
        .map_or(Selected::Unreadable, Selected::Item);
    render(&state, &session, &here, state_of(query), selected).await
}

/// The list's state from the address's query; a query that does not read is the default state.
fn state_of(query: Result<Query<RawQuery>, QueryRejection>) -> ListQuery {
    ListQuery::of(query.map(|Query(raw)| raw).unwrap_or_default())
}

/// The page in its frame: `200`, or `404` when the address selects no open claim.
async fn render(
    state: &AppState,
    session: &UiSession,
    here: &Here,
    query: ListQuery,
    selected: Selected,
) -> Result<Response, ApiError> {
    let (shell, loaded, links) = shell::load(state, session, Screen::Claims, here).await?;
    let scene = Scene {
        status: &loaded.status,
        viewer: &loaded.me.member.handle,
        now: loaded.now,
        stale_after: loaded.me.settings.stale_after(),
        links: &links,
    };
    let held: Vec<_> = loaded
        .status
        .live
        .iter()
        .chain(&loaded.status.stale)
        .collect();
    let claims: Vec<ClaimView> = held
        .iter()
        .map(|held| ClaimView::of(held, &scene))
        .collect();
    let current = match selected {
        Selected::Item(id) => Some(id),
        Selected::Nothing | Selected::Unreadable => None,
    };
    let (inspector, status) = match selected {
        Selected::Nothing => (Inspector::Hint, StatusCode::OK),
        Selected::Unreadable => (
            Inspector::Gone(GoneView {
                ticket: None,
                queue: None,
            }),
            StatusCode::NOT_FOUND,
        ),
        Selected::Item(id) => {
            let open = held
                .iter()
                .zip(&claims)
                .find(|(held, _)| held.item.item.id == id);
            match open {
                Some((held, claim)) => {
                    let threads = state
                        .app
                        .list_threads
                        .execute(session.actor(), Some(id))
                        .await?;
                    let view = InspectorView::of(claim.clone(), held, &query, &threads, &scene);
                    (Inspector::Claim(Box::new(view)), StatusCode::OK)
                }
                None => {
                    let item = item(state, session, id).await?;
                    (
                        Inspector::Gone(GoneView::of(id, item.as_ref(), &scene)),
                        StatusCode::NOT_FOUND,
                    )
                }
            }
        }
    };
    let page = ClaimsPage {
        list: ListView::of(claims, &query, &loaded.me.member.handle, current),
        shell,
        inspector,
    };
    Ok(signed_page(session, html(status, &page)))
}

/// The item `id`, when the project has it.
async fn item(
    state: &AppState,
    session: &UiSession,
    id: ItemId,
) -> Result<Option<ItemRecord>, ApiError> {
    match state.app.get_item.execute(session.actor(), id).await {
        Ok(view) => Ok(Some(view.item.item)),
        Err(AppError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

impl ListView {
    /// The list of `claims` in the state `query`, the viewer's group first, the claim on `current`
    /// marked.
    fn of(
        claims: Vec<ClaimView>,
        query: &ListQuery,
        viewer: &Handle,
        current: Option<ItemId>,
    ) -> Self {
        let left: Vec<ClaimView> = claims
            .into_iter()
            .filter(|claim| query.leaves(claim))
            .collect();
        let filters = FILTERS
            .iter()
            .map(|filter| FilterEntry {
                code: filter.code,
                count: left.iter().filter(|claim| filter.keeps(claim)).count(),
                current: std::ptr::eq(filter, query.filter),
                href: query.with_filter(filter).address(None),
            })
            .collect();
        let mut shown: Vec<ClaimView> = left
            .into_iter()
            .filter(|claim| query.filter.keeps(claim))
            .collect();
        shown.sort_by(|a, b| {
            let mine = |claim: &ClaimView| claim.holder.handle != viewer.as_str();
            (mine(a), &a.holder.handle, a.claimed_at, a.id).cmp(&(
                mine(b),
                &b.holder.handle,
                b.claimed_at,
                b.id,
            ))
        });
        let mut groups: Vec<Group> = Vec::new();
        for claim in shown {
            let row = Row {
                href: query.address(Some(claim.id)),
                tracker: claim
                    .ticket
                    .out
                    .clone()
                    .unwrap_or_else(|| claim.ticket.main.clone()),
                current: current == Some(claim.id),
                claim,
            };
            match groups.last_mut() {
                Some(group) if group.person.handle == row.claim.holder.handle => {
                    group.rows.push(row);
                }
                _ => groups.push(Group {
                    person: row.claim.holder.clone(),
                    host: String::new(),
                    areas: 0,
                    rows: vec![row],
                }),
            }
        }
        for group in &mut groups {
            group.host = group
                .rows
                .iter()
                .max_by_key(|row| row.claim.last_seen_at)
                .map(|row| row.claim.host.clone())
                .unwrap_or_default();
            group.areas = group
                .rows
                .iter()
                .flat_map(|row| row.claim.areas.iter().map(|area| area.full.as_str()))
                .collect::<BTreeSet<_>>()
                .len();
        }
        Self {
            who: query.who.clone(),
            everyone: query.everyone().address(current),
            close: query.address(None),
            filters,
            filter: (!std::ptr::eq(query.filter, &FILTERS[0])).then_some(query.filter.code),
            q: query.q.clone(),
            groups,
        }
    }
}
