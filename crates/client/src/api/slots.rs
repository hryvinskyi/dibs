//! The requests and answers of the slot routes.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::Note;
use dibs_domain::ids::{ItemId, RunId, SessionId, ThreadId};
use dibs_domain::session::HostName;
use dibs_domain::slot::{
    CommitSha, Liveness, Purpose, RepoRef, RunExit, SlotGroup, SlotName, WorkflowName,
};

/// A hold of a slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotHold {
    /// The slot held: the one it is shown under, or another slot of its group.
    pub slot: SlotName,
    /// The member holding it.
    pub holder: Handle,
    /// Why.
    pub purpose: Purpose,
    /// The work item, when one was named.
    pub item: Option<ItemId>,
    /// The session that took or last renewed it.
    pub session_id: SessionId,
    /// That session's machine.
    pub host: HostName,
    /// How it stays alive.
    pub liveness: Liveness,
    /// When it was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub taken_at: OffsetDateTime,
    /// When it ends unless renewed.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    /// The last sign of life of what keeps it.
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
    /// Whether its run stopped sending heartbeats.
    pub stalled: bool,
}

/// A place in a slot group's queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotWaiter {
    /// The member waiting.
    pub member: Handle,
    /// The slot they asked for.
    pub slot: SlotName,
    /// Why they want it.
    pub purpose: Purpose,
    /// When they joined the queue.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    /// Their last sign of life.
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
}

/// A slot group's offer to the member first in its queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotOffer {
    /// The member.
    pub member: Handle,
    /// The slot they asked for.
    pub slot: SlotName,
    /// Why they want it.
    pub purpose: Purpose,
    /// When it lapses.
    #[serde(with = "time::serde::rfc3339")]
    pub until: OffsetDateTime,
}

/// A declared slot and where its group stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    /// Its name.
    pub name: SlotName,
    /// Its group.
    pub group: Option<SlotGroup>,
    /// The longest one hold lasts.
    pub max_hold: String,
    /// How long an offer stands.
    pub offer_for: String,
    /// How long a waiter keeps their place without a sign of life.
    pub max_wait: String,
    /// Whether its commands run only inside `dibs slot run`.
    pub run_only: bool,
    /// The refs nobody but the holder may push while it is held.
    #[serde(default)]
    pub freezes: Vec<RepoRef>,
    /// Its note.
    pub note: Option<Note>,
    /// Its group's hold.
    pub hold: Option<SlotHold>,
    /// Its group's queue, first in line first.
    #[serde(default)]
    pub queue: Vec<SlotWaiter>,
    /// Its group's offer.
    pub offer: Option<SlotOffer>,
}

impl Slot {
    /// The member's place in the queue, from 1.
    pub fn position_of(&self, member: &Handle) -> Option<usize> {
        self.queue
            .iter()
            .position(|waiter| waiter.member == *member)
            .map(|index| index + 1)
    }
}

/// `GET /v1/slots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slots {
    /// Every declared slot, in declaration order.
    pub slots: Vec<Slot>,
}

/// An answer about one slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotAnswer {
    /// The slot.
    pub slot: Slot,
}

/// How a hold asked for stays alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldMode {
    /// By the member's heartbeats.
    Session,
    /// By the heartbeats of the run the take starts.
    Run,
    /// Until a forge workflow run finishes.
    UntilRun,
}

/// `POST /v1/slots/{name}/take`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TakeSlotRequest {
    /// The caller's session.
    pub session_id: SessionId,
    /// Why.
    pub purpose: Purpose,
    /// The work item, by local id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// How the hold stays alive.
    pub mode: HoldMode,
    /// For `until_run`, the workflow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow: Option<WorkflowName>,
    /// For `until_run`, the commit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<CommitSha>,
}

/// What a take came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeOutcome {
    /// The caller holds the slot now.
    Taken,
    /// The caller held it already; its expiry moved.
    Renewed,
    /// The caller waits.
    Queued,
}

/// The answer of a take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Took {
    /// What it came to.
    pub outcome: TakeOutcome,
    /// The caller's place, from 1, when queued.
    pub position: Option<usize>,
    /// The run whose heartbeats keep the hold, for a run's take.
    pub run_id: Option<RunId>,
    /// The slot now.
    pub slot: Slot,
}

/// `POST /v1/slots/{name}/release`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReleaseSlotRequest {
    /// The run releasing it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    /// How the run's command ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<RunExit>,
}

/// The answer of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotReleased {
    /// The hold that ended.
    pub released: SlotHold,
    /// The slot now.
    pub slot: Slot,
}

/// `POST /v1/slots/{name}/takeover`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TakeOverSlotRequest {
    /// The caller's session.
    pub session_id: SessionId,
    /// Why the caller holds it now.
    pub purpose: Purpose,
    /// What the caller checked, as told to the previous holder.
    pub reason: String,
}

/// The answer of a takeover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotTakenOver {
    /// The previous hold.
    pub previous: SlotHold,
    /// The thread that told its holder.
    pub thread_id: ThreadId,
    /// The slot now.
    pub slot: Slot,
}

/// `POST /v1/admin/slots/{name}/force-release`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForceReleaseSlotRequest {
    /// The admin's session.
    pub session_id: SessionId,
    /// Why, as told to the holder.
    pub reason: String,
}

/// The answer of a forced release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotForced {
    /// The hold that ended.
    pub released: SlotHold,
    /// The member who held it.
    pub holder: Handle,
    /// The thread that told them; `None` when the admin held it.
    pub thread_id: Option<ThreadId>,
    /// The slot now.
    pub slot: Slot,
}

/// A slot on offer to the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOffer {
    /// The slot.
    pub slot: SlotName,
    /// When the offer lapses.
    #[serde(with = "time::serde::rfc3339")]
    pub until: OffsetDateTime,
}
