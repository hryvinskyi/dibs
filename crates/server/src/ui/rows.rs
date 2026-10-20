//! Claims, holds and items as the pages list them: each value written as the text a page shows,
//! each time as a [`Moment`].

use time::OffsetDateTime;

use dibs_app::named::ItemName;
use dibs_app::use_cases::{
    AreaHold, ExcludedEntry, HeldItem, NamedNotReady, ReadyEntry, WideClaimStatus,
};
use dibs_domain::ids::{ItemId, MemberId};
use dibs_domain::item::ExternalRef;

use crate::ui::moments::{Moment, moment};

/// How an item is named on a page: its reference in a tracker, or its local id.
pub fn reference(id: ItemId, external: Option<&ExternalRef>) -> String {
    external.map_or_else(|| format!("#{id}"), ToString::to_string)
}

/// A claim on an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRow {
    /// The item's local id, for the forms that act on it.
    pub item_id: u64,
    /// How the item is named.
    pub reference: String,
    /// The item's title.
    pub title: String,
    /// The holder's handle.
    pub holder: String,
    /// The machine of the session that took or last renewed the claim.
    pub host: String,
    /// The areas it declares, as `repo:area`.
    pub areas: Vec<String>,
    /// When it was claimed.
    pub since: Moment,
    /// Its last sign of life.
    pub seen: Moment,
    /// Whether the member viewing the page holds it.
    pub mine: bool,
    /// Whether the outside tracker closed the item while the claim lasts.
    pub closed_upstream: bool,
}

impl ClaimRow {
    /// The row of a held item, seen by `viewer` at `now`.
    pub fn of(held: &HeldItem, viewer: MemberId, now: OffsetDateTime) -> Self {
        let claim = &held.claim.claim;
        let item = &held.item.item;
        Self {
            item_id: item.id.0,
            reference: reference(item.id, item.external_ref.as_ref()),
            title: item.title.to_string(),
            holder: held.claim.holder.to_string(),
            host: claim.host.to_string(),
            areas: claim.areas.iter().map(ToString::to_string).collect(),
            since: moment(claim.claimed_at, now),
            seen: moment(claim.last_seen, now),
            mine: claim.member == viewer,
            closed_upstream: item.closed,
        }
    }
}

/// An area and who holds it for which item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldRow {
    /// The area, as `repo:area`.
    pub area: String,
    /// The holder's handle.
    pub holder: String,
    /// How the item is named.
    pub reference: String,
}

impl HoldRow {
    /// The row of a hold; the item is named from the live claims it belongs to.
    pub fn of(hold: &AreaHold, live: &[HeldItem]) -> Self {
        let external = live
            .iter()
            .find(|held| held.item.item.id == hold.item)
            .and_then(|held| held.item.item.external_ref.as_ref());
        Self {
            area: hold.area.to_string(),
            holder: hold.holder.to_string(),
            reference: reference(hold.item, external),
        }
    }
}

/// A live wide claim and where each of its areas stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideRow {
    /// How the item is named.
    pub reference: String,
    /// The item's title.
    pub title: String,
    /// The wide claim's member.
    pub holder: String,
    /// Why the change is made.
    pub reason: String,
    /// Its areas, in area order.
    pub areas: Vec<WideAreaRow>,
}

/// One area of a wide claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideAreaRow {
    /// The area, as `repo:area`.
    pub area: String,
    /// The handle of the other member holding it, when one does.
    pub holder: Option<String>,
    /// Whether that holder granted the edits there.
    pub granted: bool,
}

impl WideRow {
    /// The row of a live wide claim.
    pub fn of(wide: &WideClaimStatus) -> Self {
        let claim = &wide.held.claim.claim;
        let item = &wide.held.item.item;
        Self {
            reference: reference(item.id, item.external_ref.as_ref()),
            title: item.title.to_string(),
            holder: wide.held.claim.holder.to_string(),
            reason: claim
                .wide
                .as_ref()
                .map(|scope| scope.reason().to_string())
                .unwrap_or_default(),
            areas: wide
                .areas
                .iter()
                .map(|row| WideAreaRow {
                    area: row.area.to_string(),
                    holder: row.holder.as_ref().map(|(handle, _)| handle.to_string()),
                    granted: row.granted,
                })
                .collect(),
        }
    }
}

