//! Claims, holds and items as the pages list them: each value written as the text a page shows,
//! each time as a [`Moment`].

use time::OffsetDateTime;

use dibs_app::named::NamedItem;
use dibs_app::use_cases::{AreaHold, HeldItem};
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

/// An item on a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemCard {
    /// How the item is named.
    pub reference: String,
    /// Its title.
    pub title: String,
}

impl ItemCard {
    /// The card of an item.
    pub fn of(named: &NamedItem) -> Self {
        Self {
            reference: reference(named.item.id, named.item.external_ref.as_ref()),
            title: named.item.title.to_string(),
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
