//! Where one pool stands: which worktree is held, by whom, and since when it was last seen in use.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::ids::{ItemId, MemberId, SessionId};
use crate::report::MachineId;
use crate::session::HostName;
use crate::slot::Purpose;
use crate::worktree::name::{AgentKey, PoolId, WorktreeName};

/// Which pool: the member it lends to, the machine it lives on and the checkout it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PoolKey {
    /// The member whose sessions and agents it serves.
    pub member: MemberId,
    /// The machine.
    pub machine: MachineId,
    /// The checkout's pool on that machine.
    pub pool: PoolId,
}

/// Who holds a worktree: a session, or one agent that a session started. A session and each of its
/// agents are different holders.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Holder {
    /// A session itself.
    Session(SessionId),
    /// One agent of a session.
    Agent {
        /// The session that started the agent.
        session: SessionId,
        /// The agent.
        agent: AgentKey,
    },
}

impl Holder {
    /// The session holding, or whose agent is holding.
    pub const fn session(&self) -> SessionId {
        match self {
            Self::Session(session) | Self::Agent { session, .. } => *session,
        }
    }

    /// The agent's key, for an agent.
    pub const fn agent(&self) -> Option<&AgentKey> {
        match self {
            Self::Session(_) => None,
            Self::Agent { agent, .. } => Some(agent),
        }
    }

    /// Whether a session or an agent holds.
    pub const fn kind(&self) -> HolderKind {
        match self {
            Self::Session(_) => HolderKind::Session,
            Self::Agent { .. } => HolderKind::Agent,
        }
    }
}

/// The kind of a holder, which is all the project's log says of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderKind {
    /// A session.
    Session,
    /// An agent of a session.
    Agent,
}

impl HolderKind {
    /// The stable name: `session` or `agent`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Agent => "agent",
        }
    }
}

/// How long a hold may go unseen before it ends, by the kind of its holder. An agent is seen at
/// every tool call it makes, so a short silence means it died or is stuck; a session waits for its
/// person between turns, which can take hours, so its silence is long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Silence {
    /// The silence that ends an agent's hold.
    pub agent: Duration,
    /// The silence that ends a session's hold.
    pub session: Duration,
}

impl Silence {
    /// Twenty minutes for an agent, twelve hours for a session.
    pub const DEFAULT: Self = Self {
        agent: Duration::minutes(20),
        session: Duration::hours(12),
    };

    /// The silence that ends a hold of this kind of holder.
    pub const fn of(&self, kind: HolderKind) -> Duration {
        match kind {
            HolderKind::Session => self.session,
            HolderKind::Agent => self.agent,
        }
    }
}

/// One worktree lent to one holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeHold {
    /// The worktree.
    pub worktree: WorktreeName,
    /// Who holds it.
    pub holder: Holder,
    /// The host of the holder's session, as people read it.
    pub host: HostName,
    /// Why it is held.
    pub purpose: Purpose,
    /// The work item it is held for, when one was named.
    pub item: Option<ItemId>,
    /// When it was taken.
    pub taken_at: OffsetDateTime,
    /// When its holder was last seen using it.
    pub last_seen: OffsetDateTime,
}

impl WorktreeHold {
    /// Whether it has gone unseen for longer than its holder's kind may at `now`.
    pub fn is_silent(&self, now: OffsetDateTime, silence: Silence) -> bool {
        now - self.last_seen > silence.of(self.holder.kind())
    }

    /// The worktree and its holder.
    pub fn held(&self) -> WorktreeHolder {
        WorktreeHolder {
            worktree: self.worktree.clone(),
            holder: self.holder.clone(),
        }
    }
}

/// A worktree and the holder it is named with: one that holds it, or one a request says holds it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorktreeHolder {
    /// The worktree.
    pub worktree: WorktreeName,
    /// The holder.
    pub holder: Holder,
}

