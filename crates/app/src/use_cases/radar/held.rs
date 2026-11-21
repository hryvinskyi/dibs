//! Recording a claim's branch: what still holds the source a recorded branch was learned from, and
//! the writes a recording makes.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::event::EventKind;
use dibs_domain::ids::{ItemId, MemberId};
use dibs_domain::radar::{
    BranchName, BranchRecording, BranchSource, HeldWorktrees, WorktreeRef, record_branch,
};
use dibs_domain::repo::RepoId;

use crate::error::AppError;
use crate::use_cases::radar::Radar;
use crate::use_cases::worktrees::PoolScene;
use crate::work::Work;

/// The pool worktrees of a claim's member still held for the claim's item, by holds that have not
/// gone silent: holds whose item is the claim's now, named at the take or learned from an edit, so
/// a hold that learns another item stops holding this one. The pools are read only when the
/// claim's branch in the repository asked about was learned from a pool worktree, the one source
/// whose standing they decide; for any other source nothing is read and nothing is held.
pub(crate) struct HeldForItem(BTreeSet<WorktreeRef>);

impl HeldForItem {
    /// What holds the source of `claim`'s branch in `repo` at `now`.
    ///
    /// # Errors
    /// [`AppError::Store`] when the pools cannot be read.
    pub(crate) async fn read(
        work: &mut Work,
        claim: &Claim,
        repo: &RepoId,
        now: OffsetDateTime,
    ) -> Result<Self, AppError> {
        let from_worktree = claim
            .branch_source(repo)
            .and_then(BranchSource::worktree)
            .is_some();
        if !from_worktree {
            return Ok(Self(BTreeSet::new()));
        }
        let mut held = BTreeSet::new();
        for scene in PoolScene::load_all(work, Some(claim.member), now).await? {
            let key = scene.pool.key();
            held.extend(
                scene
                    .pool
                    .holds()
                    .iter()
                    .filter(|hold| hold.item.id() == Some(claim.item))
                    .map(|hold| WorktreeRef::of(key, &hold.worktree)),
            );
        }
        Ok(Self(held))
    }
}

impl HeldWorktrees for HeldForItem {
    fn holds(&self, worktree: &WorktreeRef) -> bool {
        self.0.contains(worktree)
    }
}

impl Radar<'_> {
    /// Records `branch`, learned from `source`, as the branch of `claim`'s work in `repo`, and
    /// answers the claim as it stands after. A recording that replaces nothing writes nothing; one
    /// that changes the source alone stores the claim; one that changes the branch stores the
    /// claim, appends `BranchRecorded` and drops the claim's footprint of the repository, which
    /// measured the branch before, with what it made.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub(crate) async fn record_branch(
        &self,
        work: &mut Work,
        claim: &Claim,
        repo: &RepoId,
        branch: &BranchName,
        source: BranchSource,
    ) -> Result<Claim, AppError> {
        let held = HeldForItem::read(work, claim, repo, self.now).await?;
        let project = work.project_id();
        match record_branch(claim, repo, branch, source, &held) {
            BranchRecording::Kept => Ok(claim.clone()),
            BranchRecording::SourceChanged(changed) => {
                work.save_claim(project, &changed).await?;
                Ok(changed)
            }
            BranchRecording::BranchChanged {
                claim: changed,
                recorded,
            } => {
                work.save_claim(project, &changed).await?;
                work.record(self.now, EventKind::BranchRecorded(recorded))
                    .await?;
                self.drop_footprint(work, changed.item, repo).await?;
                Ok(changed)
            }
        }
    }

    /// Records `branch`, the branch of the pool worktree `worktree` held for `item`, on `member`'s
    /// live claim on `item` as the claim's branch in `repo`, learned from that worktree; with no
    /// such claim nothing is written.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub(crate) async fn record_worktree_branch(
        &self,
        work: &mut Work,
        member: MemberId,
        item: ItemId,
        repo: &RepoId,
        branch: &BranchName,
        worktree: WorktreeRef,
    ) -> Result<(), AppError> {
        let project = work.project_id();
        let stale_after = self.settings.stale_after();
        let claim = work
            .claim(project, item)
            .await?
            .filter(|claim| claim.member == member && claim.is_live(self.now, stale_after));
        if let Some(claim) = claim {
            let source = BranchSource::Worktree(worktree);
            self.record_branch(work, &claim, repo, branch, source)
                .await?;
        }
        Ok(())
    }
}
