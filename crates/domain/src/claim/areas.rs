//! The areas a claim holds, each with how the claim came to hold it and when its work last showed
//! there.

use std::collections::btree_set;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

use thiserror::Error;
use time::OffsetDateTime;

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
/// claim only gained, which is always a subset of them, and keeps a clock for each: when the claim
/// declared or gained the area, moved by every later declaration of it and by every edit of the
/// claim's work in it. An area named again is declared, whatever it was before. No area is held
/// without a clock, and giving an area up forgets its clock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimAreas {
    held: BTreeSet<QualifiedArea>,
    gained: BTreeSet<QualifiedArea>,
    clocks: BTreeMap<QualifiedArea, OffsetDateTime>,
}

/// Why stored parts do not make a claim's areas.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClaimAreasError {
    /// A gained area the claim does not hold.
    #[error("the gained area '{area}' is not one of the claim's areas")]
    GainedNotHeld {
        /// The area.
        area: QualifiedArea,
    },
    /// An area the claim holds has no clock.
    #[error("the area '{area}' has no clock")]
    Unclocked {
        /// The area.
        area: QualifiedArea,
    },
    /// A clock for an area the claim does not hold.
    #[error("the clocked area '{area}' is not one of the claim's areas")]
    ClockNotHeld {
        /// The area.
        area: QualifiedArea,
    },
}

impl ClaimAreas {
    /// Every one of `areas` declared at `at`.
    pub fn declared(areas: impl IntoIterator<Item = QualifiedArea>, at: OffsetDateTime) -> Self {
        let mut declared = Self::default();
        for area in areas {
            declared.declare(area, at);
        }
        declared
    }

    /// The areas, with those of `gained` gained and the others declared, each with its clock in
    /// `clocks`.
    ///
    /// # Errors
    /// [`ClaimAreasError`] naming the first area of `gained` that `held` lacks, else the first area
    /// of `held` without a clock, else the first clock of an area `held` lacks.
    pub fn new(
        held: BTreeSet<QualifiedArea>,
        gained: BTreeSet<QualifiedArea>,
        clocks: BTreeMap<QualifiedArea, OffsetDateTime>,
    ) -> Result<Self, ClaimAreasError> {
        if let Some(area) = gained.difference(&held).next() {
            return Err(ClaimAreasError::GainedNotHeld { area: area.clone() });
        }
        if let Some(area) = held.iter().find(|area| !clocks.contains_key(*area)) {
            return Err(ClaimAreasError::Unclocked { area: area.clone() });
        }
        if let Some(area) = clocks.keys().find(|area| !held.contains(*area)) {
            return Err(ClaimAreasError::ClockNotHeld { area: area.clone() });
        }
        Ok(Self {
            held,
            gained,
            clocks,
        })
    }

    /// Declares the area at `at`: the claim holds it, as declared even when it had only gained it,
    /// and its clock starts again at `at`.
    pub fn declare(&mut self, area: QualifiedArea, at: OffsetDateTime) {
        self.gained.remove(&area);
        self.clocks.insert(area.clone(), at);
        self.held.insert(area);
    }

    /// Adds the area as gained at `at`. An area the claim already holds keeps its origin and its
    /// clock, so a declared area stays declared.
    pub fn gain(&mut self, area: QualifiedArea, at: OffsetDateTime) {
        if !self.held.contains(&area) {
            self.gained.insert(area.clone());
            self.clocks.insert(area.clone(), at);
            self.held.insert(area);
        }
    }

    /// Notes an edit of the claim's work in the area at `at`: a held area's clock moves to `at`,
    /// whatever its origin; whether the claim holds it. An area the claim does not hold is left
    /// alone, since the claim has no clock for it.
    pub fn note_edit(&mut self, area: &QualifiedArea, at: OffsetDateTime) -> bool {
        match self.clocks.get_mut(area) {
            Some(clock) => {
                *clock = at;
                true
            }
            None => false,
        }
    }

    /// Starts every area's clock again at `at`, each keeping its origin.
    pub fn restart_clocks(&mut self, at: OffsetDateTime) {
        for clock in self.clocks.values_mut() {
            *clock = at;
        }
    }

    /// Gives the area up, whatever its origin, with its clock; whether the claim held it.
    pub fn remove(&mut self, area: &QualifiedArea) -> bool {
        self.gained.remove(area);
        self.clocks.remove(area);
        self.held.remove(area)
    }

    /// Keeps only the areas `keep` accepts, each with its origin and its clock.
    pub fn retain(&mut self, mut keep: impl FnMut(&QualifiedArea) -> bool) {
        self.held.retain(|area| keep(area));
        let held = &self.held;
        self.gained.retain(|area| held.contains(area));
        self.clocks.retain(|area, _| held.contains(area));
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

    /// The area's clock: when the claim last declared or gained it, or an edit of its work was
    /// made in it; `None` when the claim does not hold it.
    pub fn clock(&self, area: &QualifiedArea) -> Option<OffsetDateTime> {
        self.clocks.get(area).copied()
    }

    /// Every held area with its clock, in area order.
    pub fn clocks(&self) -> &BTreeMap<QualifiedArea, OffsetDateTime> {
        &self.clocks
    }

    /// The held areas whose clock is earlier than `since`, in order.
    pub fn untouched_since(&self, since: OffsetDateTime) -> BTreeSet<QualifiedArea> {
        self.clocks
            .iter()
            .filter(|(_, clock)| **clock < since)
            .map(|(area, _)| area.clone())
            .collect()
    }
}

impl Deref for ClaimAreas {
    type Target = BTreeSet<QualifiedArea>;

