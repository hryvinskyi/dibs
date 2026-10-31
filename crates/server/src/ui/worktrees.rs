//! Worktree holds as the board shows them: one card per member and host, each hold with its
//! worktree, whether the member's session or one of its agents holds it, why, for which item and
//! since when. The server knows a pool only by its holds, so a free worktree is never shown, and a
//! card never names the machine, the pool, the session or the agent.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use dibs_app::use_cases::BoardHold;
use dibs_domain::ids::ItemId;
use dibs_domain::item::ExternalRef;
use dibs_domain::worktree::HolderKind;

use crate::ui::moments::{Moment, moment};
use crate::ui::rows::reference;

/// How an item a hold is for is named, and its tracker's page when the board knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemMark {
    /// How the item is named.
    pub reference: String,
    /// The tracker's page for it.
    pub link: Option<String>,
}

/// The names and tracker pages of the items the board already shows, by id.
#[derive(Debug, Clone, Default)]
pub struct ItemMarks(BTreeMap<ItemId, ItemMark>);

impl ItemMarks {
    /// Records how an item is named and linked; the first record of an item stands.
    pub fn insert(&mut self, id: ItemId, external: Option<&ExternalRef>, link: Option<&String>) {
        self.0.entry(id).or_insert_with(|| ItemMark {
            reference: reference(id, external),
            link: link.cloned(),
        });
    }

    /// How the item is named: as recorded, or by its id with no link.
    fn mark(&self, id: ItemId) -> ItemMark {
        self.0.get(&id).cloned().unwrap_or_else(|| ItemMark {
            reference: reference(id, None),
            link: None,
        })
    }
}

/// One hold on a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRow {
    /// The worktree.
    pub worktree: String,
    /// The handle of the member whose pool it is.
    pub holder: String,
    /// Whether one of the member's agents holds it rather than a session of theirs.
    pub by_agent: bool,
    /// Why.
    pub purpose: String,
    /// The item it is held for, when one was named.
    pub item: Option<ItemMark>,
    /// When it was taken.
    pub since: Moment,
}

/// The holds of one member's pools on one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCard {
    /// The member's handle.
    pub member: String,
    /// The host.
    pub host: String,
    /// The holds, in the order given.
    pub holds: Vec<WorktreeRow>,
}

impl WorktreeCard {
    /// The cards of these holds at `now`: one per run of holds of one member on one host, in the
    /// order given, so holds ordered by member and host make one card each.
    pub fn of_all(holds: &[BoardHold], items: &ItemMarks, now: OffsetDateTime) -> Vec<Self> {
        let mut cards: Vec<Self> = Vec::new();
        for hold in holds {
            let member = hold.member.to_string();
            let host = hold.host.to_string();
            let row = WorktreeRow {
                worktree: hold.worktree.to_string(),
                holder: member.clone(),
                by_agent: hold.holder == HolderKind::Agent,
                purpose: hold.purpose.to_string(),
                item: hold.item.map(|item| items.mark(item)),
                since: moment(hold.since, now),
            };
            match cards.last_mut() {
                Some(card) if card.member == member && card.host == host => card.holds.push(row),
                _ => cards.push(Self {
                    member,
                    host,
                    holds: vec![row],
                }),
            }
        }
        cards
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use dibs_domain::handle::Handle;
    use dibs_domain::session::HostName;
    use dibs_domain::slot::Purpose;
    use dibs_domain::worktree::WorktreeName;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-03-02 10:00 UTC);

    fn hold(member: &str, host: &str, worktree: &str, holder: HolderKind) -> BoardHold {
        BoardHold {
            member: Handle::parse(member).unwrap(),
            host: HostName::parse(host).unwrap(),
            worktree: WorktreeName::parse(worktree).unwrap(),
            holder,
            purpose: Purpose::parse("fix the invoice totals").unwrap(),
            item: None,
            since: datetime!(2026-03-02 09:30 UTC),
        }
    }

    #[test]
    fn holds_of_one_member_on_one_host_share_a_card() {
        let holds = [
            hold("alice", "laptop", "worktree1", HolderKind::Agent),
            hold("alice", "laptop", "worktree2", HolderKind::Session),
            hold("alice", "tower", "worktree1", HolderKind::Agent),
            hold("bob", "laptop", "worktree1", HolderKind::Session),
        ];
        let cards = WorktreeCard::of_all(&holds, &ItemMarks::default(), NOW);
        let shape: Vec<String> = cards
            .iter()
            .map(|card| {
                let holds: Vec<String> = card
                    .holds
                    .iter()
                    .map(|row| {
                        let by = if row.by_agent { "agent" } else { "session" };
                        format!("{} by {by}", row.worktree)
                    })
                    .collect();
                format!("{} on {}: {}", card.member, card.host, holds.join(", "))
            })
            .collect();
        assert_eq!(
            shape,
            [
                "alice on laptop: worktree1 by agent, worktree2 by session",
                "alice on tower: worktree1 by agent",
                "bob on laptop: worktree1 by session",
            ]
        );
        assert_eq!(cards[0].holds[0].since.clock, "09:30");
        assert_eq!(cards[0].holds[0].purpose, "fix the invoice totals");
    }

    #[test]
    fn an_item_is_named_as_the_board_knows_it_or_by_its_id() {
        let mut items = ItemMarks::default();
        let external = ExternalRef::parse("gh:acme/widgets#7").unwrap();
        let link = "https://github.com/acme/widgets/issues/7".to_owned();
        items.insert(ItemId(4), Some(&external), Some(&link));
        items.insert(ItemId(4), None, None);
        let mut known = hold("alice", "laptop", "worktree1", HolderKind::Agent);
        known.item = Some(ItemId(4));
        let mut unknown = hold("alice", "laptop", "worktree2", HolderKind::Agent);
        unknown.item = Some(ItemId(9));
        let cards = WorktreeCard::of_all(&[known, unknown], &items, NOW);
        assert_eq!(
            cards[0].holds[0].item,
            Some(ItemMark {
                reference: "gh:acme/widgets#7".to_owned(),
                link: Some(link),
            })
        );
        assert_eq!(
            cards[0].holds[1].item,
            Some(ItemMark {
                reference: "#9".to_owned(),
                link: None,
            })
        );
    }
}
