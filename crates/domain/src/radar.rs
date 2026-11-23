//! The branch overlap radar. Work that collides rarely does so in one commit: it collides over the
//! days a branch spends apart from its base, on the files every branch touches, and through a
//! published interface changed in one place and used in another. The radar records which branch
//! each claim works on in each repository, keeps the paths each of those branches changes against
//! its base — its footprint, reported by an integration that reads the forge — and derives from
//! them the overlaps between live claims of different members, each announced once to the holders
//! it concerns, and, for two colliding claims, which of them lands first.

mod checkout;
mod contract;
mod footprint;
mod names;
mod order;
mod overlap;
mod source;

pub use checkout::{SHARED_FOR, checkout_candidate, checkout_claim};
pub use contract::{Consumer, ConsumerError, Contract};
pub use footprint::{
    Footprint, FootprintBase, FootprintDigest, FootprintEntry, FootprintOutcome, FootprintReport,
    NotStored, decide_footprint,
};
pub use names::{BranchName, BranchNameError, CommitSha, CommitShaError};
pub use order::{Contender, Landing, OrderError, OrderRule, OverlapOrder, order};
pub use overlap::{
    BASE_MOVED_TOLD_EVERY, Merge, MergeMark, Overlap, OverlapChanges, OverlapError, OverlapKey,
    OverlapKind, RadarScene, reconcile,
};
pub use source::{
    BranchRecording, BranchSource, BranchSourceError, BranchSourceKind, HeldWorktrees,
    RecordedBranch, WorktreeRef, record_branch,
};

#[cfg(test)]
mod tests;
