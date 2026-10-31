//! Ending a hold: the member's release, of a worktree or of a holder's hold, and an admin's forced
//! release of another member's.

use std::sync::Arc;

use dibs_domain::handle::Handle;
use dibs_domain::ids::{SessionId, ThreadId};
use dibs_domain::message::Body;
use dibs_domain::report::MachineId;
use dibs_domain::session::HostName;
use dibs_domain::worktree::{
    AgentKey, PoolId, PoolKey, Release, WorktreeName, WorktreeRefused, decide_release,
};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::Names;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::{own_session, present_member_by_handle};
use crate::use_cases::slots::Notice;
use crate::use_cases::worktrees::scene::{PoolScene, holder};
use crate::use_cases::worktrees::view::{BoardHold, HeldWorktree, Labels};
use crate::work::Work;

/// What a release names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeRelease {
    /// This worktree, whichever of the member's sessions, or of their agents, holds it.
    Name(WorktreeName),
    /// Whatever worktree this holder holds: a session of the member's, or one agent of it.
    Holder {
        /// The session.
        session: SessionId,
        /// The agent of the session; the session itself when `None`.
        agent: Option<AgentKey>,
    },
}

/// The release to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseWorktreeInput {
    /// The caller's session asking.
    pub session: SessionId,
    /// The machine the pool lives on.
    pub machine: MachineId,
    /// The pool, on that machine.
    pub pool: PoolId,
    /// What to release.
    pub release: WorktreeRelease,
}

/// The member releases a hold of their own pool: one their asking session holds, or one held by
/// another of their sessions or by an agent of one, so a person can clean up after a session that
/// went away. Another member's pool is never reached: the pool is always the caller's.
pub struct ReleaseWorktree {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl ReleaseWorktree {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Releases the hold; answers it as it was.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] when the asking session,
    /// or the holder's, is not the caller's; [`AppError::Worktree`] when the worktree or the holder
    /// holds nothing; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: ReleaseWorktreeInput,
    ) -> Result<HeldWorktree, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        own_session(&mut work, actor, input.session).await?;
        let release = match input.release {
            WorktreeRelease::Name(worktree) => Release::Name(worktree),
            WorktreeRelease::Holder { session, agent } => {
                let session = own_session(&mut work, actor, session).await?.id;
                Release::Holder(holder(session, agent))
            }
        };
        let key = PoolKey {
            member: actor.member,
            machine: input.machine,
            pool: input.pool,
        };
        let mut scene = PoolScene::load(&mut work, &key, now).await?;
        let hold = scene.apply(decide_release(&scene.pool, &release, actor.member)?);
        scene.save(&mut work, now).await?;
        let held = Labels::default().held(&mut work, hold).await?;
        work.commit(&*self.notifier).await?;
        Ok(held)
    }
}

/// The forced release to make. The board names a hold by its member, host and worktree only, so
/// that is how an admin names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceReleaseWorktreeInput {
    /// The admin's session asking.
    pub session: SessionId,
    /// The member whose hold it is.
    pub member: Handle,
    /// The host the hold was taken on.
    pub host: HostName,
    /// The worktree.
    pub worktree: WorktreeName,
    /// Why, as told to the member.
    pub reason: Body,
}

/// A forced release, and the thread that told the member why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedWorktrees {
    /// The holds that ended, as the board showed them: one for each of the member's pools on that
    /// host that held the worktree, usually one.
    pub ended: Vec<BoardHold>,
    /// The thread that told the member; `None` when the admin's own hold ended.
    pub thread: Option<ThreadId>,
}

/// An admin ends a member's hold of a worktree on a host, in each of the member's pools there that
/// holds it, and the member is sent the reason.
pub struct ForceReleaseWorktree {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
}

impl ForceReleaseWorktree {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Ends the holds. A caller who is no admin is refused before anything is looked up.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] when the caller is no admin; [`AppError::NotFound`] for a session
    /// not theirs or a member the project does not have; [`AppError::Worktree`] when the member
    /// holds that worktree on that host in no pool; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: ForceReleaseWorktreeInput,
    ) -> Result<ForcedWorktrees, AppError> {
        actor.require_admin()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        own_session(&mut work, actor, input.session).await?;
        let member = present_member_by_handle(&mut work, &input.member).await?;
        let release = Release::Name(input.worktree.clone());
        let mut ended = Vec::new();
        for mut scene in PoolScene::load_all(&mut work, Some(member.id), now).await? {
            let on_host = scene
                .pool
                .hold(&input.worktree)
                .is_some_and(|hold| hold.host == input.host);
            if on_host {
                let hold = scene.apply(decide_release(&scene.pool, &release, actor.member)?);
                ended.push(BoardHold::of(member.handle.clone(), &hold));
            }
            scene.save(&mut work, now).await?;
        }
        if ended.is_empty() {
            return Err(WorktreeRefused::NotHeld.into());
        }
        let thread = if member.id == actor.member {
            None
        } else {
            let admin = Names::load(&mut work).await?.handle(actor.member)?;
            let worktree = &input.worktree;
            let host = &input.host;
            let notice = Notice {
                ids: &*self.ids,
                from: actor.member,
                to: member.id,
                subject: format!("Your hold on the worktree {worktree} was ended by an admin"),
                lead: format!(
                    "{admin}, an admin of this project, ended your hold on the worktree {worktree} on {host}. The next take on that machine may hand it out again."
                ),
                reason: input.reason,
            };
            Some(notice.send(&mut work, now).await?)
        };
        work.commit(&*self.notifier).await?;
        Ok(ForcedWorktrees { ended, thread })
    }
}
