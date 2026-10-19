//! Dependencies between work items: "this item is blocked by that one", and when an item is
//! blocked.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::ItemId;
use crate::item::ItemStatus;

/// Who recorded a dependency. An importer replaces only its own edges, so an edge a person added
/// survives every import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencySource {
    /// A person added it.
    Manual,
    /// An importer reported it.
    Imported,
}

/// An edge of a project's work graph: `item` is blocked by `blocked_by`. Both are items of one
/// project, and a pair has at most one edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dependency {
    /// The item that waits.
    pub item: ItemId,
    /// The item it waits for.
    pub blocked_by: ItemId,
    /// Who recorded it.
    pub source: DependencySource,
}

/// Why an edge may not be added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DependencyRefused {
    /// The item would wait for itself.
    #[error("an item cannot be blocked by itself")]
    SelfDependency,
    /// The item it would wait for already waits for it, directly or through other items.
    #[error(
        "the blocker already waits for this item, directly or through other items, so the two would wait for each other for ever"
    )]
    Cycle,
}

/// Decides whether `edge` may join `graph`, the project's edges of both sources: it may not make an
/// item wait for itself, and it may not close a cycle — that is, `edge.blocked_by` may not already
/// wait, through any chain of edges, for `edge.item`.
///
/// # Errors
/// [`DependencyRefused`] naming why not.
pub fn add_dependency(graph: &[Dependency], edge: &Dependency) -> Result<(), DependencyRefused> {
    if edge.item == edge.blocked_by {
        return Err(DependencyRefused::SelfDependency);
    }
    if waits_for(graph, edge.blocked_by, edge.item) {
        return Err(DependencyRefused::Cycle);
    }
    Ok(())
}

/// Whether `from` waits for `target` through a chain of one or more edges.
fn waits_for(graph: &[Dependency], from: ItemId, target: ItemId) -> bool {
    let mut seen = BTreeSet::from([from]);
    let mut stack = vec![from];
    while let Some(current) = stack.pop() {
        for edge in graph.iter().filter(|edge| edge.item == current) {
            if edge.blocked_by == target {
                return true;
            }
            if seen.insert(edge.blocked_by) {
                stack.push(edge.blocked_by);
            }
        }
    }
    false
}

/// Whether an item no longer blocks the items that wait for it: it is done, or its tracker closed
/// it.
pub fn stops_blocking(status: ItemStatus, closed: bool) -> bool {
    status == ItemStatus::Done || closed
}

/// The items `item` waits for that still block it, in id order. `state` gives an item's status and
/// whether its tracker closed it; an item it does not know blocks nothing. An item is blocked while
/// this list is not empty.
pub fn blockers<F>(item: ItemId, graph: &[Dependency], state: F) -> Vec<ItemId>
where
    F: Fn(ItemId) -> Option<(ItemStatus, bool)>,
{
    let waiting: BTreeSet<ItemId> = graph
        .iter()
        .filter(|edge| edge.item == item)
        .map(|edge| edge.blocked_by)
        .filter(|blocker| {
            state(*blocker).is_some_and(|(status, closed)| !stops_blocking(status, closed))
        })
        .collect();
    waiting.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn edge(item: u64, blocked_by: u64, source: DependencySource) -> Dependency {
        Dependency {
            item: ItemId(item),
            blocked_by: ItemId(blocked_by),
            source,
        }
    }

    fn manual(item: u64, blocked_by: u64) -> Dependency {
        edge(item, blocked_by, DependencySource::Manual)
    }

    fn imported(item: u64, blocked_by: u64) -> Dependency {
        edge(item, blocked_by, DependencySource::Imported)
    }

    #[test]
    fn an_item_cannot_wait_for_itself() {
        assert_eq!(
            add_dependency(&[], &manual(1, 1)),
            Err(DependencyRefused::SelfDependency)
        );
    }

    #[test]
    fn a_chain_refuses_the_edge_that_would_close_it() {
        let chain = [manual(1, 2), imported(2, 3), manual(3, 4)];
        assert_eq!(
            add_dependency(&chain, &manual(4, 1)),
            Err(DependencyRefused::Cycle)
        );
        assert_eq!(
            add_dependency(&chain, &imported(4, 2)),
            Err(DependencyRefused::Cycle)
        );
        assert_eq!(add_dependency(&chain, &manual(1, 4)), Ok(()));
        assert_eq!(add_dependency(&chain, &manual(5, 1)), Ok(()));
    }

    #[test]
    fn a_diamond_is_no_cycle_but_its_closing_edge_is() {
        let diamond = [manual(1, 2), manual(1, 3), imported(2, 4), imported(3, 4)];
        assert_eq!(add_dependency(&diamond, &manual(2, 3)), Ok(()));
        assert_eq!(add_dependency(&diamond, &manual(1, 4)), Ok(()));
        assert_eq!(
            add_dependency(&diamond, &manual(4, 1)),
            Err(DependencyRefused::Cycle)
        );
        assert_eq!(
            add_dependency(&diamond, &imported(4, 3)),
            Err(DependencyRefused::Cycle)
        );
    }

    #[test]
    fn a_three_cycle_is_refused_over_both_kinds_of_edge() {
        let two = [imported(1, 2), manual(2, 3)];
        assert_eq!(
            add_dependency(&two, &manual(3, 1)),
            Err(DependencyRefused::Cycle)
        );
        assert_eq!(
            add_dependency(&two, &imported(3, 1)),
            Err(DependencyRefused::Cycle)
        );
    }

    #[test]
    fn a_graph_with_loops_elsewhere_is_still_walked_to_the_end() {
        let graph = [manual(1, 2), manual(2, 1), manual(3, 4)];
        assert_eq!(add_dependency(&graph, &manual(4, 5)), Ok(()));
        assert_eq!(
            add_dependency(&graph, &manual(4, 3)),
            Err(DependencyRefused::Cycle)
        );
    }

    #[test]
    fn an_item_is_blocked_while_a_blocker_is_neither_done_nor_closed() {
        let graph = [
            manual(1, 2),
            imported(1, 3),
            manual(1, 4),
            manual(1, 5),
            manual(6, 2),
        ];
        let states: BTreeMap<ItemId, (ItemStatus, bool)> = BTreeMap::from([
            (ItemId(2), (ItemStatus::Open, false)),
            (ItemId(3), (ItemStatus::Done, false)),
            (ItemId(4), (ItemStatus::Claimed, true)),
            (ItemId(5), (ItemStatus::Dropped, false)),
        ]);
        let state = |id: ItemId| states.get(&id).copied();
        assert_eq!(blockers(ItemId(1), &graph, state), [ItemId(2), ItemId(5)]);
        assert_eq!(blockers(ItemId(2), &graph, state), []);
        assert_eq!(blockers(ItemId(9), &graph, state), []);
        assert!(stops_blocking(ItemStatus::Done, false));
        assert!(stops_blocking(ItemStatus::Open, true));
        assert!(!stops_blocking(ItemStatus::Claimed, false));
    }
}
