//! Slots as the routes answer them: each declared slot with its settings, its group's hold, queue
//! and offer, every member by handle.

use serde::Serialize;
use time::OffsetDateTime;

use dibs_app::use_cases::{NamedHold, NamedOffer, NamedWaiter, PendingOffer, SlotLine};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::Note;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::session::HostName;
use dibs_domain::settings::format_duration;
use dibs_domain::slot::{Liveness, Purpose, RepoRef, SlotGroup, SlotName};

/// A hold.
#[derive(Debug, Serialize)]
pub struct HoldView {
    /// The slot held: this one, or another slot of its group.
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
    /// How it stays alive: `{"mode": "session"}`, `{"mode": "run", "run": …}` or
    /// `{"mode": "until_run", "workflow": …, "sha": …}`.
    pub liveness: Liveness,
    /// When it was taken.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub taken_at: OffsetDateTime,
    /// When it ends unless renewed; a stalled hold does not end then.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub expires_at: OffsetDateTime,
    /// The last sign of life of what keeps it.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub last_seen: OffsetDateTime,
    /// Whether its run stopped sending heartbeats.
    pub stalled: bool,
}

impl HoldView {
    /// The view of a hold.
    pub fn of(named: NamedHold) -> Self {
        let hold = named.hold;
        Self {
            stalled: hold.is_stalled(),
            slot: hold.slot,
            holder: named.holder,
            purpose: hold.purpose,
            item: hold.item,
            session_id: hold.session,
            host: hold.host,
            liveness: hold.liveness,
            taken_at: hold.taken_at,
            expires_at: hold.expires_at,
            last_seen: hold.last_seen,
        }
    }
}

/// A place in a queue.
#[derive(Debug, Serialize)]
pub struct WaiterView {
    /// The member waiting.
    pub member: Handle,
    /// The slot they asked for.
    pub slot: SlotName,
    /// Why they want it.
    pub purpose: Purpose,
    /// When they joined the queue.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub since: OffsetDateTime,
    /// Their last sign of life.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub last_seen: OffsetDateTime,
}

impl WaiterView {
    fn of(named: NamedWaiter) -> Self {
        Self {
            member: named.member,
            slot: named.waiter.slot,
            purpose: named.waiter.purpose,
            since: named.waiter.since,
            last_seen: named.waiter.last_seen,
        }
    }
}

/// An offer to the member first in the queue.
#[derive(Debug, Serialize)]
pub struct OfferView {
    /// The member.
    pub member: Handle,
    /// The slot they asked for.
    pub slot: SlotName,
    /// Why they want it.
    pub purpose: Purpose,
    /// When the offer lapses.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub until: OffsetDateTime,
}

impl OfferView {
    fn of(named: NamedOffer) -> Self {
        Self {
            member: named.member,
            slot: named.offer.slot,
            purpose: named.offer.purpose,
            until: named.offer.until,
        }
    }
}

/// A declared slot and where its group stands.
#[derive(Debug, Serialize)]
pub struct SlotView {
    /// Its name.
    pub name: SlotName,
    /// Its group, when it has one.
    pub group: Option<SlotGroup>,
    /// The longest one hold lasts (`30m`).
    pub max_hold: String,
    /// How long an offer stands.
    pub offer_for: String,
    /// How long a waiter keeps their place without a sign of life.
    pub max_wait: String,
    /// Whether its commands run only inside `dibs slot run`.
    pub run_only: bool,
    /// The refs nobody but the holder may push while it is held.
    pub freezes: Vec<RepoRef>,
    /// Its note.
    pub note: Option<Note>,
    /// The hold of its group.
    pub hold: Option<HoldView>,
    /// Its group's queue, first in line first.
    pub queue: Vec<WaiterView>,
    /// Its group's offer.
    pub offer: Option<OfferView>,
}

impl SlotView {
    /// The view of a slot's line.
    pub fn of(line: SlotLine) -> Self {
        let spec = line.spec;
        Self {
            name: spec.name,
            group: spec.group,
            max_hold: format_duration(spec.max_hold),
            offer_for: format_duration(spec.offer_for),
            max_wait: format_duration(spec.max_wait),
            run_only: spec.run_only,
            freezes: spec.freezes,
            note: spec.note,
            hold: line.hold.map(HoldView::of),
            queue: line.queue.into_iter().map(WaiterView::of).collect(),
            offer: line.offer.map(OfferView::of),
        }
    }
}

/// A slot on offer to the caller.
#[derive(Debug, Serialize)]
pub struct PendingOfferView {
    /// The slot.
    pub slot: SlotName,
    /// When the offer lapses.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub until: OffsetDateTime,
}

impl PendingOfferView {
    /// The view of a pending offer.
    pub fn of(offer: PendingOffer) -> Self {
        Self {
            slot: offer.slot,
            until: offer.until,
        }
    }
}
