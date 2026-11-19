//! `GET /ui/queue` and `GET /ui/queue/{item}`: the tickets that wait to be claimed — the ready ones
//! grouped by priority, then those that are not ready and why — filtered by text, and a ticket in
//! the inspector beside them.
//!
//! A ticket a live claim holds is a claim and is on Claims, not here. An address whose segment does
//! not read as a work item, or names an item the queue does not list, answers `404` with the whole
//! page and an inspector saying the ticket is not in the queue, with a link to its claim when it has
//! one.

mod inspector;
pub(crate) mod listing;
pub(crate) mod view;

use std::collections::BTreeMap;

use askama::Template;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;

use dibs_domain::ids::ItemId;

use crate::error::ApiError;
use crate::fields::item_in_path;
use crate::http::AppState;
use crate::ui::links::Link;
use crate::ui::queue::inspector::{GoneView, InspectorView};
use crate::ui::queue::view::{QueueItemView, queue_items};
use crate::ui::render::html;
use crate::ui::session::{PageSession, UiSession};
use crate::ui::shell::{self, Here, Screen, ShellView};
use crate::ui::signed_page;
use crate::ui::text_query::{RawQuery, TextQuery};

/// How many rows of a group the list shows before it offers all of them.
const GROUP_ROWS: usize = 25;

#[derive(Template)]
#[template(path = "screens/queue.html")]
struct QueuePage {
    shell: ShellView,
    list: ListView,
    inspector: Inspector,
}

/// The list of tickets.
#[derive(Debug)]
struct ListView {
    /// The text the tickets match.
    q: String,
    /// Whether every row is shown.
    all: bool,
    /// The address of the list alone, which closes the inspector.
    close: String,
    /// How many ready tickets the text leaves.
    ready: usize,
    /// How many tickets that are not ready it leaves.
    waiting: usize,
    /// The groups: a ready one for each priority present, soonest first, then the not ready ones.
    groups: Vec<Group>,
}

/// What a group of rows is.
#[derive(Debug, Clone, Copy)]
enum Heading {
    /// The ready tickets of a priority.
    Ready(u8),
    /// The tickets that are not ready.
    NotReady,
}

/// A group of rows.
#[derive(Debug)]
struct Group {
    /// What it is.
    heading: Heading,
    /// How many tickets it holds.
    total: usize,
    /// The rows shown.
    rows: Vec<Row>,
    /// The address of the list showing every row, when this group shows fewer than it holds.
    more: Option<String>,
}

/// A ticket in the list.
#[derive(Debug)]
struct Row {
    /// The ticket.
    item: QueueItemView,
    /// The address of the ticket in the inspector, with the list's state.
    href: String,
    /// Where its ticket's name leads: the tracker's page when it has one.
    tracker: Link,
    /// Whether it is the ticket in the inspector.
    current: bool,
}

/// What the inspector shows.
#[derive(Debug)]
enum Inspector {
    /// Nothing selected.
    Hint,
    /// A ticket of the queue.
    Ticket(Box<InspectorView>),
    /// An address that names no ticket of the queue.
    Gone(GoneView),
}

/// Which ticket an address selects.
enum Selected {
    /// None.
    Nothing,
    /// The ticket of this item.
    Item(ItemId),
    /// A segment that names no item.
    Unreadable,
}

/// Queue, nothing selected.
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

/// Queue with a ticket selected.
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
fn state_of(query: Result<Query<RawQuery>, QueryRejection>) -> TextQuery {
    TextQuery::of(
        Screen::Queue,
        query.map(|Query(raw)| raw).unwrap_or_default(),
    )
}

/// The page in its frame: `200`, or `404` when the address selects no ticket of the queue.
async fn render(
    state: &AppState,
    session: &UiSession,
    here: &Here,
    query: TextQuery,
    selected: Selected,
) -> Result<Response, ApiError> {
    let (shell, loaded, links) = shell::load(state, session, Screen::Queue, here).await?;
    let items = queue_items(&loaded.status, &links);
    let (inspector, status, current) = match selected {
        Selected::Nothing => (Inspector::Hint, StatusCode::OK, None),
        Selected::Unreadable => (
            Inspector::Gone(GoneView { claim: None }),
            StatusCode::NOT_FOUND,
            None,
        ),
        Selected::Item(id) => match items.iter().find(|item| item.id == id) {
            Some(item) => {
                let view = InspectorView::of(item.clone(), &loaded.status, &links);
                (Inspector::Ticket(Box::new(view)), StatusCode::OK, Some(id))
            }
            None => (
                Inspector::Gone(GoneView::of(id, &loaded.status)),
                StatusCode::NOT_FOUND,
                None,
            ),
        },
    };
    let page = QueuePage {
        list: ListView::of(items, &query, current),
        shell,
        inspector,
    };
    Ok(signed_page(session, html(status, &page)))
}

impl ListView {
    /// The list of `items` in the state `query`, the ticket on `current` marked.
    fn of(items: Vec<QueueItemView>, query: &TextQuery, current: Option<ItemId>) -> Self {
        let mut ready: BTreeMap<u8, Vec<Row>> = BTreeMap::new();
        let mut waiting = Vec::new();
        for item in items.into_iter().filter(|item| query.leaves(&item.text())) {
            let priority = item.priority;
            let is_ready = item.is_ready();
            let row = Row::of(item, query, current);
            if is_ready {
                ready.entry(priority).or_default().push(row);
            } else {
                waiting.push(row);
            }
        }
        let ready_count = ready.values().map(Vec::len).sum();
        let waiting_count = waiting.len();
        let groups = ready
            .into_iter()
            .map(|(priority, rows)| Group::of(Heading::Ready(priority), rows, query, current))
            .chain(
                (!waiting.is_empty())
                    .then(|| Group::of(Heading::NotReady, waiting, query, current)),
            )
            .collect();
        Self {
            q: query.q.clone(),
            all: query.all,
            close: query.address(None),
            ready: ready_count,
            waiting: waiting_count,
            groups,
        }
    }
}

impl Group {
    /// The group of `rows`, cut to its first rows unless the state shows every one; `current` is
    /// kept on the address that shows them all.
    fn of(
        heading: Heading,
        mut rows: Vec<Row>,
        query: &TextQuery,
        current: Option<ItemId>,
    ) -> Self {
        let total = rows.len();
        if !query.shows_all() {
            rows.truncate(GROUP_ROWS);
        }
        let more = (rows.len() < total).then(|| {
            query
                .with_all()
                .address(current.map(|id| id.to_string()).as_deref())
        });
        Self {
            heading,
            total,
            rows,
            more,
        }
    }
}

impl Row {
    /// The row of a ticket.
    fn of(item: QueueItemView, query: &TextQuery, current: Option<ItemId>) -> Self {
        Self {
            href: query.address(Some(&item.id.to_string())),
            tracker: item
                .ticket
                .out
                .clone()
                .unwrap_or_else(|| item.ticket.main.clone()),
            current: current == Some(item.id),
            item,
        }
    }
}