/// Why a set of holds is not a pool.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorktreePoolError {
    /// Two holds name one worktree.
    #[error("the worktree {worktree} is held twice")]
    WorktreeHeldTwice {
        /// The worktree.
        worktree: WorktreeName,
    },
    /// One holder holds two worktrees.
    #[error("one holder holds two worktrees")]
    HolderHoldsTwo {
        /// The two worktrees.
        worktrees: (WorktreeName, WorktreeName),
    },
}

/// Every hold of one pool. A worktree is held at most once and a holder holds at most one
/// worktree; the holds are kept in the order of their worktrees' names. The pool knows no free
/// worktree: which names it has is the member's machine's to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePool {
    key: PoolKey,
    holds: Vec<WorktreeHold>,
}

impl WorktreePool {
    /// A pool where nothing is held.
    pub const fn empty(key: PoolKey) -> Self {
        Self {
            key,
            holds: Vec::new(),
        }
    }

    /// A pool of these holds, in any order.
    ///
    /// # Errors
    /// [`WorktreePoolError::WorktreeHeldTwice`] when two holds name one worktree;
    /// [`WorktreePoolError::HolderHoldsTwo`] when one holder has two holds.
    pub fn new(key: PoolKey, mut holds: Vec<WorktreeHold>) -> Result<Self, WorktreePoolError> {
        holds.sort_by(|a, b| a.worktree.cmp(&b.worktree));
        if let Some(pair) = holds
            .windows(2)
            .find(|pair| pair[0].worktree == pair[1].worktree)
        {
            return Err(WorktreePoolError::WorktreeHeldTwice {
                worktree: pair[0].worktree.clone(),
            });
        }
        let mut by_holder: Vec<&WorktreeHold> = holds.iter().collect();
        by_holder.sort_by(|a, b| a.holder.cmp(&b.holder));
        if let Some(pair) = by_holder
            .windows(2)
            .find(|pair| pair[0].holder == pair[1].holder)
        {
            return Err(WorktreePoolError::HolderHoldsTwo {
                worktrees: (pair[0].worktree.clone(), pair[1].worktree.clone()),
            });
        }
        Ok(Self { key, holds })
    }

    /// Which pool.
    pub const fn key(&self) -> &PoolKey {
        &self.key
    }

    /// The holds, in the order of their worktrees' names.
    pub fn holds(&self) -> &[WorktreeHold] {
        &self.holds
    }

    /// Whether nothing is held.
    pub fn is_empty(&self) -> bool {
        self.holds.is_empty()
    }

    /// The hold of this worktree.
    pub fn hold(&self, worktree: &WorktreeName) -> Option<&WorktreeHold> {
        self.holds.iter().find(|hold| hold.worktree == *worktree)
    }

    /// The hold of this holder.
    pub fn hold_of(&self, holder: &Holder) -> Option<&WorktreeHold> {
        self.holds.iter().find(|hold| hold.holder == *holder)
    }

    /// Adds a hold of a worktree nobody holds, by a holder that holds none, keeping the order.
    pub(super) fn insert(&mut self, hold: WorktreeHold) {
        let at = self
            .holds
            .partition_point(|kept| kept.worktree < hold.worktree);
        self.holds.insert(at, hold);
    }

    /// The hold of this worktree, to change in place.
    pub(super) fn hold_mut(&mut self, worktree: &WorktreeName) -> Option<&mut WorktreeHold> {
        self.holds
            .iter_mut()
            .find(|hold| hold.worktree == *worktree)
    }

    /// Removes and answers every hold `ends` picks, in the order of their worktrees' names.
    pub(super) fn remove_where(
        &mut self,
        mut ends: impl FnMut(&WorktreeHold) -> bool,
    ) -> Vec<WorktreeHold> {
        let (ended, kept) = std::mem::take(&mut self.holds)
            .into_iter()
            .partition(|hold| ends(hold));
        self.holds = kept;
        ended
    }
}
