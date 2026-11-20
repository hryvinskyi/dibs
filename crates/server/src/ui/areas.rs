//! `GET /ui/areas` and `GET /ui/areas/{area}`: the project's areas as a seat map — each repository's
//! areas, the held ones first, each seat showing who holds it — filtered by text, and an area in the
//! inspector beside it with its paths, the claims that hold it and the tickets that will probably
//! touch it.
//!
//! The segment names the area, written `repo:name`. One that does not read as that, or names an area
//! the project does not declare, answers `404` with the whole page and an inspector saying so.

mod index;
mod inspector;

use askama::Template;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;

use dibs_domain::area::QualifiedArea;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::areas::index::{AreaEntry, AreaIndex, AreaState, RepoAreas};
use crate::ui::areas::inspector::InspectorView;
use crate::ui::links::{AreaView, PersonView};
use crate::ui::queue::view::queue_items;
use crate::ui::render::html;
use crate::ui::session::{PageSession, UiSession};
use crate::ui::shell::{self, Here, Screen, ShellView};
use crate::ui::signed_page;
use crate::ui::text_query::{RawQuery, TextQuery};

/// How many areas a repository may declare before its free ones are left out of the list.
const FREE_SHOWN_UP_TO: usize = 80;

#[derive(Template)]
#[template(path = "screens/areas.html")]
struct AreasPage {
    shell: ShellView,
    list: ListView,
    inspector: Inspector,
}

/// The map of areas.
#[derive(Debug)]
struct ListView {
    /// The text the areas match.
    q: String,
    /// Whether every free area is asked for.
    all: bool,
    /// The address of the list alone, which closes the inspector.
    close: String,
    /// How many areas the project declares.
    total: usize,
    /// How many of them a live claim holds.
    held: usize,
    /// The members who hold an area, by handle.
    holders: Vec<PersonView>,
    /// The repositories with an area the text leaves.
    sections: Vec<Section>,
}

/// One repository's seats.
#[derive(Debug)]
struct Section {
    /// The repository.
    repo: String,
    /// How many of the areas shown are held.
    held: usize,
    /// How many of the areas shown are free.
    free: usize,
    /// The seats: the held areas by name, then the free ones by name.
    seats: Vec<Seat>,
    /// The address of the list showing the free areas, when this section leaves them out.
    more: Option<String>,
}

/// An area in the list.
#[derive(Debug)]
struct Seat {
    /// The area.
    area: AreaView,
    /// The address of the area in the inspector, with the list's state.
    href: String,
    /// The full name and who holds it, for which tickets.
    title: String,
    /// How the area stands.
    state: AreaState,
    /// The colour of the first holder.
    colour: Option<u8>,
    /// How many claims hold it.
    claims: usize,
    /// Whether it is the area in the inspector.
    current: bool,
}

/// What the inspector shows.
#[derive(Debug)]
enum Inspector {
    /// Nothing selected.
    Hint,
    /// A declared area.
    Area(Box<InspectorView>),
    /// An address that names no declared area.
    Gone,
}

/// Which area an address selects.
enum Selected {
    /// None.
    Nothing,
    /// This area, declared or not.
    Area(QualifiedArea),
    /// A segment that names no area.
    Unreadable,
}

/// Areas, nothing selected.
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

/// Areas with an area selected.
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
        .and_then(|Path(segment)| QualifiedArea::parse(&segment).ok())
        .map_or(Selected::Unreadable, Selected::Area);
    render(&state, &session, &here, state_of(query), selected).await
}

/// The list's state from the address's query; a query that does not read is the default state.
fn state_of(query: Result<Query<RawQuery>, QueryRejection>) -> TextQuery {
    TextQuery::of(
        Screen::Areas,
        query.map(|Query(raw)| raw).unwrap_or_default(),
    )
}

