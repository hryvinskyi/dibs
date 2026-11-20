//! The holders' signs of life, which keep their holds from going silent, and the branches the
//! worktrees they work in are on.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::radar::{BranchName, WorktreeRef};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::repo::RepoId;
use dibs_domain::report::MachineId;
use dibs_domain::worktree::{
    AgentKey, Beaten, PoolId, PoolKey, WorktreeBeat, WorktreeName, WorktreePool, decide_beat,
};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::own_session;
use crate::use_cases::radar::Radar;
use crate::use_cases::worktrees::scene::{PoolScene, holder};
use crate::work::Work;

/// One holder seen in one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeBeatInput {
    /// The worktree.
    pub worktree: WorktreeName,
    /// The session holding it, or whose agent holds it.
    pub session: SessionId,
    /// The agent of the session that holds it; the session itself when `None`.
    pub agent: Option<AgentKey>,
    /// When the holder was last seen in it; a time after now counts as now.
    pub seen_at: OffsetDateTime,
    /// The branch the worktree is on; `None` on a detached head, or when the machine could not
    /// read it.
    pub branch: Option<BranchName>,
}

/// The beats to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeatWorktreesInput {
    /// The caller's session sending them, which may beat for any of the caller's sessions.
    pub session: SessionId,
    /// The machine the pool lives on.
    pub machine: MachineId,
    /// The pool, on that machine.
    pub pool: PoolId,
    /// The repository the pool's worktrees are of, when the machine knows it.
    pub repo: Option<RepoId>,
    /// The beats.
    pub beats: Vec<WorktreeBeatInput>,
}

/// Applies a batch of beats of the caller's own pool, sent on behalf of any of the caller's
/// sessions and their agents. A hold is seen when a beat names its worktree and its holder; a beat
/// for a worktree that is free, or that another holder has, is answered as unknown and changes
/// nothing. Holds gone silent end before the beats are applied, whenever the beats were sent, so a
/// hold ends the same way whether the sweep or this request found it silent.
///
/// A beat that names the branch its worktree is on, for a hold that names a work item, records that
/// branch on the caller's live claim on the item, if there is one, as the claim's branch in the
/// pool's repository, learned from that worktree of that pool. Nothing is recorded for a hold that
/// names no item, a beat answered as unknown, a beat without a branch, or a pool whose repository
/// is not named or not one the project lists. A branch the claim learned from a pull request it
/// still records, or from another worktree still held for the item, is kept; the same worktree
/// moves the branch it gave. A `BranchRecorded` event marks a change of branch, and the claim's
/// footprint of the repository, which measured the branch before, goes with what it made.
pub struct BeatWorktrees {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl BeatWorktrees {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Applies the beats; answers which holds they kept and which they named in vain, each pair of
    /// worktree and holder once, in the order first named.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] when the sending session,
    /// or a session a beat names, is not the caller's; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: BeatWorktreesInput,
    ) -> Result<Beaten, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        own_session(&mut work, actor, input.session).await?;
        let sessions: BTreeSet<SessionId> = input.beats.iter().map(|beat| beat.session).collect();
        for session in sessions {
            own_session(&mut work, actor, session).await?;
        }
        let (beats, branches): (Vec<WorktreeBeat>, Vec<Option<BranchName>>) = input
            .beats
            .into_iter()
            .map(|beat| {
                let seen = WorktreeBeat {
                    worktree: beat.worktree,
                    holder: holder(beat.session, beat.agent),
                    seen_at: beat.seen_at.min(now),
                };
                (seen, beat.branch)
            })
            .unzip();
        let key = PoolKey {
            member: actor.member,
            machine: input.machine,
            pool: input.pool,
        };
        let mut scene = PoolScene::load(&mut work, &key, now).await?;
        let beaten = scene.apply(decide_beat(&scene.pool, &beats));
        let worked = worked_on(&scene.pool, &beats, &branches);
        scene.save(&mut work, now).await?;
        if let Some(repo) = input.repo.filter(|_| !worked.is_empty()) {
            self.record_branches(&mut work, actor, &key, &repo, worked, now)
                .await?;
        }
        work.commit(&*self.notifier).await?;
        Ok(beaten)
    }

    /// Records each worktree's branch on the caller's live claim on the item its hold names, in a
    /// repository the project lists.
    async fn record_branches(
        &self,
        work: &mut Work,
        actor: &Actor,
        key: &PoolKey,
        repo: &RepoId,
        worked: BTreeMap<WorktreeName, (ItemId, BranchName)>,
        now: OffsetDateTime,
    ) -> Result<(), AppError> {
        let settings = work.project_settings(&self.hot_rules).await?;
        if settings.repo(repo).is_none() {
            return Ok(());
        }
        let radar = Radar {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        for (worktree, (item, branch)) in worked {
            let worktree = WorktreeRef::of(key, &worktree);
            radar
                .record_worktree_branch(work, actor.member, item, repo, &branch, worktree)
                .await?;
        }
        Ok(())
    }
}

/// Each worktree whose hold, as `pool` stands after the beats, is held by the holder a beat named
/// and names a work item, with that item and the branch named by the last of its beats that named
/// one.
fn worked_on(
    pool: &WorktreePool,
    beats: &[WorktreeBeat],
    branches: &[Option<BranchName>],
) -> BTreeMap<WorktreeName, (ItemId, BranchName)> {
    let mut worked = BTreeMap::new();
    for (beat, branch) in beats.iter().zip(branches) {
        let item = pool
            .hold(&beat.worktree)
            .filter(|hold| hold.holder == beat.holder)
            .and_then(|hold| hold.item);
        if let (Some(item), Some(branch)) = (item, branch) {
            worked.insert(beat.worktree.clone(), (item, branch.clone()));
        }
    }
    worked
}
