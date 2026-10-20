//! `GET /ui/board`: the board — the slots, the live wide claims, what is ready to pick up, what is
//! in progress, what went stale with a take-over form, and what was done today — filtered by area
//! and by member through the query string; the slots are shown whole, whatever the filter.
//! `GET /ui/board/fragment` renders the same columns alone, for the page's script to swap in when
//! the claims, the items or the slots change.
//!
//! *Ready* lists the viewer's ready list, soonest first, each card with its priority, its hinted
//! areas and its tracker's page, and below it the open items that are not ready with what holds
//! them back: their blockers, or a hinted area another member holds. An item a live claim holds is
//! in progress instead. *Done today* lists the items completed since the viewer's midnight, newest
//! first: midnight at the offset the page's script recorded, or UTC's midnight, said on the page,
//! until it has recorded one. A filter keeps what matches it: the area filter keeps claims that
//! declare that area and items that hint it, the member filter keeps the claims and completions of
//! that member; an item nobody holds names no member and a completion no area, so those filters
//! leave them out. A wide claim is kept by an area it names, and by its member or a member holding
//! one of its areas.
//!
//! The fragment answers a request without a live session with `401`, never with the sign-in page,
//! so a script cannot mistake that page for the board; it says what it is in the `X-Dibs-Fragment`
//! header, and its forms carry the session's form token as the page's do.

use askama::Template;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use time::UtcOffset;

use dibs_app::named::NamedClaim;
use dibs_domain::handle::Handle;
use dibs_domain::settings::ProjectSettings;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::chrome::{self, Chrome, Section};
use crate::ui::live::latest_event;
use crate::ui::moments::{Ago, Moment, moment, start_of_day};
use crate::ui::render::html;
use crate::ui::rows::{ClaimRow, NotReadyRow, PriorityMark, ReadyCard, WideRow, reference};
use crate::ui::session::{PageSession, ScriptSession, UiSession};
use crate::ui::signed_page;
use crate::ui::slots::{SlotRow, SlotState};
use crate::ui::status::STATUS_CHANGES;
use crate::ui::zone::viewer_offset;

/// What the board keeps, as the query string names it. An empty value keeps everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct BoardFilter {
    area: Option<String>,
    member: Option<String>,
}

impl BoardFilter {
    /// A filter on an area (`repo:area`) and a member's handle; blank values filter nothing.
    pub fn new(area: Option<String>, member: Option<String>) -> Self {
        let set = |value: Option<String>| {
            value
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        };
        Self {
            area: set(area),
            member: set(member),
        }
    }

    /// The area filtered on, or empty.
    pub fn area(&self) -> &str {
        self.area.as_deref().unwrap_or_default()
    }

    /// The member filtered on, or empty.
    pub fn member(&self) -> &str {
        self.member.as_deref().unwrap_or_default()
    }

    /// Whether a claim is kept: it declares the area and is the member's.
    pub fn keeps_claim(&self, claim: &NamedClaim) -> bool {
        let area = self.area.as_deref().is_none_or(|area| {
            claim
                .claim
                .areas
                .iter()
                .any(|declared| declared.to_string() == area)
        });
        area && self.keeps_member(&claim.holder)
    }

    /// Whether an item nobody holds is kept: no member is filtered, and the area filtered, if any,
    /// is one of the item's hinted areas.
    pub fn keeps_unheld(&self, hints: &[String]) -> bool {
        self.member.is_none()
            && self
                .area
                .as_deref()
                .is_none_or(|area| hints.iter().any(|hint| hint == area))
    }

    /// Whether a completion by this member is kept: no area is filtered, and the member matches.
    pub fn keeps_completion(&self, member: &Handle) -> bool {
        self.area.is_none() && self.keeps_member(member)
    }

    /// Whether a wide claim is kept: it names the area, and the member is its own or holds one of
    /// its areas.
    pub fn keeps_wide(&self, row: &WideRow) -> bool {
        let area = self
            .area
            .as_deref()
            .is_none_or(|area| row.areas.iter().any(|named| named.area == area));
        let member = self.member.as_deref().is_none_or(|wanted| {
            row.holder == wanted
                || row
                    .areas
                    .iter()
                    .any(|named| named.holder.as_deref() == Some(wanted))
        });
        area && member
    }

