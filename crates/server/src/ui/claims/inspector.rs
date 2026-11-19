//! The inspector of one claim: what it keeps out of the queue, how the queue's tickets touch its
//! areas, where its branches overlap another claim's or the base moved under them, and what was
//! written about its item; or, for a claim that is not open, what became of its item.

use std::collections::BTreeSet;

use dibs_app::named::{AuthorName, NamedSummary};
use dibs_app::records::ItemRecord;
use dibs_app::use_cases::{HeldItem, NamedNotReady};
use dibs_domain::forge::Revision;
use dibs_domain::ids::ItemId;
use dibs_domain::paths::RepoPath;
use dibs_domain::radar::OverlapKind;
use dibs_domain::repo::RepoId;

use crate::ui::claims::query::ListQuery;
use crate::ui::claims::view::{ClaimView, Scene};
use crate::ui::links::{
    AreaView, ConversationView, FileView, PersonView, PullRequestView, TicketView, query_value,
};
use crate::ui::moments::{Moment, moment};
use crate::ui::queue::listing::{Listed, listed};

/// How many files of a collision with another claim are listed.
const COLLISION_FILES: usize = 5;

/// How many files of a moved base are listed.
const MOVED_FILES: usize = 3;

/// The inspector of an open claim.
#[derive(Debug, Clone)]
pub struct InspectorView {
    /// The claim.
    pub claim: ClaimView,
    /// The address a take-over returns to, when the viewer may take the claim over.
    pub takeover: Option<String>,
    /// How many tickets of the queue wait for areas it holds.
    pub kept: usize,
    /// The address of those tickets in the queue.
    pub kept_href: String,
    /// The areas it holds, with the tickets that will touch each.
    pub areas: Vec<AreaRow>,
    /// The other claims whose branches change what its branches change.
    pub collisions: Vec<Collision>,
    /// The merges that moved the base under its branches.
    pub moved: Vec<Moved>,
    /// The conversations members started about its item.
    pub talk: Vec<Talk>,
    /// dibs's notices about its item, when there are any.
    pub notices: Option<Notices>,
}

/// An area a claim holds.
#[derive(Debug, Clone)]
pub struct AreaRow {
    /// The area.
    pub area: AreaView,
    /// How many tickets of the queue will touch it.
    pub waiting: usize,
    /// The address of those tickets in the queue.
    pub href: String,
}

/// Another claim whose branch changes the same files, or meets this one's at a contract.
#[derive(Debug, Clone)]
pub struct Collision {
    /// Whether they meet at a published contract rather than in the same files.
    pub contract: bool,
    /// The other claim's ticket.
    pub ticket: TicketView,
    /// Its holder.
    pub holder: PersonView,
    /// The first files, at this claim's branch.
    pub files: Vec<FileView>,
    /// How many more files there are.
    pub more: usize,
}

/// A merge that moved the base under a claim's branch.
#[derive(Debug, Clone)]
pub struct Moved {
    /// The merged pull request.
    pub pr: PullRequestView,
    /// The first files it changed that the branch changes too, on the default branch.
    pub files: Vec<FileView>,
    /// How many more files there are.
    pub more: usize,
}

/// A conversation a member started about the item.
#[derive(Debug, Clone)]
pub struct Talk {
    /// Who started it.
    pub person: PersonView,
    /// The conversation.
    pub conversation: ConversationView,
    /// When its last message was written.
    pub last: Moment,
}

/// dibs's notices about the item, as one line.
#[derive(Debug, Clone)]
pub struct Notices {
    /// How many there are.
    pub count: usize,
    /// How many have messages the viewer has not read.
    pub unread: usize,
    /// The address of the newest.
    pub newest: String,
}

/// The inspector of a claim that is not open.
#[derive(Debug, Clone)]
pub struct GoneView {
    /// The item's ticket, when the item exists.
    pub ticket: Option<TicketView>,
    /// The address of the item in the queue, when it waits there.
    pub queue: Option<String>,
}

