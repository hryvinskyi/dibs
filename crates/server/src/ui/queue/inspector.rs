//! The inspector of one ticket of the queue: where each area it will probably touch stands, beside
//! the ticket's own view; or, for an address of no ticket in the queue, what became of it.

use dibs_app::use_cases::{AreaHold, StatusView};
use dibs_domain::ids::ItemId;

use crate::ui::areas::held_by;
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

/// An area a ticket will probably touch, and every claim holding it.
#[derive(Debug, Clone)]
pub struct Standing {
    /// The area.
    pub area: AreaView,
    /// Each live claim holding it, with its own holder and ticket; empty while it is free.
    pub held: Vec<Holding>,
    /// Who holds it, in words: `alice for #1, bob for #2`; none while it is free.
    pub holders: Option<String>,
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
            .map(|area| Standing::of(area, &status.holds, links))
            .collect();
        Self { item, areas }
    }
}

impl Standing {
    /// Where `area` stands among `holds`: every hold of it, in the order given.
    fn of(area: &AreaView, holds: &[AreaHold], links: &LinkContext) -> Self {
        let held: Vec<Holding> = holds
            .iter()
            .filter(|hold| hold.area.to_string() == area.full)
            .map(|hold| Holding {
                holder: PersonView::of(&hold.holder),
                ticket: TicketView::of_item(hold.item, links),
            })
            .collect();
        let holders = held_by(held.iter().map(|holding| {
            (
                holding.holder.handle.as_str(),
                holding.ticket.title.as_str(),
            )
        }));
        Self {
            area: area.clone(),
            held,
            holders,
        }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dibs_domain::area::QualifiedArea;
    use dibs_domain::forge::ForgeRegistry;
    use dibs_domain::handle::Handle;
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::ids::MemberId;
    use dibs_domain::item::ExternalRef;
    use dibs_domain::ref_scheme::RefSchemeRegistry;
    use dibs_domain::settings::{ProjectSettings, SettingsDocument};

    use crate::ui::links::Place;

    use super::*;

    fn links() -> LinkContext {
        let document: SettingsDocument = serde_json::from_value(serde_json::json!({
            "repos": [{"id": "api", "remote": "https://github.com/acme/widgets",
                       "areas": [{"name": "billing", "paths": ["billing/**"]},
                                 {"name": "search", "paths": ["search/**"]}]}]
        }))
        .unwrap();
        LinkContext::new(
            ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap(),
            Arc::new(RefSchemeRegistry::standard()),
            Arc::new(ForgeRegistry::standard()),
            [1, 2, 3].map(|number| {
                (
                    ItemId(number),
                    Some(ExternalRef::parse(&format!("gh:acme/widgets#{number}")).unwrap()),
                    Place::Claims,
                )
            }),
        )
    }

    fn hold(area: &str, member: u64, holder: &str, item: u64) -> AreaHold {
        AreaHold {
            area: QualifiedArea::parse(area).unwrap(),
            member: MemberId(member),
            holder: Handle::parse(holder).unwrap(),
            item: ItemId(item),
        }
    }

    fn view(area: &str) -> AreaView {
        AreaView::of(&QualifiedArea::parse(area).unwrap())
    }

    #[test]
    fn an_area_held_by_two_claims_names_each_holder_with_its_own_ticket() {
        let holds = [
            hold("api:billing", 1, "alice", 1),
            hold("api:search", 2, "bob", 3),
            hold("api:billing", 1, "alice", 2),
        ];
        let standing = Standing::of(&view("api:billing"), &holds, &links());
        let named: Vec<(&str, &str)> = standing
            .held
            .iter()
            .map(|holding| {
                (
                    holding.holder.handle.as_str(),
                    holding.ticket.title.as_str(),
                )
            })
            .collect();
        assert_eq!(
            named,
            [
                ("alice", "gh:acme/widgets#1"),
                ("alice", "gh:acme/widgets#2")
            ]
        );
        assert_eq!(
            standing.holders.as_deref(),
            Some("alice for gh:acme/widgets#1, alice for gh:acme/widgets#2")
        );
    }

    #[test]
    fn an_area_no_claim_holds_is_free() {
        let holds = [hold("api:search", 2, "bob", 3)];
        let standing = Standing::of(&view("api:billing"), &holds, &links());
        assert!(standing.held.is_empty());
        assert_eq!(standing.holders, None);
    }
}
