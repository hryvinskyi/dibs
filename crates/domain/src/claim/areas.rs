//! The areas a claim holds, each with how the claim came to hold it.

use std::collections::BTreeSet;
use std::collections::btree_set;
use std::ops::Deref;

use thiserror::Error;

use crate::area::QualifiedArea;

/// How a claim came to hold one of its areas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AreaOrigin {
    /// The claim named it: when it was taken or renewed, or when the area was handed over to it.
    Declared,
    /// An edit fell to the claim only as its member's target claim, because none of their claims
    /// held the area, and the area was added to it. Such an area holds like a declared one, but an
    /// edit in it does not tell what work the edit is for.
    Gained,
}

/// The areas a claim holds. Read it as the set of those areas; it also knows which of them the
/// claim only gained, which is always a subset of them. An area named again is declared, whatever
/// it was before.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimAreas {
    held: BTreeSet<QualifiedArea>,
    gained: BTreeSet<QualifiedArea>,
}

/// A claim's areas with a gained area it does not hold.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("the gained area '{area}' is not one of the claim's areas")]
pub struct GainedNotHeld {
    /// The area.
    pub area: QualifiedArea,
}

impl ClaimAreas {
    /// The areas, with those of `gained` gained and the others declared.
    ///
    /// # Errors
    /// [`GainedNotHeld`] naming the first area of `gained` that `held` lacks.
    pub fn new(
        held: BTreeSet<QualifiedArea>,
        gained: BTreeSet<QualifiedArea>,
    ) -> Result<Self, GainedNotHeld> {
        if let Some(area) = gained.difference(&held).next() {
            return Err(GainedNotHeld { area: area.clone() });
        }
        Ok(Self { held, gained })
    }

    /// Declares the area: the claim holds it, as declared even when it had only gained it.
    pub fn declare(&mut self, area: QualifiedArea) {
        self.gained.remove(&area);
        self.held.insert(area);
    }

    /// Adds the area as gained. An area the claim already holds keeps its origin, so a declared
    /// area stays declared.
    pub fn gain(&mut self, area: QualifiedArea) {
        if !self.held.contains(&area) {
            self.gained.insert(area.clone());
            self.held.insert(area);
        }
    }

    /// Gives the area up, whatever its origin; whether the claim held it.
    pub fn remove(&mut self, area: &QualifiedArea) -> bool {
        self.gained.remove(area);
        self.held.remove(area)
    }

    /// Keeps only the areas `keep` accepts, each with its origin.
    pub fn retain(&mut self, mut keep: impl FnMut(&QualifiedArea) -> bool) {
        self.held.retain(|area| keep(area));
        let held = &self.held;
        self.gained.retain(|area| held.contains(area));
    }

    /// How the claim came to hold the area; `None` when it does not hold it.
    pub fn origin(&self, area: &QualifiedArea) -> Option<AreaOrigin> {
        if self.gained.contains(area) {
            Some(AreaOrigin::Gained)
        } else if self.held.contains(area) {
            Some(AreaOrigin::Declared)
        } else {
            None
        }
    }

    /// Whether the claim holds the area because it named it.
    pub fn declares(&self, area: &QualifiedArea) -> bool {
        self.origin(area) == Some(AreaOrigin::Declared)
    }

    /// The areas the claim only gained, in order.
    pub fn gained(&self) -> &BTreeSet<QualifiedArea> {
        &self.gained
    }
}

impl Deref for ClaimAreas {
    type Target = BTreeSet<QualifiedArea>;

    fn deref(&self) -> &Self::Target {
        &self.held
    }
}

impl From<BTreeSet<QualifiedArea>> for ClaimAreas {
    /// Every area declared.
    fn from(held: BTreeSet<QualifiedArea>) -> Self {
        Self {
            held,
            gained: BTreeSet::new(),
        }
    }
}

impl FromIterator<QualifiedArea> for ClaimAreas {
    /// Every area declared.
    fn from_iter<I: IntoIterator<Item = QualifiedArea>>(areas: I) -> Self {
        Self::from(areas.into_iter().collect::<BTreeSet<_>>())
    }
}

impl<'a> IntoIterator for &'a ClaimAreas {
    type Item = &'a QualifiedArea;
    type IntoIter = btree_set::Iter<'a, QualifiedArea>;

    fn into_iter(self) -> Self::IntoIter {
        self.held.iter()
    }
}

impl PartialEq<BTreeSet<QualifiedArea>> for ClaimAreas {
    /// Whether the claim holds exactly these areas, whatever their origins.
    fn eq(&self, other: &BTreeSet<QualifiedArea>) -> bool {
        self.held == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{area, areas};

    #[test]
    fn a_gained_area_is_held_and_known_as_gained() {
        let mut held = ClaimAreas::from(areas(&["api:billing"]));
        held.gain(area("api:search"));
        assert_eq!(held, areas(&["api:billing", "api:search"]));
        assert_eq!(held.origin(&area("api:search")), Some(AreaOrigin::Gained));
        assert_eq!(
            held.origin(&area("api:billing")),
            Some(AreaOrigin::Declared)
        );
        assert_eq!(held.origin(&area("web:checkout")), None);
        assert!(held.contains(&area("api:search")));
        assert!(!held.declares(&area("api:search")));
    }

    #[test]
    fn gaining_a_declared_area_keeps_it_declared() {
        let mut held = ClaimAreas::from(areas(&["api:billing"]));
        held.gain(area("api:billing"));
        assert!(held.declares(&area("api:billing")));
        assert!(held.gained().is_empty());
    }

    #[test]
    fn declaring_a_gained_area_makes_it_declared() {
        let mut held = ClaimAreas::default();
        held.gain(area("api:search"));
        held.declare(area("api:search"));
        assert!(held.declares(&area("api:search")));
        assert!(held.gained().is_empty());
    }

    #[test]
    fn giving_an_area_up_forgets_its_origin() {
        let mut held = ClaimAreas::default();
        held.gain(area("api:search"));
        held.declare(area("api:billing"));
        assert!(held.remove(&area("api:search")));
        assert!(!held.remove(&area("api:search")));
        held.retain(|kept| *kept != area("api:billing"));
        assert!(held.is_empty());
        assert!(held.gained().is_empty());
    }

    #[test]
    fn gained_areas_must_be_held() {
        assert_eq!(
            ClaimAreas::new(areas(&["api:billing"]), areas(&["api:search"])),
            Err(GainedNotHeld {
                area: area("api:search")
            })
        );
        let read = ClaimAreas::new(
            areas(&["api:billing", "api:search"]),
            areas(&["api:search"]),
        )
        .unwrap();
        assert_eq!(read.gained(), &areas(&["api:search"]));
        assert!(read.declares(&area("api:billing")));
    }
}
