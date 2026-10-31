//! The end of a session, which ends every hold it and its agents have.

use std::sync::Arc;

use dibs_domain::ids::SessionId;
use dibs_domain::worktree::decide_session_ended;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::common::own_session;
use crate::use_cases::worktrees::scene::PoolScene;
use crate::use_cases::worktrees::view::{HeldWorktree, Labels};
use crate::work::Work;

/// A session of the caller ended: every hold it, or one of its agents, has in any of the caller's
/// pools ends with it. The session itself stays registered.
pub struct EndSession {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl EndSession {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Ends the session's holds; answers them as they were, ordered by pool and worktree. A
    /// session that holds nothing ends nothing and is answered with none.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a session not the
    /// caller's; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        session: SessionId,
    ) -> Result<Vec<HeldWorktree>, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let session = own_session(&mut work, actor, session).await?.id;
        let mut ended = Vec::new();
        for mut scene in PoolScene::load_all(&mut work, Some(actor.member), now).await? {
            ended.extend(scene.apply(decide_session_ended(&scene.pool, session)));
            scene.save(&mut work, now).await?;
        }
        let ended = Labels::default().all(&mut work, ended).await?;
        work.commit(&*self.notifier).await?;
        Ok(ended)
    }
}
