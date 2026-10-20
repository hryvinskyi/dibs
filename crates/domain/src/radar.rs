//! The branch overlap radar. Work that collides rarely does so in one commit: it collides over the
//! days a branch spends apart from its base, on the files every branch touches, and through a
//! published interface changed in one place and used in another. The radar records which branch
//! each claim works on in each repository, keeps the paths each of those branches changes against
//! its base — its footprint, reported by an integration that reads the forge — and derives from
//! them the overlaps between live claims of different members, each announced once to the holders
//! it concerns.

mod contract;
mod footprint;
mod names;
mod overlap;

pub use contract::{Consumer, ConsumerError, Contract};
pub use footprint::{
    Footprint, FootprintBase, FootprintDigest, FootprintEntry, FootprintOutcome, FootprintReport,
    NotStored, decide_footprint, heartbeat_branches, record_branch,
};
pub use names::{BranchName, BranchNameError, CommitSha, CommitShaError};
pub use overlap::{
    Merge, MergeMark, Overlap, OverlapChanges, OverlapError, OverlapKey, OverlapKind, RadarScene,
    reconcile,
};

#[cfg(test)]
mod tests;
