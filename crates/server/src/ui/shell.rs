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
use crate::ui::moments::{Ago, Moment, Span, moment, span};
use crate::ui::now;
use crate::ui::session::UiSession;
use crate::ui::slots::{SlotRow, SlotState};

/// The kinds of event that change the project's claims and holds: who holds what, the areas a
/// wide claim may edit in, the pull requests a claim waits in and the branch it works on.
const STATUS_CHANGES: &str = "claimed renewed taken_over released completed force_released \
     reserved wide_claimed wide_granted_by_ack granted grant_revoked review_started pr_merged \
     pr_closed pr_dropped review_ended completed_by_merge completed_by_close branch_recorded \
     area_handed_over wait_withdrawn claim_narrowed released_stale";

/// The kinds of event that change the items, the slots, the worktrees, the escalations and the
/// overlaps between claims.
const PROJECT_CHANGES: &str = "item_created item_updated dependency_added dependency_removed \
     settings_changed slot_taken slot_renewed slot_queued slot_left slot_offered \
     slot_offer_lapsed slot_released slot_expired slot_force_released slot_stalled slot_resumed \
     slot_stall_taken_over escalated acked thread_started member_removed overlap_found \
     footprint_changed yield_requested area_handed_over wait_withdrawn claim_narrowed \
     item_reopened worktree_taken worktree_released released_stale";

/// The kinds of event that change the conversations: one started, written in, acknowledged,
/// deleted by an admin, or removed by the daily retention.
const MESSAGE_CHANGES: &str = "thread_started message_posted acked thread_deleted pruned";

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
}

/// The kinds of event after which a screen reads itself again, space separated, each once: those
/// that change the claims, the rest of the project and the conversations. Every screen takes them
/// all, as the rail every screen carries counts the claims, what waits on Now and the unread
/// conversations.
fn changed_kinds() -> String {
    let mut seen = BTreeSet::new();
    [STATUS_CHANGES, PROJECT_CHANGES, MESSAGE_CHANGES]
        .iter()
        .flat_map(|kinds| kinds.split_whitespace())
        .filter(|kind| seen.insert(*kind))
        .collect::<Vec<_>>()
        .join(" ")
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
    /// The kinds of event after which the page's script refreshes it, space separated.
    pub changed_kinds: String,
}

impl ShellView {
    /// The frame of a page that answers a form: the browser's address is the form's, which the
    /// page's script cannot ask for again, so no change refreshes it; the stream still tells it when
    /// the session ends.
    pub fn answering_a_form(self) -> Self {
        Self {
            changed_kinds: String::new(),
            ..self
        }
    }
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
    /// Where it stands.
    pub state: SlotState,
    /// The handle of its group's holder, while it is held.
    pub holder: Option<String>,
    /// Its group's hold.
    pub hold: Option<SlotHoldView>,
    /// The handles of the members waiting, first in line first.
    pub waiting: Vec<String>,
    /// Its group's offer.
    pub offer: Option<SlotOfferView>,
    /// What its declaration says about it.
    pub note: Option<LinkedText>,
    /// The longest one hold lasts.
    pub max_hold: Span,
    /// Its group, when it has one.
    pub group: Option<String>,
    /// Whether the page's query opens it.
    pub open: bool,
}

/// The hold of a slot's group in the rail.
#[derive(Debug, Clone)]
pub struct SlotHoldView {
    /// Why it is held.
    pub purpose: LinkedText,
    /// The slot held, when it is another slot of the group.
    pub through: Option<String>,
    /// When it ends unless renewed.
    pub until: Moment,
    /// The last sign of life of what keeps it.
    pub seen: Moment,
}

/// The offer of a slot's group in the rail.
#[derive(Debug, Clone)]
pub struct SlotOfferView {
    /// The handle of the member it is offered to.
    pub member: String,
    /// When it lapses.
    pub until: Moment,
}

/// A worktree hold in the rail.
#[derive(Debug, Clone)]
pub struct WorktreeView {
    /// The worktree.
    pub worktree: String,
    /// The member whose pool it is.
    pub holder: PersonView,
    /// The host of the session that holds it.
    pub host: String,
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
        changed_kinds: changed_kinds(),
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
                    let row = SlotRow::of(line);
                    SlotView {
                        open: slot == Some(row.name.as_str()),
                        state: row.state,
                        holder: row.holder,
                        hold: row.hold.map(|hold| SlotHoldView {
                            purpose: LinkedText::of(&hold.purpose, None, links),
                            through: hold.through,
                            until: moment(hold.until, loaded.now),
                            seen: moment(hold.last_seen, loaded.now),
                        }),
                        waiting: row.waiting,
                        offer: row.offer.map(|offer| SlotOfferView {
                            member: offer.member,
                            until: moment(offer.until, loaded.now),
                        }),
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
                    host: hold.host.to_string(),
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
    use dibs_domain::event::EventKind;

    use super::*;

    /// The name of every kind of event, read from the answer the derived parser of [`EventKind`]
    /// gives a name it does not know: the parser is generated from the enum, so it lists them all.
    fn every_kind_name() -> Vec<String> {
        let unknown = serde_json::json!({"kind": "no_such_kind", "payload": {}});
        let error = serde_json::from_value::<EventKind>(unknown)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        let (_, listed) = error
            .split_once("expected one of ")
            .unwrap_or_else(|| panic!("{error}"));
        listed
            .split(", ")
            .map(|name| name.trim_matches('`').to_owned())
            .collect()
    }

    const ALL: [Screen; 6] = [
        Screen::Now,
        Screen::Claims,
        Screen::Queue,
        Screen::Areas,
        Screen::Inbox,
        Screen::Search,
    ];

    /// The kinds that change nothing a screen or the rail shows.
    const UNSHOWN: [&str; 4] = [
        // A pending area's escalation time, which no screen shows.
        "yield_unescalated",
        // A machine's day counts, kept for admins and that member, shown on no screen.
        "hook_reported",
        // An agent's opinion of a refusal, shown on no screen.
        "feedback_given",
        // Whether a pull request merges cleanly, shown on no screen.
        "pr_state_reported",
    ];

    #[test]
    fn every_screen_refreshes_on_every_kind_of_event_that_changes_what_it_or_the_rail_shows() {
        let kinds = changed_kinds();
        let listed: Vec<&str> = kinds.split(' ').collect();
        let refreshing: BTreeSet<&str> = listed.iter().copied().collect();
        assert_eq!(listed.len(), refreshing.len(), "each once: {kinds}");
        let names = every_kind_name();
        let every: BTreeSet<&str> = names.iter().map(String::as_str).collect();
        assert!(every.len() > UNSHOWN.len(), "{names:?}");
        for kind in UNSHOWN {
            assert!(every.contains(kind), "{kind} is a kind of event");
        }
        for &kind in &every {
            assert!(
                refreshing.contains(kind) != UNSHOWN.contains(&kind),
                "{kind} is refreshed on or explicitly not shown, and not both"
            );
        }
        assert!(refreshing.is_subset(&every), "{kinds}");
        for kind in [
            "branch_recorded",
            "pr_dropped",
            "granted",
            "thread_deleted",
            "pruned",
            "message_posted",
        ] {
            assert!(refreshing.contains(kind), "{kind}");
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
