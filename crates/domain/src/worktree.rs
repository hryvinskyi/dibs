//! The worktree pool: a fixed set of long-lived checkouts on one member's machine, lent to that
//! member's sessions and to the agents those sessions start, one holder per worktree at a time.
//!
//! A pool is the member's, on one machine, for one checkout: two checkouts of one project on one
//! machine keep two pools. A hold is not the member's but a single holder's — a session, or one
//! agent of a session — so two agents of one person never work in one tree, and two sessions of one
//! person never share one either. Nothing notices a holder that went away, so a hold
//! ends when it is released, when its session ends, or after a silence whose length depends on the
//! kind of holder.

mod decide;
mod name;
mod state;

#[cfg(test)]
mod tests;

pub use decide::{
    Beaten, Release, TakeAnswer, TakeWorktree, Transition, WorktreeBeat, WorktreeRefused,
    decide_beat, decide_member_removed, decide_release, decide_session_ended, decide_take,
    decide_tick,
};
pub use name::{AgentKey, AgentKeyError, PoolId, PoolIdError, WorktreeName, WorktreeNameError};
pub use state::{
    Holder, HolderKind, PoolKey, Silence, WorktreeHold, WorktreeHolder, WorktreePool,
    WorktreePoolError,
};
