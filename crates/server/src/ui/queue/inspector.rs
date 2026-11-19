//! The inspector of one ticket of the queue: where each area it will probably touch stands, beside
//! the ticket's own view; or, for an address of no ticket in the queue, what became of it.

use dibs_app::use_cases::StatusView;
use dibs_domain::ids::ItemId;

use crate::ui::links::{AreaView, LinkContext, PersonView, TicketView};
use crate::ui::queue::view::QueueItemView;

/// The inspector of a ticket of the queue.
#[derive(Debug, Clone)]
pub struct InspectorView {
    /// The ticket.
    pub item: QueueItemView,
    /// The areas it will probably touch, each with who holds it.
    pub areas: Vec<Standing>,
}

/// An area a ticket will probably touch, and who holds it.
#[derive(Debug, Clone)]
pub struct Standing {
    /// The area.
    pub area: AreaView,
    /// The live claim holding it; none while it is free.
    pub held: Option<Holding>,
}

/// A live claim holding an area.
#[derive(Debug, Clone)]
pub struct Holding {
    /// The holder.
    pub holder: PersonView,
    /// The ticket of the claim.
    pub ticket: TicketView,
}

/// The inspector of an address that names no ticket of the queue.
#[derive(Debug, Clone)]
pub struct GoneView {
    /// The address of the claim on the item, when one holds it.
    pub claim: Option<String>,
}

impl InspectorView {
    /// The inspector of the ticket `item`, with the holds the status shows.
    pub fn of(item: QueueItemView, status: &StatusView, links: &LinkContext) -> Self {
        let areas = item
            .areas
            .iter()
            .map(|area| Standing {
                held: status
                    .holds
                    .iter()
                    .find(|hold| hold.area.to_string() == area.full)
                    .map(|hold| Holding {
                        holder: PersonView::of(&hold.holder),
                        ticket: TicketView::of_item(hold.item, links),
                    }),
                area: area.clone(),
            })
            .collect();
        Self { item, areas }
    }
}

impl GoneView {
    /// What became of the item `id`: the address of its claim when a live or stale claim holds it.
    pub fn of(id: ItemId, status: &StatusView) -> Self {
        let held = status
            .live
            .iter()
            .chain(&status.stale)
            .any(|held| held.item.item.id == id);
        Self {
            claim: held.then(|| format!("/ui/claims/{id}")),
        }
    }
}