impl InspectorView {
    /// The inspector of the claim `held`, whose view is `claim`, beside the list in the state
    /// `query`; `threads` are the conversations about its item.
    pub fn of(
        claim: ClaimView,
        held: &HeldItem,
        query: &ListQuery,
        threads: &[NamedSummary],
        scene: &Scene<'_>,
    ) -> Self {
        let id = claim.id;
        let status = scene.status;
        let queue: Vec<Listed<'_>> = listed(status).collect();
        let kept = queue
            .iter()
            .filter(|entry| {
                entry.reasons.iter().any(|reason| {
                    matches!(reason, NamedNotReady::AreaHeld { item, .. } if item.id == id)
                })
            })
            .count();
        let held_areas = match claim.silent {
            Some(_) => None,
            None => Some(&held.claim.claim.areas),
        };
        let areas = held_areas
            .into_iter()
            .flatten()
            .map(|area| AreaRow {
                area: AreaView::of(area),
                waiting: queue
                    .iter()
                    .filter(|entry| entry.item.areas_hint.contains(area))
                    .count(),
                href: queue_search(&area.to_string()),
            })
            .collect();
        let (collisions, moved) = overlaps(held, scene);
        let (talk, notices) = conversations(threads, scene);
        Self {
            takeover: (claim.silent.is_some() && !claim.you).then(|| query.address(Some(id))),
            kept,
            kept_href: queue_search(&claim.ticket.title),
            areas,
            collisions,
            moved,
            talk,
            notices,
            claim,
        }
    }
}

impl GoneView {
    /// What became of the item `id`: `item` when it exists, its place in the queue when it waits
    /// there.
    pub fn of(id: ItemId, item: Option<&ItemRecord>, scene: &Scene<'_>) -> Self {
        let waits = listed(scene.status).any(|entry| entry.item.id == id);
        Self {
            ticket: item.map(|item| TicketView::of(id, item.external_ref.as_ref(), scene.links)),
            queue: (item.is_some() && waits).then(|| format!("/ui/queue/{id}")),
        }
    }
}

/// The address of the queue filtered by `text`.
fn queue_search(text: &str) -> String {
    format!("/ui/queue?q={}", query_value(text))
}

/// The collisions with other claims and the moved bases the status shows on the claim's item.
fn overlaps(held: &HeldItem, scene: &Scene<'_>) -> (Vec<Collision>, Vec<Moved>) {
    let claim = &held.claim.claim;
    let id = held.item.item.id;
    let item_repo = held
        .item
        .item
        .external_ref
        .as_ref()
        .and_then(|external| scene.links.repository_of(external));
    let at_branch = |repo: &RepoId| {
        claim
            .branches
            .get(repo)
            .map_or(Revision::DefaultBranch, Revision::Branch)
    };
    let mut collisions = Vec::new();
    let mut moved = Vec::new();
    for named in scene
        .status
        .overlaps
        .iter()
        .filter(|named| named.overlap.involves(id))
    {
        let overlap = &named.overlap;
        let paths = overlap.paths();
        if let Some(pr) = overlap.pr() {
            let (files, more) = files(paths, MOVED_FILES, |_| Revision::DefaultBranch, scene);
            moved.push(Moved {
                pr: PullRequestView::of(pr, None, item_repo.as_ref(), scene.links),
                files,
                more,
            });
            continue;
        }
        let Some((other, holder)) = named.items.iter().find(|(name, _)| name.id != id) else {
            continue;
        };
        let (files, more) = files(paths, COLLISION_FILES, at_branch, scene);
        collisions.push(Collision {
            contract: overlap.kind() == OverlapKind::Contract,
            ticket: TicketView::of(other.id, other.external_ref.as_ref(), scene.links),
            holder: PersonView::of(holder),
            files,
            more,
        });
    }
    (collisions, moved)
}

/// The first `limit` of the paths as files at the revision `at` gives for their repository, and
/// how many more there are.
fn files<'a>(
    paths: &'a BTreeSet<RepoPath>,
    limit: usize,
    at: impl Fn(&'a RepoId) -> Revision<'a>,
    scene: &Scene<'_>,
) -> (Vec<FileView>, usize) {
    let shown = paths
        .iter()
        .take(limit)
        .map(|path| FileView::of(path, at(&path.repo), scene.links))
        .collect();
    (shown, paths.len().saturating_sub(limit))
}

/// The conversations members started about the item, and dibs's notices about it as one line.
fn conversations(threads: &[NamedSummary], scene: &Scene<'_>) -> (Vec<Talk>, Option<Notices>) {
    let mut talk = Vec::new();
    let mut notices: Option<Notices> = None;
    for summary in threads {
        let record = &summary.thread.record;
        let href = format!("/ui/inbox/{}", record.id);
        match &summary.thread.author {
            AuthorName::Member(handle) => talk.push(Talk {
                person: PersonView::of(handle),
                conversation: ConversationView::of(record.id, record.subject.as_str(), scene.links),
                last: moment(
                    summary.last_message_at.unwrap_or(record.created_at),
                    scene.now,
                ),
            }),
            AuthorName::Dibs => {
                let line = notices.get_or_insert(Notices {
                    count: 0,
                    unread: 0,
                    newest: href,
                });
                line.count += 1;
                line.unread += usize::from(summary.unread > 0);
            }
        }
    }
    (talk, notices)
}
