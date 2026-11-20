//! The frame every screen of the pages is drawn in: the rail — the screens with their counts, the
//! people holding claims, the slots, the worktrees held, who is signed in — and the data attributes
//! the page's scripts read, all read once per request.
//!
//! The position in the log is read before anything the page shows, so an event that lands between
//! two reads is delivered again by the stream rather than lost. Reading the status and the
//! worktrees applies and records what time changed about them, so loading the frame writes.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use serde::Deserialize;
use time::OffsetDateTime;

use dibs_app::use_cases::{HeldItem, InboxQuery, InboxView, MeView, StatusView, WorktreesView};
use dibs_domain::worktree::HolderKind;

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::assets::BUNDLE_HASH;
use crate::ui::inbox::summary::{unread_from_people, waiting_grants};
use crate::ui::links::{LinkContext, LinkedText, PersonView, TicketView, places};
use crate::ui::live::latest_event;
use crate::ui::moments::{Ago, Span, moment, span};
use crate::ui::now;
use crate::ui::session::UiSession;
use crate::ui::slots::SlotRow;

/// The kinds of event that change the project's claims and holds.
pub const STATUS_CHANGES: &str = "claimed renewed taken_over released completed force_released \
     reserved wide_claimed wide_granted_by_ack grant_revoked review_started pr_merged pr_closed \
     review_ended completed_by_merge area_handed_over wait_withdrawn claim_narrowed";

/// The kinds of event that change the items, the slots, the worktrees, the escalations and the
/// overlaps between claims.
pub const BOARD_CHANGES: &str = "item_created item_updated dependency_added dependency_removed \
     settings_changed slot_taken slot_renewed slot_queued slot_left slot_offered \
     slot_offer_lapsed slot_released slot_expired slot_force_released slot_stalled slot_resumed \
     slot_stall_taken_over escalated acked thread_started member_removed overlap_found \
     footprint_changed yield_requested area_handed_over wait_withdrawn claim_narrowed \
     item_reopened worktree_taken worktree_released";

/// The kinds of event that change the conversations.
const MESSAGE_CHANGES: &str = "thread_started message_posted acked";

/// A screen of the pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// What needs the viewer now.
    Now,
    /// The claims.
    Claims,
    /// What waits to be claimed.
    Queue,
    /// The areas and who holds them.
    Areas,
    /// The conversations.
    Inbox,
    /// The search over everything.
    Search,
}

impl Screen {
    /// The name the page's scripts know the screen by.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Now => "now",
            Self::Claims => "claims",
            Self::Queue => "queue",
            Self::Areas => "areas",
            Self::Inbox => "inbox",
            Self::Search => "search",
        }
    }

    /// The address of the screen.
    pub const fn path(self) -> &'static str {
        match self {
            Self::Now => "/ui/",
            Self::Claims => "/ui/claims",
            Self::Queue => "/ui/queue",
            Self::Areas => "/ui/areas",
            Self::Inbox => "/ui/inbox",
            Self::Search => "/ui/search",
        }
    }

    /// The screen's name in the navigation.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Now => "Now",
            Self::Claims => "Claims",
            Self::Queue => "Queue",
            Self::Areas => "Areas",
            Self::Inbox => "Inbox",
            Self::Search => "Search",
        }
    }

    /// Whether the screen shows a selected object beside its list.
    pub const fn has_inspector(self) -> bool {
        !matches!(self, Self::Now | Self::Search)
    }

    /// The kinds of event after which the screen offers to refresh, space separated, each once:
    /// those that change the claims and those that change the rest of the project, and on Now and
    /// Inbox those that change the conversations.
    pub fn changed_kinds(self) -> String {
        let messages = match self {
            Self::Now | Self::Inbox => MESSAGE_CHANGES,
            Self::Claims | Self::Queue | Self::Areas | Self::Search => "",
        };
        let mut seen = BTreeSet::new();
        [STATUS_CHANGES, BOARD_CHANGES, messages]
            .iter()
            .flat_map(|kinds| kinds.split_whitespace())
            .filter(|kind| seen.insert(*kind))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Where a page was asked for: its address with its query, and the slot the query opens in the
/// rail. A query that does not read leaves every slot closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Here {
    address: String,
    slot: Option<String>,
}

/// The part of a page's query the frame reads.
#[derive(Debug, Deserialize)]
struct SlotQuery {
    slot: Option<String>,
}

