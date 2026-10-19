//! `GET /ui/board`: the board — what is ready to pick up, what is in progress, what went stale with
//! a take-over form, and what was done today — filtered by area and by member through the query
//! string, with a reload banner when the claims or the items change.
//!
//! *Ready* lists the open items nobody holds. *Done today* lists the items completed since the
//! viewer's midnight, newest first: midnight at the offset the page's script recorded, or UTC's
//! midnight, said on the page, until it has recorded one. A filter keeps what matches it: the area filter keeps
//! claims that declare that area, the member filter keeps the claims and completions of that
//! member; an item nobody holds and a completion name no area, so a set filter leaves them out.

use askama::Template;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::Deserialize;
use time::UtcOffset;

use dibs_app::named::NamedClaim;
use dibs_app::use_cases::ItemQuery;
use dibs_domain::handle::Handle;
use dibs_domain::item::ItemStatus;
use dibs_domain::settings::ProjectSettings;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::chrome::{self, Chrome, Section};
use crate::ui::live::latest_event;
use crate::ui::moments::{Ago, Moment, moment, start_of_day};
use crate::ui::render::html;
use crate::ui::rows::{ClaimRow, ItemCard, reference};
use crate::ui::session::PageSession;
use crate::ui::signed_page;
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

    /// Whether an item nobody holds is kept: only while nothing is filtered.
    pub fn keeps_unheld(&self) -> bool {
        self.area.is_none() && self.member.is_none()
    }

    /// Whether a completion by this member is kept: no area is filtered, and the member matches.
    pub fn keeps_completion(&self, member: &Handle) -> bool {
        self.area.is_none() && self.keeps_member(member)
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
    title: String,
    member: String,
    at: Moment,
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
    ready: Vec<ItemCard>,
    in_progress: Vec<ClaimRow>,
    stale: Vec<ClaimRow>,
    done: Vec<DoneCard>,
    day_in_utc: bool,
    updated: Moment,
    after: u64,
    changed_kinds: String,
    reload: String,
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
    let view = state.app.status.execute(actor).await?;
    let open = state
        .app
        .list_items
        .execute(
            actor,
            ItemQuery {
                status: Some(ItemStatus::Open),
                ..ItemQuery::default()
            },
        )
        .await?;
    let now = state.clock.now();
    let zone = viewer_offset(&headers);
    let since = start_of_day(now, zone.unwrap_or(UtcOffset::UTC));
    let completed = state.app.list_completed.execute(actor, since).await?;
    let people = state.app.list_members.execute(actor).await?;
    let signed = chrome::load(&state, &session, Section::Board).await?;
    let rows = |held: &[dibs_app::use_cases::HeldItem]| {
        held.iter()
            .filter(|held| filter.keeps_claim(&held.claim))
            .map(|held| ClaimRow::of(held, actor.member, now))
            .collect::<Vec<_>>()
    };
    let ready = if filter.keeps_unheld() {
        open.iter().map(ItemCard::of).collect()
    } else {
        Vec::new()
    };
    let done = completed
        .iter()
        .filter(|done| filter.keeps_completion(&done.member))
        .map(|done| DoneCard {
            reference: reference(done.item.item.id, done.item.item.external_ref.as_ref()),
            title: done.item.item.title.to_string(),
            member: done.member.to_string(),
            at: moment(done.at, now),
        })
        .collect();
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
        ready,
        in_progress: rows(&view.live),
        stale: rows(&view.stale),
        done,
        day_in_utc: zone.is_none(),
        updated: moment(now, now),
        after,
        changed_kinds: format!("{STATUS_CHANGES} item_created"),
        reload: reload_link(&filter),
    };
    Ok(signed_page(&session, html(StatusCode::OK, &page)))
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

/// The board's own address with the filter kept.
fn reload_link(filter: &BoardFilter) -> String {
    let mut pairs = Vec::new();
    for (key, value) in [("area", filter.area()), ("member", filter.member())] {
        if !value.is_empty() {
            pairs.push(format!("{key}={}", encode(value)));
        }
    }
    if pairs.is_empty() {
        "/ui/board".to_owned()
    } else {
        format!("/ui/board?{}", pairs.join("&"))
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
        assert!(none.keeps_unheld());
        assert!(none.keeps_completion(&Handle::parse("bob").unwrap()));
    }

    #[test]
    fn an_area_filter_keeps_the_claims_that_declare_it_and_nothing_unheld() {
        let billing = filter("api:billing", "");
        assert!(billing.keeps_claim(&claim("alice", &["api:billing", "api:search"])));
        assert!(!billing.keeps_claim(&claim("alice", &["api:search"])));
        assert!(!billing.keeps_claim(&claim("alice", &[])));
        assert!(!billing.keeps_unheld());
        assert!(!billing.keeps_completion(&Handle::parse("alice").unwrap()));
    }

    #[test]
    fn a_member_filter_keeps_that_members_claims_and_completions() {
        let bob = filter("", "bob");
        assert!(bob.keeps_claim(&claim("bob", &["api:search"])));
        assert!(!bob.keeps_claim(&claim("alice", &["api:search"])));
        assert!(bob.keeps_completion(&Handle::parse("bob").unwrap()));
        assert!(!bob.keeps_completion(&Handle::parse("alice").unwrap()));
        assert!(!bob.keeps_unheld());
        let both = filter("api:search", "bob");
        assert!(both.keeps_claim(&claim("bob", &["api:search"])));
        assert!(!both.keeps_claim(&claim("bob", &["api:billing"])));
    }

    #[test]
    fn the_reload_link_keeps_the_filter_encoded() {
        assert_eq!(reload_link(&BoardFilter::default()), "/ui/board");
        assert_eq!(
            reload_link(&filter("api:billing", "bob")),
            "/ui/board?area=api%3Abilling&member=bob"
        );
        assert_eq!(
            reload_link(&filter("", "a&b=c")),
            "/ui/board?member=a%26b%3Dc"
        );
    }
}