    fn keeps_member(&self, member: &Handle) -> bool {
        self.member
            .as_deref()
            .is_none_or(|wanted| member.as_str() == wanted)
    }
}

/// A completed item on a card.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DoneCard {
    reference: String,
    link: Option<String>,
    title: String,
    member: String,
    at: Moment,
}

/// The header naming what a fragment answer holds, and its value for the board's columns.
const FRAGMENT_HEADER: HeaderName = HeaderName::from_static("x-dibs-fragment");
const BOARD_FRAGMENT: HeaderValue = HeaderValue::from_static("board");

/// The kinds of event after which the board's columns are rendered again.
const BOARD_CHANGES: &str = "item_created item_updated dependency_added dependency_removed \
     settings_changed slot_taken slot_renewed slot_queued slot_left slot_offered \
     slot_offer_lapsed slot_released slot_expired slot_force_released slot_stalled slot_resumed \
     slot_stall_taken_over escalated acked thread_started member_removed";

/// An escalation waiting for an arbiter, as the board's banner shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EscalationBanner {
    about: String,
    by: String,
    arbiters: String,
    thread: u64,
}

/// What the board shows below its filters, as the page and the fragment render it alike.
#[derive(Debug, Clone)]
struct BoardColumns {
    csrf: String,
    back: String,
    updated: Moment,
    /// The escalations the person may read that wait for an arbiter, newest first.
    escalations: Vec<EscalationBanner>,
    slots: Vec<SlotRow>,
    wide: Vec<WideRow>,
    ready: Vec<ReadyCard>,
    waiting: Vec<ReadyCard>,
    in_progress: Vec<ClaimRow>,
    stale: Vec<ClaimRow>,
    done: Vec<DoneCard>,
    day_in_utc: bool,
}

#[derive(Template)]
#[template(path = "board.html")]
struct BoardPage {
    chrome: Chrome,
    filter_area: String,
    filter_member: String,
    areas: Vec<String>,
    members: Vec<String>,
    repos: Vec<String>,
    board: BoardColumns,
    after: u64,
    changed_kinds: String,
    reload: String,
    fragment: String,
}

#[derive(Template)]
#[template(path = "board_fragment.html")]
struct BoardFragment {
    board: BoardColumns,
}

/// The board.
///
/// # Errors
/// `invalid_query` for a malformed query string; the answer for a use case's failure.
pub async fn page(
    State(state): State<AppState>,
    PageSession(session): PageSession,
    headers: HeaderMap,
    query: Result<Query<BoardFilter>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query?;
    let filter = BoardFilter::new(query.area, query.member);
    let actor = session.actor();
    let after = latest_event(&state, actor).await?;
    let board = columns(&state, &session, &headers, &filter).await?;
    let people = state.app.list_members.execute(actor).await?;
    let signed = chrome::load(&state, &session, Section::Board).await?;
    let page = BoardPage {
        chrome: signed.chrome,
        filter_area: filter.area().to_owned(),
        filter_member: filter.member().to_owned(),
        areas: declared_areas(&signed.settings),
        members: people
            .iter()
            .filter(|member| member.role.is_person())
            .map(|member| member.handle.to_string())
            .collect(),
        repos: if signed.settings.lists_repositories() {
            signed
                .settings
                .repos()
                .iter()
                .map(|repo| repo.id.to_string())
                .collect()
        } else {
            Vec::new()
        },
        board,
        after,
        changed_kinds: format!("{STATUS_CHANGES} {BOARD_CHANGES}"),
        reload: address("/ui/board", &filter),
        fragment: address("/ui/board/fragment", &filter),
    };
    Ok(signed_page(&session, html(StatusCode::OK, &page)))
}

