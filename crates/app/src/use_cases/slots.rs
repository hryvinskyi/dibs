//! The use cases of slots: taking, renewing and waiting for one, releasing and leaving, an admin's
//! forced release, a run's heartbeat, taking over a stalled hold, the forge's report that a run
//! finished, reading where they stand, and the sweep. Each runs in one transaction and applies what
//! time changed before it decides.

mod end;
mod list;
mod run;
mod scene;
mod take;
mod view;

#[cfg(test)]
mod tests;

pub use end::{
    EndedHold, ForceReleaseSlot, ForceReleaseSlotInput, ForcedHold, LeaveSlotQueue, ReleaseSlot,
    ReleaseSlotInput, RunFinishedInput, SlotRunFinished,
};
pub use list::{Slots, SweepReport, TickSlots};
pub use run::{SlotRunBeat, TakeOverSlot, TakeOverSlotInput, TakenOverSlot};
pub use take::{HoldMode, TakeSlot, TakeSlotInput, TookKind, TookSlot};
pub use view::{NamedHold, NamedOffer, NamedWaiter, SlotLine};

pub(crate) use scene::Scene;
pub(crate) use view::scene_lines;
