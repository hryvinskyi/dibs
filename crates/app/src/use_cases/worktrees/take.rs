//! Taking a worktree of one of the caller's pools, for one of their sessions or an agent of it.

use std::sync::Arc;

use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::report::MachineId;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{
    AgentKey, PoolId, PoolKey, TakeAnswer, TakeWorktree as TakeRequest, WorktreeName, decide_take,
};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::common::{existing_item, own_session};
use crate::use_cases::worktrees::scene::{PoolScene, holder};
use crate::use_cases::worktrees::view::{HeldWorktree, Labels};
use crate::work::Work;

/// The take to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeWorktreeInput {
    /// The caller's session asking; it holds the worktree, or its agent does.
    pub session: SessionId,
    /// The machine the pool lives on.
    pub machine: MachineId,
    /// The pool, on that machine.
    pub pool: PoolId,
    /// The worktrees that may be handed out, best first.
    pub names: Vec<WorktreeName>,
    /// The agent of the session that is to hold it; the session itself when `None`.
    pub agent: Option<AgentKey>,
    /// Why.
    pub purpose: Purpose,
    /// The work item it is for, by local id.
    pub item: Option<ItemId>,
}

/// What the take came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TookWorktree {
    /// The holder holds this worktree.
    Took {
        /// The worktree.
        worktree: WorktreeName,
        /// Whether the holder held it already before asking: then nothing about it changed but
        /// that it was seen now.
        again: bool,
    },
    /// Every worktree named is held by another holder.
    Busy {
        /// The hold of each, in the order the take named them.
        held: Vec<HeldWorktree>,
    },
}

/// Takes a worktree of the caller's pool on a machine: a holder that holds one gets it again;
/// otherwise the first worktree named that nobody holds becomes theirs; otherwise the answer names
/// who holds each. A session and each of its agents are different holders, each holding one
/// worktree at most. The pool is the caller's: a member never takes from another member's pool.
pub struct TakeWorktree {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl TakeWorktree {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Makes the take.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a session not the
    /// caller's or an unknown item; [`AppError::Worktree`] when no worktree is named or one is
    /// named twice; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: TakeWorktreeInput,
    ) -> Result<TookWorktree, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let session = own_session(&mut work, actor, input.session).await?;
        if let Some(item) = input.item {
            existing_item(&mut work, item).await?;
        }
        let key = PoolKey {
            member: actor.member,
            machine: input.machine,
            pool: input.pool,
        };
        let mut scene = PoolScene::load(&mut work, &key, now).await?;
        let request = TakeRequest {
            names: input.names,
            holder: holder(session.id, input.agent),
            host: session.host,
            purpose: input.purpose,
            item: input.item,
        };
        let transition = decide_take(&scene.pool, &request, now)?;
        let took = match scene.apply(transition) {
            TakeAnswer::Took { worktree, again } => TookWorktree::Took { worktree, again },
            TakeAnswer::Busy { held } => {
                let holds = held
                    .iter()
                    .filter_map(|held| scene.pool.hold(&held.worktree).cloned());
                TookWorktree::Busy {
                    held: Labels::default().all(&mut work, holds).await?,
                }
            }
        };
        scene.save(&mut work, now).await?;
        work.commit(&*self.notifier).await?;
        Ok(took)
    }
}
