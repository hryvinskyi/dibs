//! `GET /ui/queue` and `GET /ui/queue/{item}`: the tickets that wait to be claimed — the ready ones
//! grouped by priority, then those that are not ready and why — filtered by text, and a ticket in
//! the inspector beside them.
//!
//! Besides the text (`q`) and every row (`all=1`), the address can narrow the list to exactly the
//! tickets the claim on an item keeps out of the queue (`kept_by={item}`) and to those that will
//! touch an area (`area={repo:name}`); a value that does not read narrows nothing. The narrowing is
//! kept in every address the list builds, like the text.
//!
//! A ticket a live claim holds is a claim and is on Claims, not here. An address whose segment does
//! not read as a work item, or names an item the queue does not list, answers `404` with the whole
//! page and an inspector saying the ticket is not in the queue, with a link to its claim when it has
//! one.

mod inspector;
pub(crate) mod listing;
pub(crate) mod view;

use std::collections::{BTreeMap, BTreeSet};

use askama::Template;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use serde::Deserialize;

use dibs_domain::area::QualifiedArea;
use dibs_domain::ids::ItemId;

use crate::error::ApiError;
use crate::fields::item_in_path;
use crate::http::AppState;
use crate::ui::links::{Link, LinkContext, TicketView, query_value};
use crate::ui::queue::inspector::{GoneView, InspectorView};
use crate::ui::queue::listing::{Narrowing, listed};
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
    /// The item whose claim's kept-out tickets the list is narrowed to, as its address writes it.
    kept_by: Option<String>,
    /// The area whose tickets the list is narrowed to, as its address writes it.
    area: Option<String>,
    /// What the narrowing keeps, when the list is narrowed.
    narrowed: Option<Narrowed>,
    /// The address of the list alone, which closes the inspector.
    close: String,
    /// How many ready tickets the text leaves.
    ready: usize,
    /// How many tickets that are not ready it leaves.
    waiting: usize,
    /// The groups: a ready one for each priority present, soonest first, then the not ready ones.
    groups: Vec<Group>,
}

/// What a narrowed list keeps, and the address of the list without the narrowing.
#[derive(Debug)]
struct Narrowed {
    /// What it keeps, in words.
    says: String,
    /// The address of the same list with every ticket.
    clear: String,
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

/// The narrowing of the list's address as it arrives.
#[derive(Debug, Default, Deserialize)]
pub struct RawNarrowing {
    kept_by: Option<String>,
    area: Option<String>,
}

impl RawNarrowing {
    /// The narrowing it names; a value that does not read narrows nothing.
    fn read(self) -> Narrowing {
        Narrowing {
            kept_by: self.kept_by.and_then(|text| item_in_path(text.trim()).ok()),
            touches: self
                .area
                .and_then(|text| QualifiedArea::parse(text.trim()).ok()),
        }
    }
}

/// The list's state as its address keeps it: the text and every row, and the narrowing.
#[derive(Debug, Clone)]
struct QueueState {
    /// The text the rows match and whether every row is shown.
    text: TextQuery,
    /// The tickets the list is narrowed to.
    narrowing: Narrowing,
}

impl QueueState {
    /// The state the address's query gives; a query that does not read is the default state.
    fn of(
        text: Result<Query<RawQuery>, QueryRejection>,
        narrowing: Result<Query<RawNarrowing>, QueryRejection>,
    ) -> Self {
        Self {
            text: TextQuery::of(
                Screen::Queue,
                text.map(|Query(raw)| raw).unwrap_or_default(),
            ),
            narrowing: narrowing.map(|Query(raw)| raw).unwrap_or_default().read(),
        }
    }

    /// The state of the whole list narrowed by `narrowing`.
    fn narrowed(narrowing: Narrowing) -> Self {
        Self {
            text: TextQuery::of(Screen::Queue, RawQuery::default()),
            narrowing,
        }
    }

    /// Whether every row is shown: asked for, or narrowed by a text or a narrowing.
    fn shows_all(&self) -> bool {
        self.text.shows_all() || self.narrowing.narrows()
    }

