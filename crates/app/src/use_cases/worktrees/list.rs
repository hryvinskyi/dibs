//! Reading the pools, and the sweep that ends silent holds when nobody asks.

use std::sync::Arc;

use dibs_domain::ids::ProjectId;
use dibs_domain::report::MachineId;
use dibs_domain::worktree::PoolKey;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::Names;
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::worktrees::scene::PoolScene;
use crate::use_cases::worktrees::view::{WorktreesView, board, own_pools};
use crate::work::Work;

/// The caller's own pools in full, and every member's holds as the board shows them. An
/// integration may read them; it has no pool of its own. Reading ends the silent holds first, and
/// records it.
pub struct Worktrees {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl Worktrees {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// The pools now; the caller's own only on `machine` when one is named, the board whole.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        machine: Option<&MachineId>,
    ) -> Result<WorktreesView, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let mut scenes = PoolScene::load_all(&mut work, None, now).await?;
        for scene in &mut scenes {
            scene.save(&mut work, now).await?;
        }
        let pools: Vec<_> = scenes
            .into_iter()
            .map(|scene| scene.pool)
            .filter(|pool| !pool.is_empty())
            .collect();
        let mine = pools.iter().filter(|pool| {
            let key = pool.key();
            key.member == actor.member && machine.is_none_or(|machine| key.machine == *machine)
        });
        let mine = own_pools(&mut work, mine).await?;
        let names = Names::load(&mut work).await?;
        let board = board(&names, &pools)?;
        work.commit(&*self.notifier).await?;
        Ok(WorktreesView { mine, board })
    }
}

/// What one sweep did.
#[derive(Debug, Default)]
pub struct WorktreeSweepReport {
    /// How many pools had a hold.
    pub pools: usize,
    /// How many events the sweep recorded.
    pub events: usize,
    /// The pools the sweep could not apply, each with its project and why; the others were
    /// applied all the same.
    pub failed: Vec<(ProjectId, PoolKey, AppError)>,
}

/// Ends the silent holds of every pool that has a hold, each pool in a transaction of its own, so
/// a hold whose holder went away ends even when nobody touches its pool. The service runs it on a
/// schedule; it acts for nobody.
pub struct TickWorktrees {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl TickWorktrees {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Sweeps every busy pool once.
    ///
    /// # Errors
    /// [`AppError::Store`] when the busy pools cannot be listed; a pool that fails is in the report
    /// instead.
    pub async fn execute(&self) -> Result<WorktreeSweepReport, AppError> {
        let mut tx = self.store.begin().await?;
        let pools = tx.busy_worktree_pools().await?;
        drop(tx);
        let mut report = WorktreeSweepReport {
            pools: pools.len(),
            ..WorktreeSweepReport::default()
        };
        for (project, key) in pools {
            match self.sweep(project, &key).await {
                Ok(events) => report.events += events,
                Err(error) => report.failed.push((project, key, error)),
            }
        }
        Ok(report)
    }

    async fn sweep(&self, project: ProjectId, key: &PoolKey) -> Result<usize, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_unattended(&*self.store, project).await?;
        let mut scene = PoolScene::load(&mut work, key, now).await?;
        let events = scene.save(&mut work, now).await?;
        work.commit(&*self.notifier).await?;
        Ok(events)
    }
}
