//! What an edit check asks the store to record: areas added to a claim and paths shared under one.

use crate::area::QualifiedArea;
use crate::ids::ItemId;
use crate::paths::RepoPath;

/// A path a claim recorded as edited under a `warn-shared` rule. Unique per claim and path; removed
/// with its claim.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SharedPath {
    /// The item of the claim.
    pub claim: ItemId,
    /// The path.
    pub path: RepoPath,
}

/// An area to add to a claim, because the claim's member edited inside it and nobody held it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AddArea {
    /// The item of the claim.
    pub claim: ItemId,
    /// The area.
    pub area: QualifiedArea,
}

/// The reservations an edit check produced. Empty when everything it saw was already recorded, so
/// a repeated check writes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReservationPlan {
    /// Areas to add to claims, each once.
    pub add_areas: Vec<AddArea>,
    /// Paths to record as shared, each once.
    pub share_paths: Vec<SharedPath>,
}

impl ReservationPlan {
    /// Whether the plan records nothing.
    pub fn is_empty(&self) -> bool {
        self.add_areas.is_empty() && self.share_paths.is_empty()
    }

    pub(crate) fn add_area(&mut self, entry: AddArea) {
        if !self.add_areas.contains(&entry) {
            self.add_areas.push(entry);
        }
    }

    pub(crate) fn share_path(&mut self, entry: SharedPath) {
        if !self.share_paths.contains(&entry) {
            self.share_paths.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::area;

    #[test]
    fn a_plan_holds_each_entry_once() {
        let mut plan = ReservationPlan::default();
        assert!(plan.is_empty());
        let add = AddArea {
            claim: ItemId(1),
            area: area("api:billing"),
        };
        plan.add_area(add.clone());
        plan.add_area(add);
        let share = SharedPath {
            claim: ItemId(1),
            path: RepoPath::parse("api:composer.lock").unwrap(),
        };
        plan.share_path(share.clone());
        plan.share_path(share);
        assert_eq!((plan.add_areas.len(), plan.share_paths.len()), (1, 1));
        assert!(!plan.is_empty());
    }
}
