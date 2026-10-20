//! The slots as an answer shows them: each declared slot with its group's hold, queue and offer,
//! every member named by handle.

use dibs_domain::handle::Handle;
use dibs_domain::ids::MemberId;
use dibs_domain::slot::{
    SlotBoard, SlotCatalog, SlotHold, SlotName, SlotOffer, SlotSpec, SlotWaiter,
};

use crate::error::AppError;
use crate::named::Names;
use crate::use_cases::slots::scene::Scene;

/// A hold with its holder's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedHold {
    /// The hold.
    pub hold: SlotHold,
    /// The handle of `hold.member`.
    pub holder: Handle,
}

/// A place in a queue with its member's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedWaiter {
    /// The place.
    pub waiter: SlotWaiter,
    /// The handle of `waiter.member`.
    pub member: Handle,
}

/// An offer with its member's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedOffer {
    /// The offer.
    pub offer: SlotOffer,
    /// The handle of `offer.member`.
    pub member: Handle,
}

/// One declared slot and where its group stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotLine {
    /// The slot as declared.
    pub spec: SlotSpec,
    /// The hold of its group: of this slot, or of another slot of the group (`hold.slot` says).
    pub hold: Option<NamedHold>,
    /// Its group's queue, first in line first.
    pub queue: Vec<NamedWaiter>,
    /// Its group's offer.
    pub offer: Option<NamedOffer>,
}

impl SlotLine {
    /// The member's place in the queue, from 1.
    pub fn position_of(&self, member: MemberId) -> Option<usize> {
        self.queue
            .iter()
            .position(|place| place.waiter.member == member)
            .map(|index| index + 1)
    }
}

/// Builds the lines of a board, each member named.
pub(crate) struct Lines<'a> {
    pub(crate) catalog: &'a SlotCatalog,
    pub(crate) board: &'a SlotBoard,
    pub(crate) names: &'a Names,
}

impl Lines<'_> {
    /// The line of every declared slot, in declaration order.
    pub(crate) fn all(&self) -> Result<Vec<SlotLine>, AppError> {
        self.catalog
            .slots()
            .iter()
            .map(|spec| self.line(spec))
            .collect()
    }

    /// The line of one slot, when it is declared.
    pub(crate) fn of(&self, slot: &SlotName) -> Result<Option<SlotLine>, AppError> {
        self.catalog
            .get(slot)
            .map(|spec| self.line(spec))
            .transpose()
    }

    fn line(&self, spec: &SlotSpec) -> Result<SlotLine, AppError> {
        let key = spec.group_key();
        let hold = self
            .board
            .group_holds(self.catalog, &key)
            .next()
            .map(|hold| {
                Ok::<_, AppError>(NamedHold {
                    holder: self.names.handle(hold.member)?,
                    hold: hold.clone(),
                })
            })
            .transpose()?;
        let queue = self
            .board
            .group_queue(self.catalog, &key)
            .into_iter()
            .map(|waiter| {
                Ok(NamedWaiter {
                    member: self.names.handle(waiter.member)?,
                    waiter: waiter.clone(),
                })
            })
            .collect::<Result<_, AppError>>()?;
        let offer = self
            .board
            .group_offer(self.catalog, &key)
            .map(|offer| {
                Ok::<_, AppError>(NamedOffer {
                    member: self.names.handle(offer.member)?,
                    offer: offer.clone(),
                })
            })
            .transpose()?;
        Ok(SlotLine {
            spec: spec.clone(),
            hold,
            queue,
            offer,
        })
    }
}

/// The line of every declared slot of the scene, named.
pub(crate) async fn scene_lines(
    work: &mut crate::work::Work,
    scene: &Scene,
) -> Result<Vec<SlotLine>, AppError> {
    let names = Names::load(work).await?;
    Lines {
        catalog: scene.catalog(),
        board: &scene.board,
        names: &names,
    }
    .all()
}
