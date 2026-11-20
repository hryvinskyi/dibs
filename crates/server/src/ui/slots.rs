//! A declared slot as the pages name it: its name, its group, who holds it and what its
//! declaration says about it.

use dibs_app::use_cases::SlotLine;

/// A declared slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRow {
    /// The slot's name.
    pub name: String,
    /// Its group, when it has one.
    pub group: Option<String>,
    /// The handle of its group's holder, while it is held.
    pub holder: Option<String>,
    /// Its note.
    pub note: Option<String>,
}

impl SlotRow {
    /// The slot of a line of the status.
    pub fn of(line: &SlotLine) -> Self {
        Self {
            name: line.spec.name.to_string(),
            group: line.spec.group.as_ref().map(ToString::to_string),
            holder: line.hold.as_ref().map(|named| named.holder.to_string()),
            note: line.spec.note.as_ref().map(ToString::to_string),
        }
    }
}