/// An item another card mentions, named and linked to its tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemLink {
    /// How the item is named.
    pub reference: String,
    /// Its title.
    pub title: String,
    /// The tracker's page for it, when its reference has one.
    pub link: Option<String>,
}

impl ItemLink {
    /// The link of a mentioned item.
    pub fn of(name: &ItemName) -> Self {
        Self {
            reference: reference(name.id, name.external_ref.as_ref()),
            title: name.title.to_string(),
            link: name.link.clone(),
        }
    }
}

/// Why an item on the board is not ready, as the card says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotReadyRow {
    /// A member's live claim holds it.
    Claimed {
        /// The holder's handle.
        holder: String,
    },
    /// Items it waits for still block it.
    BlockedBy {
        /// Those items.
        items: Vec<ItemLink>,
    },
    /// Another member holds an area of its hint.
    AreaHeld {
        /// The area, as `repo:area`.
        area: String,
        /// The holder's handle.
        holder: String,
        /// The holder's item.
        item: ItemLink,
    },
}

impl NotReadyRow {
    fn of(reason: &NamedNotReady) -> Self {
        match reason {
            NamedNotReady::Claimed { holder } => Self::Claimed {
                holder: holder.to_string(),
            },
            NamedNotReady::BlockedBy { items } => Self::BlockedBy {
                items: items.iter().map(ItemLink::of).collect(),
            },
            NamedNotReady::AreaHeld { area, holder, item } => Self::AreaHeld {
                area: area.to_string(),
                holder: holder.to_string(),
                item: ItemLink::of(item),
            },
        }
    }

    /// Whether the reason is a live claim, which puts the item in progress rather than out of
    /// the ready list.
    pub fn is_claim(&self) -> bool {
        matches!(self, Self::Claimed { .. })
    }
}

/// An item of the ready list, or one left out of it, on a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyCard {
    /// How the item is named.
    pub reference: String,
    /// Its title.
    pub title: String,
    /// The tracker's page for it, when its reference has one.
    pub link: Option<String>,
    /// How soon: 0 to 4, lower is sooner.
    pub priority: u8,
    /// The areas the work will probably touch, as `repo:area`.
    pub hints: Vec<String>,
    /// The member whose claim on it went stale, when one did.
    pub stale_holder: Option<String>,
    /// Why it is not ready; empty for an item of the ready list.
    pub reasons: Vec<NotReadyRow>,
}

impl ReadyCard {
    /// The card of an item of the ready list.
    pub fn ready(entry: &ReadyEntry) -> Self {
        let item = &entry.item.item;
        Self {
            reference: reference(item.id, item.external_ref.as_ref()),
            title: item.title.to_string(),
            link: entry.link.clone(),
            priority: item.priority.get(),
            hints: item.areas_hint.iter().map(ToString::to_string).collect(),
            stale_holder: entry.stale_holder.as_ref().map(ToString::to_string),
            reasons: Vec::new(),
        }
    }

    /// The card of an open item left out of the ready list.
    pub fn excluded(entry: &ExcludedEntry) -> Self {
        let item = &entry.item.item;
        Self {
            reference: reference(item.id, item.external_ref.as_ref()),
            title: item.title.to_string(),
            link: entry.link.clone(),
            priority: item.priority.get(),
            hints: item.areas_hint.iter().map(ToString::to_string).collect(),
            stale_holder: None,
            reasons: entry.reasons.iter().map(NotReadyRow::of).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_is_named_by_its_reference_or_else_its_id() {
        let external = ExternalRef::parse("gh:acme/widgets#42").unwrap();
        assert_eq!(reference(ItemId(7), Some(&external)), "gh:acme/widgets#42");
        assert_eq!(reference(ItemId(7), None), "#7");
    }
}