/// The page in its frame: `200`, or `404` when the address selects no declared area.
async fn render(
    state: &AppState,
    session: &UiSession,
    here: &Here,
    query: TextQuery,
    selected: Selected,
) -> Result<Response, ApiError> {
    let (shell, loaded, links) = shell::load(state, session, Screen::Areas, here).await?;
    let queue = queue_items(&loaded.status, &links);
    let index = AreaIndex::of(&loaded, &links, &queue);
    let (inspector, status, current) = match selected {
        Selected::Nothing => (Inspector::Hint, StatusCode::OK, None),
        Selected::Unreadable => (Inspector::Gone, StatusCode::NOT_FOUND, None),
        Selected::Area(area) => match index.entry(&area.to_string()) {
            Some(entry) => {
                let view = InspectorView::of(entry.clone(), &area.repo, &links);
                (
                    Inspector::Area(Box::new(view)),
                    StatusCode::OK,
                    Some(entry.area.full.clone()),
                )
            }
            None => (Inspector::Gone, StatusCode::NOT_FOUND, None),
        },
    };
    let page = AreasPage {
        list: ListView::of(&index, &query, current.as_deref()),
        shell,
        inspector,
    };
    Ok(signed_page(session, html(status, &page)))
}

/// Who holds an area and for which tickets, each claim naming its own holder: `alice for #1, bob
/// for #2`; `None` when no claim holds it. Each pair is a holder's handle and the ticket's name.
pub(crate) fn held_by<'a>(claims: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<String> {
    let named: Vec<String> = claims
        .into_iter()
        .map(|(holder, ticket)| format!("{holder} for {ticket}"))
        .collect();
    (!named.is_empty()).then(|| named.join(", "))
}

impl ListView {
    /// The map of `index` in the state `query`, the area `current` marked.
    fn of(index: &AreaIndex, query: &TextQuery, current: Option<&str>) -> Self {
        let every = || index.repos.iter().flat_map(|repo| &repo.areas);
        let mut holders: Vec<PersonView> = every()
            .flat_map(|entry| entry.holders.iter().map(|claim| claim.holder.clone()))
            .collect();
        holders.sort_by(|a, b| a.handle.cmp(&b.handle));
        holders.dedup_by(|a, b| a.handle == b.handle);
        Self {
            q: query.q.clone(),
            all: query.all,
            close: query.address(None),
            total: every().count(),
            held: every().filter(|entry| entry.state.is_held()).count(),
            holders,
            sections: index
                .repos
                .iter()
                .filter_map(|repo| Section::of(repo, query, current))
                .collect(),
        }
    }
}

impl Section {
    /// The seats of the areas of `repo` the text leaves; none when it leaves none. A repository
    /// that declares many areas leaves out its free ones unless the state asks for them.
    fn of(repo: &RepoAreas, query: &TextQuery, current: Option<&str>) -> Option<Self> {
        let mut left: Vec<&AreaEntry> = repo
            .areas
            .iter()
            .filter(|entry| query.leaves(&entry.text()))
            .collect();
        if left.is_empty() {
            return None;
        }
        left.sort_by(|a, b| a.area.label.cmp(&b.area.label));
        let (held, free): (Vec<&AreaEntry>, Vec<&AreaEntry>) =
            left.into_iter().partition(|entry| entry.state.is_held());
        let free_count = free.len();
        let leaves_free_out =
            repo.areas.len() > FREE_SHOWN_UP_TO && !query.shows_all() && free_count > 0;
        let shown_free = if leaves_free_out { Vec::new() } else { free };
        Some(Self {
            repo: repo.repo.to_string(),
            held: held.len(),
            free: free_count,
            seats: held
                .into_iter()
                .chain(shown_free)
                .map(|entry| Seat::of(entry, query, current))
                .collect(),
            more: leaves_free_out.then(|| query.with_all().address(current)),
        })
    }
}

