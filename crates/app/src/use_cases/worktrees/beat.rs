//! The holders' signs of life, which keep their holds from going silent.

use std::collections::BTreeSet;
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::ids::SessionId;
use dibs_domain::report::MachineId;
use dibs_domain::worktree::{
    AgentKey, Beaten, PoolId, PoolKey, WorktreeBeat, WorktreeName, decide_beat,
};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::common::own_session;
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
    /// The beats.
    pub beats: Vec<WorktreeBeatInput>,
}

/// Applies a batch of beats of the caller's own pool, sent on behalf of any of the caller's
/// sessions and their agents. A hold is seen when a beat names its worktree and its holder; a beat
/// for a worktree that is free, or that another holder has, is answered as unknown and changes
/// nothing. Holds gone silent end before the beats are applied, whenever the beats were sent, so a
/// hold ends the same way whether the sweep or this request found it silent.
pub struct BeatWorktrees {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl BeatWorktrees {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
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
        let beats: Vec<WorktreeBeat> = input
            .beats
            .into_iter()
            .map(|beat| WorktreeBeat {
                worktree: beat.worktree,
                holder: holder(beat.session, beat.agent),
                seen_at: beat.seen_at.min(now),
            })
            .collect();
        let key = PoolKey {
            member: actor.member,
            machine: input.machine,
            pool: input.pool,
        };
        let mut scene = PoolScene::load(&mut work, &key, now).await?;
        let beaten = scene.apply(decide_beat(&scene.pool, &beats));
        scene.save(&mut work, now).await?;
        work.commit(&*self.notifier).await?;
        Ok(beaten)
    }
}