impl Here {
    /// The page at `address`, a path of this service with its query, with no slot opened: the page a
    /// form's answer renders in place of the one that posted it.
    pub fn at(address: String) -> Self {
        Self {
            address,
            slot: None,
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Here {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        let slot = Query::<SlotQuery>::try_from_uri(&parts.uri)
            .ok()
            .and_then(|Query(query)| query.slot);
        let address = parts
            .uri
            .path_and_query()
            .map_or_else(|| parts.uri.path().to_owned(), ToString::to_string);
        Ok(Self { address, slot })
    }
}

/// What every screen reads once per request.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The position in the log read before everything else.
    pub after: u64,
    /// The project, the member signed in and the settings.
    pub me: MeView,
    /// The project's claims, holds, ready list and slots.
    pub status: StatusView,
    /// The conversations with messages the member has not read.
    pub unread: InboxView,
    /// The worktree holds of every member.
    pub worktrees: WorktreesView,
    /// The time the page is rendered at.
    pub now: OffsetDateTime,
}

/// The frame of a page.
#[derive(Debug, Clone)]
pub struct ShellView {
    /// The bundle directory the page loads its stylesheet and script from.
    pub assets: &'static str,
    /// The project's slug.
    pub project: String,
    /// The handle of the member signed in.
    pub handle: String,
    /// The session's form token.
    pub csrf: String,
    /// The screen shown.
    pub screen: Screen,
    /// The rail.
    pub rail: RailView,
    /// The position in the log the page's script follows the stream from.
    pub after: u64,
    /// The page's own address, which reloading it asks for again.
    pub address: String,
}

/// The rail of a page.
#[derive(Debug, Clone)]
pub struct RailView {
    /// The screens, in order.
    pub nav: Vec<NavEntry>,
    /// The viewer, then every other member holding a claim, by handle.
    pub people: Vec<PersonRow>,
    /// The declared slots.
    pub slots: Vec<SlotView>,
    /// The worktrees held.
    pub worktrees: Vec<WorktreeView>,
}

/// A screen in the navigation.
#[derive(Debug, Clone)]
pub struct NavEntry {
    /// The screen.
    pub screen: Screen,
    /// Whether it is the screen shown.
    pub current: bool,
    /// What it counts, when it counts something.
    pub count: Option<NavCount>,
}

/// What a screen in the navigation counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavCount {
    /// So many of its things.
    Plain(usize),
    /// So many held.
    Held(usize),
    /// So many unread.
    Unread(usize),
    /// So many waiting for a look.
    Waiting(usize),
}

/// A member in the rail.
#[derive(Debug, Clone)]
pub struct PersonRow {
    /// The member.
    pub person: PersonView,
    /// The host of their most recently renewed claim.
    pub host: Option<String>,
    /// How many live and stale claims they hold.
    pub claims: usize,
    /// How many of those are stale.
    pub silent: usize,
}

/// A slot in the rail.
#[derive(Debug, Clone)]
pub struct SlotView {
    /// Its name.
    pub name: String,
    /// The handle of its group's holder, while it is held.
    pub holder: Option<String>,
    /// What its declaration says about it.
    pub note: Option<LinkedText>,
    /// The longest one hold lasts.
    pub max_hold: Span,
    /// Its group, when it has one.
    pub group: Option<String>,
    /// Whether the page's query opens it.
    pub open: bool,
}

/// A worktree hold in the rail.
#[derive(Debug, Clone)]
pub struct WorktreeView {
    /// The worktree.
    pub worktree: String,
    /// The member whose pool it is.
    pub holder: PersonView,
    /// Whether one of the member's agents holds it.
    pub by_agent: bool,
    /// How long ago it was taken.
    pub age: Ago,
    /// Why.
    pub purpose: LinkedText,
    /// The item it is held for, when it names one.
    pub ticket: Option<TicketView>,
}

/// Reads what the frame of `screen` shows for the session's member, the position in the log first,
/// and the link context of the request.
///
/// # Errors
/// The answer for a use case's failure.
pub async fn load(
    state: &AppState,
    session: &UiSession,
    screen: Screen,
    here: &Here,
) -> Result<(ShellView, Loaded, LinkContext), ApiError> {
    let actor = session.actor();
    let after = latest_event(state, actor).await?;
    let me = state.app.me.execute(actor).await?;
    let status = state.app.status.execute(actor).await?;
    let unread = state
        .app
        .inbox
        .execute(
            actor,
            InboxQuery {
                unread_only: true,
                unacked: false,
                deliverable: false,
            },
        )
        .await?;
    let worktrees = state.app.worktrees.execute(actor, None).await?;
    let loaded = Loaded {
        after,
        me,
        status,
        unread,
        worktrees,
        now: state.clock.now(),
    };
    let links = LinkContext::new(
        loaded.me.settings.clone(),
        state.ref_schemes.clone(),
        state.forges.clone(),
        places(&loaded.status),
    );
    let shell = ShellView {
        assets: BUNDLE_HASH,
        project: loaded.me.project.slug.to_string(),
        handle: loaded.me.member.handle.to_string(),
        csrf: session.form_token(),
        screen,
        rail: RailView::of(&loaded, &links, screen, here.slot.as_deref()),
        after: loaded.after,
        address: here.address.clone(),
    };
    Ok((shell, loaded, links))
}

