//! The view of one claim — its ticket, holder, areas, state, pull requests and branches, and how it
//! stands — as a list row and an inspector print it, built from the status the frame read.

use std::collections::BTreeSet;

use time::{Duration, OffsetDateTime};

use dibs_app::use_cases::{HeldItem, StatusView};
use dibs_domain::handle::Handle;
use dibs_domain::handover::ClaimKind;
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemRepository;
use dibs_domain::radar::OverlapKind;
use dibs_domain::repo::RepoId;

use crate::ui::claims::classify::{finished, held_over_a_week, other_ticket, silent};
use crate::ui::links::{
    AreaView, BranchView, Link, LinkContext, LinkedText, PersonView, PullRequestView, TicketView,
};
use crate::ui::moments::{Ago, Moment, ago, moment};

/// What every claim's view is built against: the status, who is looking, when, and how the
/// references link.
pub(crate) struct Scene<'a> {
    /// The project's claims and holds.
    pub(crate) status: &'a StatusView,
    /// The member signed in.
    pub(crate) viewer: &'a Handle,
    /// The time the page is rendered at.
    pub(crate) now: OffsetDateTime,
    /// How long a claim may go unseen before it is silent.
    pub(crate) stale_after: Duration,
    /// How the references link.
    pub(crate) links: &'a LinkContext,
}

/// Whether a claim's holder works on it, waits for its pull requests, or went silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Activity {
    /// Its holder works on it.
    Working,
    /// Its work waits in pull requests.
    Review,
    /// It went silent.
    Silent,
}

impl Activity {
    /// The name of its mark's style.
    pub(crate) const fn mark(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Review => "review",
            Self::Silent => "stale",
        }
    }
}

/// One claim.
#[derive(Debug, Clone)]
pub(crate) struct ClaimView {
    /// The item's id.
    pub(crate) id: ItemId,
    /// The item's ticket.
    pub(crate) ticket: TicketView,
    /// The item's title, with the references in it linked.
    pub(crate) title: LinkedText,
    /// The tracker's own status of the item.
    pub(crate) source_status: Option<String>,
    /// The item's priority.
    pub(crate) priority: u8,
    /// The holder.
    pub(crate) holder: PersonView,
    /// The host of the session that took or last renewed it.
    pub(crate) host: String,
    /// Whether the holder is the member signed in.
    pub(crate) you: bool,
    /// Whether it is debt work.
    pub(crate) debt: bool,
    /// The areas it names as a wide claim, while it is a live one.
    pub(crate) wide: Option<WideView>,
    /// The areas it holds: none while it is silent.
    pub(crate) areas: Vec<AreaView>,
    /// The areas it waits to receive from a debt claim.
    pub(crate) pending: Vec<PendingView>,
    /// When it was first claimed.
    pub(crate) claimed_at: OffsetDateTime,
    /// The same, as a page shows it.
    pub(crate) claimed: Moment,
    /// How many whole days it has been held.
    pub(crate) held_days: i64,
    /// Its last sign of life.
    pub(crate) last_seen_at: OffsetDateTime,
    /// The same, as a page shows it.
    pub(crate) last_seen: Moment,
    /// Whether it is worked on, in review, or silent.
    pub(crate) activity: Activity,
    /// The pull requests its review recorded, each leading to this claim.
    pub(crate) prs: Vec<PullRequestView>,
    /// The full names of the merged pull requests that moved the base under it.
    pub(crate) moved_by: Vec<String>,
    /// The branch it recorded in each repository.
    pub(crate) branches: Vec<RepoBranch>,
    /// Whether its ticket is finished.
    pub(crate) finished: bool,
    /// How long it has been silent, when it is.
    pub(crate) silent: Option<Ago>,
    /// Whether it has been held over a week.
    pub(crate) held_over_a_week: bool,
    /// The ticket a branch it recorded is named after, when that is another ticket.
    pub(crate) other_ticket: Option<OtherTicket>,
}

/// A live wide claim's reason and where each area it names stands.
#[derive(Debug, Clone)]
pub(crate) struct WideView {
    /// Why it is made, with the references in it linked.
    pub(crate) reason: LinkedText,
    /// Its areas, in area order.
    pub(crate) areas: Vec<WideAreaView>,
}

/// One area a wide claim names.
#[derive(Debug, Clone)]
pub(crate) struct WideAreaView {
    /// The area.
    pub(crate) area: AreaView,
    /// The other member whose live claim holds it; `None` when the area is free.
    pub(crate) holder: Option<PersonView>,
    /// Whether that member agreed to the edits there.
    pub(crate) granted: bool,
}

/// An area a claim waits to receive from a debt claim.
#[derive(Debug, Clone)]
pub(crate) struct PendingView {
    /// The area.
    pub(crate) area: AreaView,
    /// The holder of the debt claim, while the status shows it.
    pub(crate) holder: Option<PersonView>,
}

/// The branch a claim recorded in one repository.
#[derive(Debug, Clone)]
pub(crate) struct RepoBranch {
    /// The repository.
    pub(crate) repo: String,
    /// The branch.
    pub(crate) branch: BranchView,
}

