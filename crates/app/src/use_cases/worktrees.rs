//! The use cases of worktree pools: taking a worktree for a session or one of its agents,
//! releasing it, an admin's forced release, the holders' beats, the end of a session, reading the
//! pools, and the sweep. Each runs in one transaction and ends the holds gone silent before it
//! decides.

mod beat;
mod end_session;
mod list;
mod release;
mod scene;
mod take;
mod view;

#[cfg(test)]
mod tests;

pub use beat::{BeatWorktrees, BeatWorktreesInput, WorktreeBeatInput};
pub use end_session::EndSession;
pub use list::{TickWorktrees, WorktreeSweepReport, Worktrees};
pub use release::{
    ForceReleaseWorktree, ForceReleaseWorktreeInput, ForcedWorktrees, ReleaseWorktree,
    ReleaseWorktreeInput, WorktreeRelease,
};
pub use take::{TakeWorktree, TakeWorktreeInput, TookWorktree};
pub use view::{BoardHold, HeldWorktree, OwnPool, WorktreesView};

pub(crate) use scene::PoolScene;
pub(crate) use view::own_pools;