    fn deref(&self) -> &Self::Target {
        &self.held
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
    /// Whether the claim holds exactly these areas, whatever their origins and clocks.
    fn eq(&self, other: &BTreeSet<QualifiedArea>) -> bool {
        self.held == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{area, areas, at, declared};

    #[test]
    fn a_gained_area_is_held_and_known_as_gained() {
        let mut held = declared(&["api:billing"], 0);
        held.gain(area("api:search"), at(5));
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
    fn gaining_a_declared_area_keeps_it_declared_and_its_clock() {
        let mut held = declared(&["api:billing"], 0);
        held.gain(area("api:billing"), at(5));
        assert!(held.declares(&area("api:billing")));
        assert!(held.gained().is_empty());
        assert_eq!(held.clock(&area("api:billing")), Some(at(0)));
    }

    #[test]
    fn declaring_a_gained_area_makes_it_declared() {
        let mut held = ClaimAreas::default();
        held.gain(area("api:search"), at(0));
        held.declare(area("api:search"), at(5));
        assert!(held.declares(&area("api:search")));
        assert!(held.gained().is_empty());
    }

    #[test]
    fn giving_an_area_up_forgets_its_origin_and_its_clock() {
        let mut held = ClaimAreas::default();
        held.gain(area("api:search"), at(0));
        held.declare(area("api:billing"), at(0));
        assert!(held.remove(&area("api:search")));
        assert!(!held.remove(&area("api:search")));
        assert_eq!(held.clock(&area("api:search")), None);
        held.retain(|kept| *kept != area("api:billing"));
        assert!(held.is_empty());
        assert!(held.gained().is_empty());
        assert!(held.clocks().is_empty());
    }

    #[test]
    fn an_area_declared_or_gained_starts_its_clock_then() {
        let mut held = declared(&["api:billing"], 3);
        held.gain(area("api:search"), at(7));
        assert_eq!(held.clock(&area("api:billing")), Some(at(3)));
        assert_eq!(held.clock(&area("api:search")), Some(at(7)));
        assert_eq!(held.clock(&area("web:checkout")), None);
    }

    #[test]
    fn declaring_an_area_again_or_an_edit_in_it_moves_its_clock() {
        let mut held = declared(&["api:billing", "api:search"], 0);
        held.gain(area("web:checkout"), at(0));
        held.declare(area("api:billing"), at(10));
        assert!(held.note_edit(&area("api:search"), at(20)));
        assert!(held.note_edit(&area("web:checkout"), at(30)));
        assert!(!held.note_edit(&area("web:billing"), at(40)));
        assert_eq!(held.clock(&area("api:billing")), Some(at(10)));
        assert_eq!(held.clock(&area("api:search")), Some(at(20)));
        assert_eq!(
            held.clock(&area("web:checkout")),
            Some(at(30)),
            "an edit in a gained area moves its clock and keeps it gained"
        );
        assert_eq!(held.origin(&area("web:checkout")), Some(AreaOrigin::Gained));
        assert_eq!(held.clock(&area("web:billing")), None, "not held");
        assert_eq!(held.len(), 3);
    }

    #[test]
    fn restarting_the_clocks_keeps_every_area_and_its_origin() {
        let mut held = declared(&["api:billing"], 0);
        held.gain(area("api:search"), at(5));
        held.restart_clocks(at(100));
        assert_eq!(held.clock(&area("api:billing")), Some(at(100)));
        assert_eq!(held.clock(&area("api:search")), Some(at(100)));
        assert_eq!(held.gained(), &areas(&["api:search"]));
    }

    #[test]
    fn the_areas_untouched_since_a_time_are_those_whose_clock_is_earlier() {
        let mut held = declared(&["api:billing", "api:search"], 0);
        held.note_edit(&area("api:search"), at(10));
        assert_eq!(held.untouched_since(at(10)), areas(&["api:billing"]));
        assert_eq!(held.untouched_since(at(0)), areas(&[]));
        assert_eq!(
            held.untouched_since(at(11)),
            areas(&["api:billing", "api:search"])
        );
    }

    #[test]
    fn stored_parts_hold_every_area_with_its_clock_and_gained_ones_among_them() {
        let clocks = |named: &[&str]| -> BTreeMap<QualifiedArea, OffsetDateTime> {
            named.iter().map(|name| (area(name), at(1))).collect()
        };
        assert_eq!(
            ClaimAreas::new(
                areas(&["api:billing"]),
                areas(&["api:search"]),
                clocks(&["api:billing"])
            ),
            Err(ClaimAreasError::GainedNotHeld {
                area: area("api:search")
            })
        );
        assert_eq!(
            ClaimAreas::new(
                areas(&["api:billing", "api:search"]),
                areas(&[]),
                clocks(&["api:billing"])
            ),
            Err(ClaimAreasError::Unclocked {
                area: area("api:search")
            })
        );
        assert_eq!(
            ClaimAreas::new(
                areas(&["api:billing"]),
                areas(&[]),
                clocks(&["api:billing", "web:checkout"])
            ),
            Err(ClaimAreasError::ClockNotHeld {
                area: area("web:checkout")
            })
        );
        let read = ClaimAreas::new(
            areas(&["api:billing", "api:search"]),
            areas(&["api:search"]),
            clocks(&["api:billing", "api:search"]),
        )
        .unwrap();
        assert_eq!(read.gained(), &areas(&["api:search"]));
        assert!(read.declares(&area("api:billing")));
        assert_eq!(read.clock(&area("api:search")), Some(at(1)));
    }
}