/// The board's columns alone, filtered as the page is, for the page's script to swap in.
///
/// # Errors
/// `unauthorized` without a live session; `invalid_query` for a malformed query string; the
/// answer for a use case's failure.
pub async fn fragment(
    State(state): State<AppState>,
    ScriptSession(session): ScriptSession,
    headers: HeaderMap,
    query: Result<Query<BoardFilter>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Query(query) = query?;
    let filter = BoardFilter::new(query.area, query.member);
    let board = columns(&state, &session, &headers, &filter).await?;
    let mut response = html(StatusCode::OK, &BoardFragment { board });
    if response.status() == StatusCode::OK {
        let fields = response.headers_mut();
        fields.insert(FRAGMENT_HEADER, BOARD_FRAGMENT);
        fields.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    Ok(signed_page(&session, response))
}

/// Reads what the columns show for the session's member, kept by the filter.
async fn columns(
    state: &AppState,
    session: &UiSession,
    headers: &HeaderMap,
    filter: &BoardFilter,
) -> Result<BoardColumns, ApiError> {
    let actor = session.actor();
    let view = state.app.status.execute(actor).await?;
    let now = state.clock.now();
    let zone = viewer_offset(headers);
    let since = start_of_day(now, zone.unwrap_or(UtcOffset::UTC));
    let completed = state.app.list_completed.execute(actor, since).await?;
    let rows = |held: &[dibs_app::use_cases::HeldItem]| {
        held.iter()
            .filter(|held| filter.keeps_claim(&held.claim))
            .map(|held| ClaimRow::of(held, actor.member, now))
            .collect::<Vec<_>>()
    };
    Ok(BoardColumns {
        csrf: session.form_token(),
        back: address("/ui/board", filter),
        updated: moment(now, now),
        escalations: view
            .escalations
            .iter()
            .map(|open| EscalationBanner {
                about: open.about.clone(),
                by: open.by.to_string(),
                arbiters: open
                    .arbiters
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                thread: open.thread.0,
            })
            .collect(),
        slots: view
            .slots
            .iter()
            .map(|line| SlotRow::of(line, now))
            .collect(),
        wide: view
            .wide
            .iter()
            .map(WideRow::of)
            .filter(|row| filter.keeps_wide(row))
            .collect(),
        ready: view
            .ready
            .ready
            .iter()
            .map(ReadyCard::ready)
            .filter(|card| filter.keeps_unheld(&card.hints))
            .collect(),
        waiting: view
            .ready
            .excluded
            .iter()
            .map(ReadyCard::excluded)
            .filter(|card| !card.reasons.iter().any(NotReadyRow::is_claim))
            .filter(|card| filter.keeps_unheld(&card.hints))
            .collect(),
        in_progress: rows(&view.live),
        stale: rows(&view.stale),
        done: completed
            .iter()
            .filter(|done| filter.keeps_completion(&done.member))
            .map(|done| DoneCard {
                reference: reference(done.item.item.id, done.item.item.external_ref.as_ref()),
                link: done.link.clone(),
                title: done.item.item.title.to_string(),
                member: done.member.to_string(),
                at: moment(done.at, now),
            })
            .collect(),
        day_in_utc: zone.is_none(),
    })
}

/// Every area the settings declare, as `repo:area`, in declaration order.
fn declared_areas(settings: &ProjectSettings) -> Vec<String> {
    settings
        .repos()
        .iter()
        .flat_map(|repo| {
            repo.areas
                .iter()
                .map(|area| repo.qualify(&area.name).to_string())
        })
        .collect()
}

/// A path of the board with the filter kept in its query string.
fn address(path: &str, filter: &BoardFilter) -> String {
    let mut pairs = Vec::new();
    for (key, value) in [("area", filter.area()), ("member", filter.member())] {
        if !value.is_empty() {
            pairs.push(format!("{key}={}", encode(value)));
        }
    }
    if pairs.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{}", pairs.join("&"))
    }
}

