//! One pool as a use case sees it inside its transaction, with the holds gone silent already
//! ended, so a take, a read or a release between two sweeps never sees a silent hold as standing.

use time::OffsetDateTime;

use dibs_domain::event::EventKind;
use dibs_domain::ids::{MemberId, SessionId};
use dibs_domain::worktree::{
    AgentKey, Holder, PoolKey, Silence, Transition, WorktreePool, decide_tick,
};

use crate::error::AppError;
use crate::work::Work;

/// A pool, and the events its changes record.
pub(crate) struct PoolScene {
    pub(crate) pool: WorktreePool,
    stored: WorktreePool,
    events: Vec<EventKind>,
}

impl PoolScene {
    /// Reads the pool of this key and ends the holds silent for longer than their kind of holder
    /// may be at `now`.
    pub(crate) async fn load(
        work: &mut Work,
        key: &PoolKey,
        now: OffsetDateTime,
    ) -> Result<Self, AppError> {
        let project = work.project_id();
        let stored = work.worktree_pool(project, key).await?;
        Ok(Self::ticked(stored, now))
    }

    /// Reads every pool of the project with a hold — only `member`'s when one is named — each with
    /// its silent holds ended, ordered by key.
    pub(crate) async fn load_all(
        work: &mut Work,
        member: Option<MemberId>,
        now: OffsetDateTime,
    ) -> Result<Vec<Self>, AppError> {
        let project = work.project_id();
        Ok(work
            .worktree_pools(project)
            .await?
            .into_iter()
            .filter(|pool| member.is_none_or(|member| pool.key().member == member))
            .map(|pool| Self::ticked(pool, now))
            .collect())
    }

    fn ticked(stored: WorktreePool, now: OffsetDateTime) -> Self {
        let mut scene = Self {
            pool: stored.clone(),
            stored,
            events: Vec::new(),
        };
        let ticked = decide_tick(&scene.pool, now, Silence::DEFAULT);
        scene.apply(ticked);
        scene
    }

    /// Takes a transition's pool and events, answering its answer.
    pub(crate) fn apply<A>(&mut self, transition: Transition<A>) -> A {
        self.pool = transition.pool;
        self.events.extend(transition.events);
        transition.answer
    }

    /// Writes the pool when it changed and records its events, in the order they happened;
    /// answers how many events it recorded.
    pub(crate) async fn save(
        &mut self,
        work: &mut Work,
        now: OffsetDateTime,
    ) -> Result<usize, AppError> {
        if self.pool != self.stored {
            let project = work.project_id();
            work.put_worktree_pool(project, &self.pool).await?;
            self.stored = self.pool.clone();
        }
        let events = std::mem::take(&mut self.events);
        let count = events.len();
        for event in events {
            work.record(now, event).await?;
        }
        Ok(count)
    }
}

/// The holder a request names by its session and, for one of the session's agents, the agent's key.
pub(crate) fn holder(session: SessionId, agent: Option<AgentKey>) -> Holder {
    agent.map_or(Holder::Session(session), |agent| Holder::Agent {
        session,
        agent,
    })
}
