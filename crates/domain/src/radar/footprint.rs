//! Branches and footprints: which branch a claim's work is on in each repository, and the paths
//! that branch changes against its base.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use crate::area::QualifiedArea;
use crate::claim::Claim;
use crate::ids::{ItemId, MemberId, SessionId};
use crate::paths::{RelPath, RepoPath};
use crate::radar::{BranchName, CommitSha};
use crate::repo::RepoId;
use crate::settings::ProjectSettings;

/// The claim with `branch` recorded for `repo`; `None` when that is the branch it records already.
pub fn record_branch(claim: &Claim, repo: &RepoId, branch: &BranchName) -> Option<Claim> {
    if claim.branch(repo) == Some(branch) {
        return None;
    }
    let mut changed = claim.clone();
    changed.branches.insert(repo.clone(), branch.clone());
    Some(changed)
}

/// The claims a heartbeat of `session` from a checkout of `repo` on `branch` records the branch on,
/// among the claims it kept alive: those taken or last renewed in that session. One session works
/// in one checkout, so every claim of it is on that checkout's branch; a claim of another session
/// of the same machine may be in another checkout on another branch, and is left alone. Each
/// answer is the claim to store.
pub fn heartbeat_branches(
    refreshed: &[Claim],
    session: SessionId,
    repo: &RepoId,
    branch: &BranchName,
) -> Vec<Claim> {
    refreshed
        .iter()
        .filter(|claim| claim.session == session)
        .filter_map(|claim| record_branch(claim, repo, branch))
        .collect()
}

/// What the radar knows of a footprint's paths under the project's settings: the areas they lie in,
/// whether one lies in no area, and those a contract of their repository declares. It is what the
/// contract overlaps are derived from, so they never need the paths themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FootprintDigest {
    /// The declared areas the paths lie in.
    pub areas: BTreeSet<QualifiedArea>,
    /// Whether a path lies in no declared area.
    pub unzoned: bool,
    /// The paths a contract of their repository covers.
    pub contracts: BTreeSet<RelPath>,
}

impl FootprintDigest {
    /// The digest of `paths` of `repo` under `settings`. A path's area is the first, in declaration
    /// order, that contains it. A repository the settings do not list has no areas and no
    /// contracts.
    pub fn of(repo: &RepoId, paths: &BTreeSet<RelPath>, settings: &ProjectSettings) -> Self {
        let Some(spec) = settings.repo(repo) else {
            return Self {
                unzoned: !paths.is_empty(),
                ..Self::default()
            };
        };
        let mut digest = Self::default();
        for path in paths {
            match spec.area_for(path) {
                Some(area) => {
                    digest.areas.insert(spec.qualify(&area.name));
                }
                None => digest.unzoned = true,
            }
            if spec.contracts_for(path).next().is_some() {
                digest.contracts.insert(path.clone());
            }
        }
        digest
    }
}

/// The base a branch's paths are measured from: the base branch, and the commit where the branch
/// left it (the merge base of the two), with that commit's time. A merge into the base branch at a
/// later time is one the branch does not have yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootprintBase {
    /// The base branch (`main`).
    pub branch: BranchName,
    /// The merge base of the branch and its base branch.
    pub sha: CommitSha,
    /// The merge base's committer time.
    pub at: OffsetDateTime,
}

impl FootprintBase {
    /// Whether a merge into `branch` at `merged_at` came after this base: the branch measured
    /// from it does not have that merge.
    pub fn behind(&self, branch: &BranchName, merged_at: OffsetDateTime) -> bool {
        self.branch == *branch && self.at < merged_at
    }
}

/// The paths a claim's branch changes against its base in one repository, as an integration last
/// reported them, without the paths themselves: the store keeps those apart, one row each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footprint {
    /// The item of the claim.
    pub item: ItemId,
    /// The repository.
    pub repo: RepoId,
    /// The branch.
    pub branch: BranchName,
    /// What the paths are measured from.
    pub base: FootprintBase,
    /// Whether the integration was given fewer paths than the branch changes, its forge capping
    /// the list.
    pub truncated: bool,
    /// How many paths it holds.
    pub path_count: usize,
    /// What the radar reads of its paths.
    pub digest: FootprintDigest,
}

/// One path of one footprint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FootprintEntry {
    /// The item of the claim whose footprint holds it.
    pub item: ItemId,
    /// The path.
    pub path: RepoPath,
}

/// What an integration reports of a claim's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootprintReport {
    /// The item of the claim.
    pub item: ItemId,
    /// The repository.
    pub repo: RepoId,
    /// The branch it compared.
    pub branch: BranchName,
    /// What it compared against.
    pub base: FootprintBase,
    /// The paths the branch changes against that base.
    pub paths: BTreeSet<RelPath>,
    /// Whether its forge gave it fewer paths than the branch changes.
    pub truncated: bool,
}

impl FootprintReport {
    /// The most paths one report carries.
    pub const MAX_PATHS: usize = 3000;
}

/// Why a footprint was not stored. Neither is an error: an integration reads the branches between
/// two of its passes, so a claim may end or move to another branch before its report arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStored {
    /// No live claim holds the item.
    NoLiveClaim,
    /// The claim records another branch for the repository, or none.
    OtherBranch,
}

impl NotStored {
    /// The stable name: `no_live_claim` or `other_branch`.
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoLiveClaim => "no_live_claim",
            Self::OtherBranch => "other_branch",
        }
    }
}

/// What a footprint report does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FootprintOutcome {
    /// It is not stored.
    NotStored(NotStored),
    /// It says what the stored footprint says; nothing changes.
    Unchanged,
    /// It replaces the stored footprint, or is the first.
    Replaced {
        /// The footprint to store, with the report's paths.
        footprint: Footprint,
        /// The holder of the claim.
        member: MemberId,
    },
}

/// Decides a footprint report against the item's claim and the footprint stored for its
/// repository, `current` with its paths. A report is stored only for a live claim that records the
/// reported branch for the repository; one that says what is stored changes nothing.
pub fn decide_footprint(
    claim: Option<&Claim>,
    current: Option<(&Footprint, &BTreeSet<RelPath>)>,
    report: &FootprintReport,
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> FootprintOutcome {
    let Some(claim) = claim.filter(|claim| claim.is_live(now, settings.stale_after())) else {
        return FootprintOutcome::NotStored(NotStored::NoLiveClaim);
    };
    if claim.branch(&report.repo) != Some(&report.branch) {
        return FootprintOutcome::NotStored(NotStored::OtherBranch);
    }
    let same = current.is_some_and(|(footprint, paths)| {
        footprint.branch == report.branch
            && footprint.base == report.base
            && footprint.truncated == report.truncated
            && *paths == report.paths
    });
    if same {
        return FootprintOutcome::Unchanged;
    }
    FootprintOutcome::Replaced {
        member: claim.member,
        footprint: Footprint {
            item: report.item,
            repo: report.repo.clone(),
            branch: report.branch.clone(),
            base: report.base.clone(),
            truncated: report.truncated,
            path_count: report.paths.len(),
            digest: FootprintDigest::of(&report.repo, &report.paths, settings),
        },
    }
}