/// A query-string value with every byte outside the unreserved set percent-encoded.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::macros::datetime;

    use dibs_domain::area::QualifiedArea;
    use dibs_domain::claim::Claim;
    use dibs_domain::ids::{ItemId, MemberId, SessionId};
    use dibs_domain::session::HostName;

    use super::*;

    fn claim(holder: &str, areas: &[&str]) -> NamedClaim {
        let at = datetime!(2026-03-02 09:00 UTC);
        NamedClaim {
            claim: Claim {
                item: ItemId(1),
                member: MemberId(2),
                session: SessionId(3),
                host: HostName::parse("laptop").unwrap(),
                areas: areas
                    .iter()
                    .map(|area| QualifiedArea::parse(area).unwrap())
                    .collect::<BTreeSet<_>>(),
                wide: None,
                claimed_at: at,
                renewed_at: at,
                last_seen: at,
            },
            holder: Handle::parse(holder).unwrap(),
        }
    }

    fn filter(area: &str, member: &str) -> BoardFilter {
        BoardFilter::new(Some(area.to_owned()), Some(member.to_owned()))
    }

    #[test]
    fn no_filter_keeps_everything() {
        let none = BoardFilter::new(None, Some("  ".to_owned()));
        assert_eq!(none, BoardFilter::default());
        assert!(none.keeps_claim(&claim("alice", &[])));
        assert!(none.keeps_unheld(&[]));
        assert!(none.keeps_completion(&Handle::parse("bob").unwrap()));
    }

    #[test]
    fn an_area_filter_keeps_the_claims_that_declare_it_and_the_items_that_hint_it() {
        let billing = filter("api:billing", "");
        assert!(billing.keeps_claim(&claim("alice", &["api:billing", "api:search"])));
        assert!(!billing.keeps_claim(&claim("alice", &["api:search"])));
        assert!(!billing.keeps_claim(&claim("alice", &[])));
        assert!(!billing.keeps_unheld(&[]));
        assert!(billing.keeps_unheld(&["api:search".to_owned(), "api:billing".to_owned()]));
        assert!(!billing.keeps_unheld(&["api:search".to_owned()]));
        assert!(!billing.keeps_completion(&Handle::parse("alice").unwrap()));
    }

    #[test]
    fn a_member_filter_keeps_that_members_claims_and_completions() {
        let bob = filter("", "bob");
        assert!(bob.keeps_claim(&claim("bob", &["api:search"])));
        assert!(!bob.keeps_claim(&claim("alice", &["api:search"])));
        assert!(bob.keeps_completion(&Handle::parse("bob").unwrap()));
        assert!(!bob.keeps_completion(&Handle::parse("alice").unwrap()));
        assert!(!bob.keeps_unheld(&[]));
        assert!(!bob.keeps_unheld(&["api:search".to_owned()]));
        let both = filter("api:search", "bob");
        assert!(both.keeps_claim(&claim("bob", &["api:search"])));
        assert!(!both.keeps_claim(&claim("bob", &["api:billing"])));
    }

    #[test]
    fn the_board_addresses_keep_the_filter_encoded() {
        assert_eq!(address("/ui/board", &BoardFilter::default()), "/ui/board");
        assert_eq!(
            address("/ui/board", &filter("api:billing", "bob")),
            "/ui/board?area=api%3Abilling&member=bob"
        );
        assert_eq!(
            address("/ui/board/fragment", &filter("", "a&b=c")),
            "/ui/board/fragment?member=a%26b%3Dc"
        );
    }

    fn wide(holder: &str, areas: &[(&str, Option<&str>)]) -> WideRow {
        WideRow {
            reference: "#1".to_owned(),
            link: None,
            title: "Upgrade".to_owned(),
            holder: holder.to_owned(),
            reason: "Swap the HTTP client".to_owned(),
            areas: areas
                .iter()
                .map(|(area, held)| crate::ui::rows::WideAreaRow {
                    area: (*area).to_owned(),
                    holder: held.map(ToOwned::to_owned),
                    granted: false,
                })
                .collect(),
        }
    }

    #[test]
    fn a_wide_claim_is_kept_by_an_area_it_names_and_by_its_member_or_a_holder() {
        let row = wide(
            "bob",
            &[("api:billing", Some("alice")), ("api:search", None)],
        );
        assert!(BoardFilter::default().keeps_wide(&row));
        assert!(filter("api:search", "").keeps_wide(&row));
        assert!(!filter("web:checkout", "").keeps_wide(&row));
        assert!(filter("", "bob").keeps_wide(&row));
        assert!(
            filter("", "alice").keeps_wide(&row),
            "alice is asked to ack"
        );
        assert!(!filter("", "carol").keeps_wide(&row));
        assert!(filter("api:billing", "alice").keeps_wide(&row));
        assert!(!filter("web:checkout", "bob").keeps_wide(&row));
    }
}