    /// The address of the list, or of the ticket `selected` beside it, with this state; each value
    /// percent-encoded, and the defaults left out.
    fn address(&self, selected: Option<&str>) -> String {
        let base = self.text.address(selected);
        let pairs: Vec<String> = self
            .narrowing
            .kept_by
            .map(|id| format!("kept_by={id}"))
            .into_iter()
            .chain(
                self.narrowing
                    .touches
                    .as_ref()
                    .map(|area| format!("area={}", query_value(&area.to_string()))),
            )
            .collect();
        if pairs.is_empty() {
            return base;
        }
        let joint = if base.contains('?') { '&' } else { '?' };
        format!("{base}{joint}{}", pairs.join("&"))
    }

    /// The same state showing every row.
    fn with_all(&self) -> Self {
        Self {
            text: self.text.with_all(),
            narrowing: self.narrowing.clone(),
        }
    }

    /// The same state without the narrowing.
    fn widened(&self) -> Self {
        Self {
            text: self.text.clone(),
            narrowing: Narrowing::default(),
        }
    }
}

/// The address of the queue narrowed to exactly what `narrowing` keeps, with every other part of
/// the list's state at its default.
pub(crate) fn narrowed_address(narrowing: Narrowing) -> String {
    QueueState::narrowed(narrowing).address(None)
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
    narrowing: Result<Query<RawNarrowing>, QueryRejection>,
) -> Result<Response, ApiError> {
    let list = QueueState::of(query, narrowing);
    render(&state, &session, &here, list, Selected::Nothing).await
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
    narrowing: Result<Query<RawNarrowing>, QueryRejection>,
) -> Result<Response, ApiError> {
    let selected = path
        .ok()
        .and_then(|Path(segment)| item_in_path(&segment).ok())
        .map_or(Selected::Unreadable, Selected::Item);
    let list = QueueState::of(query, narrowing);
    render(&state, &session, &here, list, selected).await
}

/// The page in its frame: `200`, or `404` when the address selects no ticket of the queue.
async fn render(
    state: &AppState,
    session: &UiSession,
    here: &Here,
    list: QueueState,
    selected: Selected,
) -> Result<Response, ApiError> {
    let (shell, loaded, links) = shell::load(state, session, Screen::Queue, here).await?;
    let items = queue_items(&loaded.status, &links);
    let kept: BTreeSet<ItemId> = listed(&loaded.status)
        .filter(|entry| list.narrowing.keeps(entry))
        .map(|entry| entry.item.id)
        .collect();
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
        list: ListView::of(items, &kept, &list, current, &links),
        shell,
        inspector,
    };
    Ok(signed_page(session, html(status, &page)))
}

