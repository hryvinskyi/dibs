//! Slots as the board's strip shows them: each declared slot, who holds it and why, until when,
//! whether its run went silent, and who waits for it or is offered it.

use time::OffsetDateTime;

use dibs_app::use_cases::SlotLine;

use crate::ui::moments::{Moment, Span, moment, span};

/// Where a slot stands, as the strip marks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    /// Nobody holds it, waits for it, or is offered it.
    Free,
    /// Held, nobody waiting.
    Held,
    /// Held, with members waiting.
    Queued,
    /// Held by a run that stopped sending heartbeats.
    Stalled {
        /// How long since its last heartbeat.
        silent: Span,
    },
    /// Offered to the member first in the queue.
    Offered {
        /// The member.
        member: String,
        /// When the offer lapses.
        until: Moment,
    },
}

/// The hold a slot's card names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldText {
    /// The holder's handle.
    pub holder: String,
    /// Why they hold it.
    pub purpose: String,
    /// The slot held, when it is another slot of the group.
    pub through: Option<String>,
    /// How long until the hold ends.
    pub left: Span,
    /// When it ends.
    pub until: Moment,
    /// Its last sign of life.
    pub seen: Moment,
}

/// A slot's card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRow {
    /// The slot's name.
    pub name: String,
    /// Its group, when it has one.
    pub group: Option<String>,
    /// Where it stands.
    pub state: SlotState,
    /// Its group's hold.
    pub hold: Option<HoldText>,
    /// The handles of the members waiting, first in line first.
    pub waiting: Vec<String>,
    /// Its note.
    pub note: Option<String>,
}

impl SlotRow {
    /// The card of a slot's line at `now`.
    pub fn of(line: &SlotLine, now: OffsetDateTime) -> Self {
        let hold = line.hold.as_ref().map(|named| HoldText {
            holder: named.holder.to_string(),
            purpose: named.hold.purpose.to_string(),
            through: (named.hold.slot != line.spec.name).then(|| named.hold.slot.to_string()),
            left: span(named.hold.expires_at - now),
            until: moment(named.hold.expires_at, now),
            seen: moment(named.hold.last_seen, now),
        });
        let waiting: Vec<String> = line
            .queue
            .iter()
            .map(|place| place.member.to_string())
            .collect();
        let state = match (&line.hold, &line.offer) {
            (Some(named), _) if named.hold.is_stalled() => SlotState::Stalled {
                silent: span(now - named.hold.last_seen),
            },
            (Some(_), _) if !waiting.is_empty() => SlotState::Queued,
            (Some(_), _) => SlotState::Held,
            (None, Some(offer)) => SlotState::Offered {
                member: offer.member.to_string(),
                until: moment(offer.offer.until, now),
            },
            (None, None) => SlotState::Free,
        };
        Self {
            name: line.spec.name.to_string(),
            group: line.spec.group.as_ref().map(ToString::to_string),
            state,
            hold,
            waiting,
            note: line.spec.note.as_ref().map(ToString::to_string),
        }
    }
}
