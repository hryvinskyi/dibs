//! What the queue lists: the ready items, then the open items left out of the ready list for
//! another reason than a claim, each group in the status's order. An item a live claim holds is a
//! claim, not a ticket that waits, so the queue never lists it.

use dibs_app::records::ItemRecord;
use dibs_app::use_cases::{ExcludedEntry, NamedNotReady, StatusView};

/// An item the queue lists, and why it is not ready, when it is not.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Listed<'a> {
    /// The item.
    pub(crate) item: &'a ItemRecord,
    /// Why it is not ready; empty for a ready one.
    pub(crate) reasons: &'a [NamedNotReady],
}

/// The items the queue lists, the ready ones first.
pub(crate) fn listed(status: &StatusView) -> impl Iterator<Item = Listed<'_>> {
    let ready = status.ready.ready.iter().map(|entry| Listed {
        item: &entry.item.item,
        reasons: &[],
    });
    let waiting = status
        .ready
        .excluded
        .iter()
        .filter(|entry| waits(entry))
        .map(|entry| Listed {
            item: &entry.item.item,
            reasons: &entry.reasons,
        });
    ready.chain(waiting)
}

/// Whether an item left out of the ready list waits in the queue: no claim holds it.
pub(crate) fn waits(entry: &ExcludedEntry) -> bool {
    !entry
        .reasons
        .iter()
        .any(|reason| matches!(reason, NamedNotReady::Claimed { .. }))
}