impl ListView {
    /// The list of those of `items` the narrowing kept — `kept` — in the state `list`, the ticket
    /// on `current` marked.
    fn of(
        items: Vec<QueueItemView>,
        kept: &BTreeSet<ItemId>,
        list: &QueueState,
        current: Option<ItemId>,
        links: &LinkContext,
    ) -> Self {
        let mut ready: BTreeMap<u8, Vec<Row>> = BTreeMap::new();
        let mut waiting = Vec::new();
        for item in items
            .into_iter()
            .filter(|item| kept.contains(&item.id) && list.text.leaves(&item.text()))
        {
            let priority = item.priority;
            let is_ready = item.is_ready();
            let row = Row::of(item, list, current);
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
            .map(|(priority, rows)| Group::of(Heading::Ready(priority), rows, list, current))
            .chain(
                (!waiting.is_empty()).then(|| Group::of(Heading::NotReady, waiting, list, current)),
            )
            .collect();
        Self {
            q: list.text.q.clone(),
            all: list.text.all,
            kept_by: list.narrowing.kept_by.map(|id| id.to_string()),
            area: list.narrowing.touches.as_ref().map(ToString::to_string),
            narrowed: Narrowed::of(list, links),
            close: list.address(None),
            ready: ready_count,
            waiting: waiting_count,
            groups,
        }
    }
}

impl Narrowed {
    /// What the narrowing of `list` keeps, in words, when it narrows; the claim's ticket is named
    /// as `links` knows it.
    fn of(list: &QueueState, links: &LinkContext) -> Option<Self> {
        let narrowing = &list.narrowing;
        let says: Vec<String> = narrowing
            .kept_by
            .map(|id| {
                format!(
                    "waiting for the claim on {}",
                    TicketView::of_item(id, links).label
                )
            })
            .into_iter()
            .chain(
                narrowing
                    .touches
                    .as_ref()
                    .map(|area| format!("touching {area}")),
            )
            .collect();
        narrowing.narrows().then(|| Self {
            says: says.join(" · "),
            clear: list.widened().address(None),
        })
    }
}

impl Group {
    /// The group of `rows`, cut to its first rows unless the state shows every one; `current` is
    /// kept on the address that shows them all.
    fn of(
        heading: Heading,
        mut rows: Vec<Row>,
        list: &QueueState,
        current: Option<ItemId>,
    ) -> Self {
        let total = rows.len();
        if !list.shows_all() {
            rows.truncate(GROUP_ROWS);
        }
        let more = (rows.len() < total).then(|| {
            list.with_all()
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
    fn of(item: QueueItemView, list: &QueueState, current: Option<ItemId>) -> Self {
        Self {
            href: list.address(Some(&item.id.to_string())),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kept_by: Option<&str>, area: Option<&str>) -> RawNarrowing {
        RawNarrowing {
            kept_by: kept_by.map(str::to_owned),
            area: area.map(str::to_owned),
        }
    }

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    #[test]
    fn a_narrowing_reads_an_item_id_and_an_area_and_ignores_what_does_not_read() {
        assert_eq!(raw(None, None).read(), Narrowing::default());
        assert_eq!(
            raw(Some("7"), Some("api:billing")).read(),
            Narrowing {
                kept_by: Some(ItemId(7)),
                touches: Some(area("api:billing")),
            }
        );
        for unreadable in ["", "#7", "x", "-1", "0", "7a"] {
            assert_eq!(
                raw(Some(unreadable), None).read(),
                Narrowing::default(),
                "{unreadable}"
            );
        }
        for unreadable in ["", "billing", "api:", ":billing"] {
            assert_eq!(
                raw(None, Some(unreadable)).read(),
                Narrowing::default(),
                "{unreadable}"
            );
        }
    }

    #[test]
    fn the_narrowing_is_kept_in_every_address_after_the_text() {
        let text = TextQuery::of(Screen::Queue, RawQuery::default());
        let both = QueueState {
            text: text.clone(),
            narrowing: Narrowing {
                kept_by: Some(ItemId(7)),
                touches: Some(area("api:billing")),
            },
        };
        assert_eq!(both.address(None), "/ui/queue?kept_by=7&area=api%3Abilling");
        assert_eq!(
            both.with_all().address(Some("12")),
            "/ui/queue/12?all=1&kept_by=7&area=api%3Abilling"
        );
        assert_eq!(both.widened().address(None), "/ui/queue");
        let plain = QueueState {
            text,
            narrowing: Narrowing::default(),
        };
        assert_eq!(plain.address(Some("12")), "/ui/queue/12");
    }

    #[test]
    fn a_narrowed_list_shows_every_row() {
        let text = TextQuery::of(Screen::Queue, RawQuery::default());
        let plain = QueueState {
            text: text.clone(),
            narrowing: Narrowing::default(),
        };
        assert!(!plain.shows_all());
        let narrowed = QueueState {
            text,
            narrowing: Narrowing::kept_by(ItemId(7)),
        };
        assert!(narrowed.shows_all());
    }

    #[test]
    fn the_address_of_a_narrowing_alone_names_only_it() {
        assert_eq!(
            narrowed_address(Narrowing::kept_by(ItemId(7))),
            "/ui/queue?kept_by=7"
        );
        assert_eq!(
            narrowed_address(Narrowing::touching(area("api:billing"))),
            "/ui/queue?area=api%3Abilling"
        );
    }
}
