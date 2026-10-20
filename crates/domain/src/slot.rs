//! Slots: named shared actions one member does at a time — a deploy, a release, a migration of a
//! shared database. A claim says what someone works on; a slot says who may act now.
//!
//! A slot hold is a member's, like a claim: every session of the member acts on it. Slots of one
//! group exclude each other. When a hold ends, the first member waiting for its group is offered
//! the slot for a while; a take from anyone else meanwhile waits behind them. How a hold stays
//! alive is fixed when it is taken: by the member's heartbeats, by the heartbeats of one run of a
//! command (stalled, not ended, when they stop), or until a forge workflow run finishes.

mod command;
mod decide;
mod spec;
mod state;
mod verdict;

#[cfg(test)]
mod tests;

pub use command::{MAX_COMMAND_BYTES, SimpleCommand, split_commands};
pub use decide::{
    BeatRefused, SlotRefused, TakeAnswer, TakeRequest, Transition, decide_force_release,
    decide_leave, decide_release, decide_run_beat, decide_run_finished, decide_slots_removed,
    decide_stall_takeover, decide_take, decide_tick,
};
pub use spec::{
    CommandPattern, DEFAULT_MAX_WAIT, DEFAULT_OFFER_FOR, GroupKey, MAX_HOLD_RANGE,
    MAX_PATTERN_CHARS, MAX_PATTERNS, PATTERN_SIZE_LIMIT, PatternError, Purpose, PurposeError,
    RepoRef, RepoRefError, SlotCatalog, SlotGroup, SlotGroupError, SlotName, SlotNameError,
    SlotSpec,
};
pub use state::{
    CommitSha, CommitShaError, EndWord, EndWordError, Liveness, RunExit, STALL_AFTER, SlotBoard,
    SlotHold, SlotOffer, SlotStanding, SlotWaiter, WorkflowName, WorkflowNameError,
};
pub use verdict::{CommandVerdict, command_verdict, push_targets, push_verdict, slots_in_question};