impl RailView {
    /// The rail of `screen`, with the slot `slot` open.
    fn of(loaded: &Loaded, links: &LinkContext, screen: Screen, slot: Option<&str>) -> Self {
        let status = &loaded.status;
        let held_areas: BTreeSet<_> = status.holds.iter().map(|hold| &hold.area).collect();
        let unread = unread_from_people(&loaded.unread.threads, &waiting_grants(&loaded.unread));
        let entry = |shown: Screen, count: Option<NavCount>| NavEntry {
            screen: shown,
            current: shown == screen,
            count,
        };
        Self {
            nav: vec![
                entry(Screen::Now, Some(NavCount::Waiting(now::count(loaded)))),
                entry(
                    Screen::Claims,
                    Some(NavCount::Plain(status.live.len() + status.stale.len())),
                ),
                entry(
                    Screen::Queue,
                    Some(NavCount::Plain(status.ready.ready.len())),
                ),
                entry(Screen::Areas, Some(NavCount::Held(held_areas.len()))),
                entry(Screen::Inbox, Some(NavCount::Unread(unread))),
            ],
            people: people(loaded),
            slots: status
                .slots
                .iter()
                .map(|line| {
                    let row = SlotRow::of(line, loaded.now);
                    SlotView {
                        open: slot == Some(row.name.as_str()),
                        holder: row.hold.map(|hold| hold.holder),
                        note: row.note.map(|note| LinkedText::of(&note, None, links)),
                        max_hold: span(line.spec.max_hold),
                        group: row.group,
                        name: row.name,
                    }
                })
                .collect(),
            worktrees: loaded
                .worktrees
                .board
                .iter()
                .map(|hold| WorktreeView {
                    worktree: hold.worktree.to_string(),
                    holder: PersonView::of(&hold.member),
                    by_agent: hold.holder == HolderKind::Agent,
                    age: moment(hold.since, loaded.now).ago,
                    purpose: LinkedText::of(hold.purpose.as_str(), None, links),
                    ticket: hold.item.map(|item| TicketView::of_item(item, links)),
                })
                .collect(),
        }
    }
}

/// One member's claims, counted.
#[derive(Debug, Default)]
struct Tally {
    claims: usize,
    silent: usize,
    latest: Option<(OffsetDateTime, String)>,
}

impl Tally {
    fn count(&mut self, held: &HeldItem, stale: bool) {
        let claim = &held.claim.claim;
        self.claims += 1;
        self.silent += usize::from(stale);
        if self
            .latest
            .as_ref()
            .is_none_or(|(renewed, _)| claim.renewed_at > *renewed)
        {
            self.latest = Some((claim.renewed_at, claim.host.to_string()));
        }
    }
}

/// The viewer, then every other member holding a live or stale claim, by handle.
fn people(loaded: &Loaded) -> Vec<PersonRow> {
    let mut tallies: BTreeMap<_, Tally> = BTreeMap::new();
    let claims = (loaded.status.live.iter().map(|held| (held, false)))
        .chain(loaded.status.stale.iter().map(|held| (held, true)));
    for (held, stale) in claims {
        tallies
            .entry(held.claim.holder.clone())
            .or_default()
            .count(held, stale);
    }
    let viewer = &loaded.me.member.handle;
    let mine = tallies.remove(viewer).unwrap_or_default();
    std::iter::once((viewer.clone(), mine))
        .chain(tallies)
        .map(|(handle, tally)| PersonRow {
            person: PersonView::of(&handle),
            host: tally.latest.map(|(_, host)| host),
            claims: tally.claims,
            silent: tally.silent,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Screen; 6] = [
        Screen::Now,
        Screen::Claims,
        Screen::Queue,
        Screen::Areas,
        Screen::Inbox,
        Screen::Search,
    ];

    #[test]
    fn every_screen_refreshes_on_the_claims_and_the_project_and_now_and_inbox_on_messages() {
        for screen in ALL {
            let kinds = screen.changed_kinds();
            let listed: Vec<&str> = kinds.split(' ').collect();
            let distinct: BTreeSet<&str> = listed.iter().copied().collect();
            assert_eq!(listed.len(), distinct.len(), "{screen:?}: {kinds}");
            for kind in STATUS_CHANGES
                .split_whitespace()
                .chain(BOARD_CHANGES.split_whitespace())
            {
                assert!(distinct.contains(kind), "{screen:?} misses {kind}");
            }
            let messages = matches!(screen, Screen::Now | Screen::Inbox);
            assert_eq!(
                distinct.contains("message_posted"),
                messages,
                "{screen:?}: {kinds}"
            );
        }
    }

    #[test]
    fn four_screens_show_an_inspector_and_each_has_its_own_address() {
        let split: Vec<&str> = ALL
            .iter()
            .filter(|screen| screen.has_inspector())
            .map(|screen| screen.name())
            .collect();
        assert_eq!(split, ["claims", "queue", "areas", "inbox"]);
        let paths: BTreeSet<&str> = ALL.iter().map(|screen| screen.path()).collect();
        assert_eq!(paths.len(), ALL.len());
    }
}