/// The ticket a branch is named after.
#[derive(Debug, Clone)]
pub(crate) struct OtherTicket {
    /// Its number.
    pub(crate) number: u64,
    /// Where `#{number}` leads, as a numbered reference of the branch's repository is linked.
    pub(crate) link: Link,
}

impl ClaimView {
    /// The view of a live or stale claim.
    pub(crate) fn of(held: &HeldItem, scene: &Scene<'_>) -> Self {
        let claim = &held.claim.claim;
        let item = &held.item.item;
        let links = scene.links;
        let home = match &item.repository {
            ItemRepository::Listed(repo) => Some(repo),
            ItemRepository::Unlisted(_) | ItemRepository::Unnamed => None,
        };
        let quiet = silent(claim, scene.now, scene.stale_after);
        let activity = if claim.state.is_in_review() {
            Activity::Review
        } else if quiet {
            Activity::Silent
        } else {
            Activity::Working
        };
        let item_repo = item
            .external_ref
            .as_ref()
            .and_then(|external| links.repository_of(external));
        let here = format!("/ui/claims/{}", item.id);
        let prs = claim
            .state
            .review()
            .map(|review| {
                review
                    .prs()
                    .iter()
                    .map(|pr| {
                        PullRequestView::of(pr, Some(here.clone()), item_repo.as_ref(), links)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let areas = if quiet {
            Vec::new()
        } else {
            claim.areas.iter().map(AreaView::of).collect()
        };
        Self {
            id: item.id,
            ticket: TicketView::of(item.id, item.external_ref.as_ref(), links),
            title: LinkedText::of(item.title.as_str(), home, links),
            source_status: item.source_status.as_ref().map(ToString::to_string),
            priority: item.priority.get(),
            holder: PersonView::of(&held.claim.holder),
            host: claim.host.to_string(),
            you: held.claim.holder == *scene.viewer,
            debt: claim.kind == ClaimKind::Debt,
            wide: wide(held, home, scene),
            areas,
            pending: claim
                .pending
                .iter()
                .map(|(area, pending)| PendingView {
                    area: AreaView::of(area),
                    holder: holder_of(scene.status, pending.from).map(PersonView::of),
                })
                .collect(),
            claimed_at: claim.claimed_at,
            claimed: moment(claim.claimed_at, scene.now),
            held_days: (scene.now - claim.claimed_at).whole_days().max(0),
            last_seen_at: claim.last_seen,
            last_seen: moment(claim.last_seen, scene.now),
            activity,
            prs,
            moved_by: moved_by(scene.status, item.id),
            branches: claim
                .branch_names()
                .map(|(repo, branch)| RepoBranch {
                    repo: repo.to_string(),
                    branch: BranchView::of(repo, branch, links),
                })
                .collect(),
            finished: finished(item),
            silent: quiet.then(|| ago(scene.now - claim.last_seen)),
            held_over_a_week: held_over_a_week(claim, item, scene.now, scene.stale_after),
            other_ticket: other_ticket(claim, item).map(|(repo, number)| OtherTicket {
                number,
                link: links.numbered(repo, number),
            }),
        }
    }

    /// The text a list's filter and the search match against: the ticket's full reference, the
    /// title, the holder, the areas, the branches, the pull requests of its review and the merged
    /// ones that moved its base.
    pub(crate) fn text(&self) -> String {
        let title: String = self
            .title
            .pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect();
        [self.ticket.title.as_str(), &title, &self.holder.handle]
            .into_iter()
            .map(str::to_owned)
            .chain(self.areas.iter().map(|area| area.full.clone()))
            .chain(
                self.branches
                    .iter()
                    .map(|branch| branch.branch.label.clone()),
            )
            .chain(self.prs.iter().map(|pr| pr.title.clone()))
            .chain(self.moved_by.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The wide claim the status shows on this claim's item, with its reason.
fn wide(held: &HeldItem, home: Option<&RepoId>, scene: &Scene<'_>) -> Option<WideView> {
    let scope = held.claim.claim.wide.as_ref()?;
    let standing = scene
        .status
        .wide
        .iter()
        .find(|wide| wide.held.item.item.id == held.item.item.id)?;
    Some(WideView {
        reason: LinkedText::of(scope.reason().as_str(), home, scene.links),
        areas: standing
            .areas
            .iter()
            .map(|area| WideAreaView {
                area: AreaView::of(&area.area),
                holder: area
                    .holder
                    .as_ref()
                    .map(|(handle, _)| PersonView::of(handle)),
                granted: area.granted,
            })
            .collect(),
    })
}

/// The full names of the merged pull requests that moved the base under `item`, each once, in
/// name order.
fn moved_by(status: &StatusView, item: ItemId) -> Vec<String> {
    status
        .overlaps
        .iter()
        .map(|named| &named.overlap)
        .filter(|overlap| {
            overlap.kind() == OverlapKind::MainMoved && overlap.items().contains(&item)
        })
        .filter_map(|overlap| overlap.pr())
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The holder of the live or stale claim on `item`, when the status shows one.
fn holder_of(status: &StatusView, item: ItemId) -> Option<&Handle> {
    status
        .live
        .iter()
        .chain(&status.stale)
        .find(|held| held.item.item.id == item)
        .map(|held| &held.claim.holder)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use time::macros::datetime;

    use dibs_app::named::{NamedClaim, NamedItem};
    use dibs_app::records::ItemRecord;
    use dibs_app::use_cases::{NamedOverlap, ReadyView};
    use dibs_domain::claim::Claim;
    use dibs_domain::forge::ForgeRegistry;
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::ids::{MemberId, SessionId};
    use dibs_domain::item::{ExternalRef, Title};
    use dibs_domain::paths::RepoPath;
    use dibs_domain::radar::{MergeMark, Overlap};
    use dibs_domain::ref_scheme::RefSchemeRegistry;
    use dibs_domain::review::{ClaimState, PrRef};
    use dibs_domain::session::HostName;
    use dibs_domain::settings::{ProjectSettings, SettingsDocument};

    use crate::ui::links::places;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-03-10 12:00:00 UTC);

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn held(id: u64) -> HeldItem {
        let item = ItemRecord::new(
            ItemId(id),
            Some(ExternalRef::parse(&format!("gh:acme/widgets#{id}")).unwrap()),
            Title::parse(&format!("Item {id}")).unwrap(),
            ItemRepository::Listed(RepoId::parse("api").unwrap()),
            MemberId(1),
            NOW - Duration::days(60),
        );
        HeldItem {
            claim: NamedClaim {
                claim: Claim {
                    item: ItemId(id),
                    member: MemberId(1),
                    session: SessionId(1),
                    host: HostName::parse("laptop").unwrap(),
                    areas: BTreeSet::new(),
                    wide: None,
                    claimed_at: NOW - Duration::hours(2),
                    renewed_at: NOW - Duration::hours(2),
                    last_seen: NOW - Duration::minutes(1),
                    state: ClaimState::Working,
                    branches: BTreeMap::new(),
                    kind: ClaimKind::Feature,
                    pending: BTreeMap::new(),
                },
                holder: handle("alice"),
            },
            item: NamedItem {
                item,
                created_by: handle("alice"),
            },
            link: None,
        }
    }

    fn overlap(kind: OverlapKind, pr: Option<&str>, items: &[u64]) -> NamedOverlap {
        let merge = pr.map(|pr| MergeMark {
            pr: PrRef::parse(pr).unwrap(),
            at: NOW,
        });
        let overlap = Overlap::new(
            kind,
            items.iter().map(|id| ItemId(*id)).collect(),
            merge,
            BTreeSet::from([RepoPath::parse("api:src/a.rs").unwrap()]),
        )
        .unwrap();
        NamedOverlap {
            overlap,
            items: Vec::new(),
        }
    }

    fn status(claims: Vec<HeldItem>, overlaps: Vec<NamedOverlap>) -> StatusView {
        StatusView {
            live: claims,
            stale: Vec::new(),
            holds: Vec::new(),
            wide: Vec::new(),
            ready: ReadyView {
                ready: Vec::new(),
                excluded: Vec::new(),
            },
            slots: Vec::new(),
            worktrees: Vec::new(),
            escalations: Vec::new(),
            integration_silent_since: None,
            overlaps,
        }
    }

    /// The views of every live claim of `status`.
    fn views(status: &StatusView) -> Vec<ClaimView> {
        let document: SettingsDocument = serde_json::from_value(serde_json::json!({
            "repos": [{"id": "api", "remote": "https://github.com/acme/widgets",
                       "areas": [{"name": "billing", "paths": ["billing/**"]}]}]
        }))
        .unwrap();
        let links = LinkContext::new(
            ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap(),
            Arc::new(RefSchemeRegistry::standard()),
            Arc::new(ForgeRegistry::standard()),
            places(status),
        );
        let viewer = handle("alice");
        let scene = Scene {
            status,
            viewer: &viewer,
            now: NOW,
            stale_after: Duration::days(1),
            links: &links,
        };
        status
            .live
            .iter()
            .map(|held| ClaimView::of(held, &scene))
            .collect()
    }

    #[test]
    fn the_text_holds_the_merged_pull_requests_that_moved_the_claims_base() {
        let status = status(
            vec![held(3), held(5), held(8)],
            vec![
                overlap(OverlapKind::MainMoved, Some("acme/widgets#7"), &[3]),
                overlap(OverlapKind::MainMoved, Some("acme/widgets#7"), &[5]),
                overlap(OverlapKind::MainMoved, Some("acme/widgets#4"), &[3]),
                overlap(OverlapKind::SameFile, None, &[5, 8]),
            ],
        );
        let claims = views(&status);
        assert_eq!(claims[0].moved_by, ["acme/widgets#4", "acme/widgets#7"]);
        assert_eq!(claims[1].moved_by, ["acme/widgets#7"]);
        assert!(claims[2].moved_by.is_empty());
        let words = |claim: &ClaimView| claim.text();
        assert!(words(&claims[0]).contains("acme/widgets#7"));
        assert!(words(&claims[1]).contains("acme/widgets#7"));
        assert!(!words(&claims[2]).contains("acme/widgets#7"));
    }
}
