//! Worktree pools as the routes answer them: the caller's own pools in full, with the machine and
//! the pool they were named by and each holder's session and agent, and every member's holds as the
//! whole project sees them, which name neither a machine, a pool, a session nor an agent.

use serde::Serialize;
use time::OffsetDateTime;

use dibs_app::use_cases::{BoardHold, HeldWorktree, OwnPool};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::message::SessionLabel;
use dibs_domain::report::MachineId;
use dibs_domain::session::HostName;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{AgentKey, HolderKind, PoolId, WorktreeHolder, WorktreeName};

/// A hold of one of the caller's own pools.
#[derive(Debug, Serialize)]
pub struct OwnHoldView {
    /// The worktree.
    pub worktree: WorktreeName,
    /// `session` or `agent`.
    pub holder: HolderKind,
    /// The session holding it, or whose agent holds it.
    pub session_id: SessionId,
    /// That session's label.
    pub session: SessionLabel,
    /// The agent holding it; `null` when the session itself does.
    pub agent: Option<AgentKey>,
    /// The host of the holder's session.
    pub host: HostName,
    /// Why.
    pub purpose: Purpose,
    /// The work item it is held for, when one was named.
    pub item: Option<ItemId>,
    /// When it was taken.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub taken_at: OffsetDateTime,
    /// When its holder was last seen using it.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub last_seen: OffsetDateTime,
}

impl OwnHoldView {
    /// The view of a hold of the caller's own pool.
    pub fn of(held: HeldWorktree) -> Self {
        let hold = held.hold;
        Self {
            holder: hold.holder.kind(),
            session_id: hold.holder.session(),
            agent: hold.holder.agent().cloned(),
            session: held.session,
            worktree: hold.worktree,
            host: hold.host,
            purpose: hold.purpose,
            item: hold.item,
            taken_at: hold.taken_at,
            last_seen: hold.last_seen,
        }
    }
}

/// One of the caller's own pools: the machine and the pool its client named it by, and its holds.
#[derive(Debug, Serialize)]
pub struct OwnPoolView {
    /// The machine the pool lives on.
    pub machine: MachineId,
    /// The pool, on that machine.
    pub pool: PoolId,
    /// Its holds, ordered by worktree.
    pub holds: Vec<OwnHoldView>,
}

impl OwnPoolView {
    /// The view of one of the caller's pools.
    pub fn of(own: OwnPool) -> Self {
        Self {
            machine: own.key.machine,
            pool: own.key.pool,
            holds: own.holds.into_iter().map(OwnHoldView::of).collect(),
        }
    }
}

/// A hold as every member of the project sees it.
#[derive(Debug, Serialize)]
pub struct BoardHoldView {
    /// The member whose pool it is.
    pub member: Handle,
    /// The host of the holder's session.
    pub host: HostName,
    /// The worktree.
    pub worktree: WorktreeName,
    /// `session` or `agent`.
    pub holder: HolderKind,
    /// Why.
    pub purpose: Purpose,
    /// The work item it is held for, when one was named.
    pub item: Option<ItemId>,
    /// When it was taken.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub since: OffsetDateTime,
}

impl BoardHoldView {
    /// The view of a hold as the board shows it.
    pub fn of(hold: BoardHold) -> Self {
        Self {
            member: hold.member,
            host: hold.host,
            worktree: hold.worktree,
            holder: hold.holder,
            purpose: hold.purpose,
            item: hold.item,
            since: hold.since,
        }
    }
}

/// A worktree and a holder a beat named: the session and, for an agent, its key.
#[derive(Debug, Serialize)]
pub struct BeatenView {
    /// The worktree.
    pub worktree: WorktreeName,
    /// The holder's session.
    pub holder_session: SessionId,
    /// The holder's agent; `null` for the session itself.
    pub agent: Option<AgentKey>,
}

impl BeatenView {
    /// The view of a worktree and its holder.
    pub fn of(pair: WorktreeHolder) -> Self {
        Self {
            holder_session: pair.holder.session(),
            agent: pair.holder.agent().cloned(),
            worktree: pair.worktree,
        }
    }
}