impl Seat {
    /// The seat of an area.
    fn of(entry: &AreaEntry, query: &TextQuery, current: Option<&str>) -> Self {
        let full = &entry.area.full;
        let title = held_by(
            entry
                .holders
                .iter()
                .map(|claim| (claim.holder.handle.as_str(), claim.ticket.title.as_str())),
        )
        .map_or_else(|| format!("{full} · free"), |who| format!("{full} · {who}"));
        Self {
            href: query.address(Some(full)),
            title,
            state: entry.state,
            colour: entry.holders.first().map(|claim| claim.holder.colour),
            claims: entry.holders.len(),
            current: current == Some(full.as_str()),
            area: entry.area.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::ItemId;
    use dibs_domain::repo::RepoId;

    use crate::ui::areas::index::ClaimSummary;
    use crate::ui::links::{Link, LinkedText, TicketView};
    use crate::ui::moments::Ago;

    use super::*;

    #[test]
    fn every_holder_is_named_with_its_own_ticket() {
        assert_eq!(held_by([]), None);
        assert_eq!(held_by([("alice", "#1")]).as_deref(), Some("alice for #1"));
        assert_eq!(
            held_by([("alice", "#1"), ("bob", "#2")]).as_deref(),
            Some("alice for #1, bob for #2")
        );
    }

    fn summary(holder: &str, number: u64) -> ClaimSummary {
        ClaimSummary {
            id: ItemId(number),
            ticket: TicketView {
                label: format!("widgets#{number}"),
                title: format!("#{number}"),
                main: Link {
                    href: format!("/ui/claims/{number}"),
                    outside: false,
                },
                out: None,
                out_name: "the tracker",
            },
            title: LinkedText { pieces: Vec::new() },
            holder: PersonView::of(&Handle::parse(holder).unwrap()),
            finished: false,
            held_over_a_week: false,
            held: Ago::Days(1),
        }
    }

    fn entry(name: &str, holders: Vec<ClaimSummary>) -> AreaEntry {
        let area = QualifiedArea::parse(&format!("api:{name}")).unwrap();
        AreaEntry {
            area: AreaView::of(&area),
            qualified: area,
            paths: vec![format!("src/{name}/**")],
            depends_on: Vec::new(),
            holders: holders.clone(),
            wanted_by: Vec::new(),
            state: AreaState::of(&holders),
        }
    }

    fn repo(held: usize, free: usize) -> RepoAreas {
        let areas = (0..held)
            .map(|number| entry(&format!("held-{number:03}"), vec![summary("alice", 1)]))
            .chain((0..free).map(|number| entry(&format!("free-{number:03}"), Vec::new())))
            .collect();
        RepoAreas {
            repo: RepoId::parse("api").unwrap(),
            areas,
        }
    }

    fn unfiltered() -> TextQuery {
        TextQuery::of(Screen::Areas, RawQuery::default())
    }

    #[test]
    fn a_seat_names_every_holder_with_their_tickets() {
        let both = entry("billing", vec![summary("alice", 1), summary("bob", 2)]);
        let seat = Seat::of(&both, &unfiltered(), None);
        assert_eq!(seat.title, "api:billing · alice for #1, bob for #2");
        assert_eq!(seat.claims, 2);
        let free = Seat::of(&entry("docs", Vec::new()), &unfiltered(), None);
        assert_eq!(free.title, "api:docs · free");
    }

    #[test]
    fn a_repository_with_many_areas_offers_its_free_ones_only_while_it_leaves_some_out() {
        let with_free = Section::of(&repo(1, FREE_SHOWN_UP_TO), &unfiltered(), None).unwrap();
        assert_eq!(with_free.free, FREE_SHOWN_UP_TO);
        assert_eq!(with_free.seats.len(), 1);
        assert_eq!(with_free.more.as_deref(), Some("/ui/areas?all=1"));
        let none_free = Section::of(&repo(FREE_SHOWN_UP_TO + 1, 0), &unfiltered(), None).unwrap();
        assert_eq!(none_free.free, 0);
        assert_eq!(none_free.seats.len(), FREE_SHOWN_UP_TO + 1);
        assert_eq!(none_free.more, None);
    }
}
