//! The edits a machine's hooks let through without asking the server, as a heartbeat reports them,
//! and the claims those edits show work under.

use std::collections::BTreeSet;

use serde::Serialize;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use super::{live_claims_of, work_target};
use crate::area::QualifiedArea;
use crate::claim::Claim;
use crate::ids::{ItemId, MemberId};

/// What a member's hooks on one machine let through without asking the server, since the last
/// heartbeat that reported it: the areas of edits answered from what the machine knew the member
/// holds, and whether an edit was made in a path no area governs. An edit the server checked is not
/// in it, since the check itself counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReportedEdits {
    areas: BTreeSet<QualifiedArea>,
    outside: bool,
}

/// Why reported edits cannot be taken.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReportedEditsError {
    /// More areas than one report may name.
    #[error("a heartbeat reports edits in at most {max} areas")]
    TooManyAreas {
        /// The most areas one report may name.
        max: usize,
    },
}

impl ReportedEdits {
    /// The most areas one report may name.
    pub const MAX_AREAS: usize = 200;

    /// Edits in `areas`, and outside every area when `outside`.
    ///
    /// # Errors
    /// [`ReportedEditsError::TooManyAreas`] for more than [`ReportedEdits::MAX_AREAS`] areas.
    pub fn new(areas: BTreeSet<QualifiedArea>, outside: bool) -> Result<Self, ReportedEditsError> {
        if areas.len() > Self::MAX_AREAS {
            return Err(ReportedEditsError::TooManyAreas {
                max: Self::MAX_AREAS,
            });
        }
        Ok(Self { areas, outside })
    }

    /// The areas edits were made in.
    pub fn areas(&self) -> &BTreeSet<QualifiedArea> {
        &self.areas
    }

    /// Whether an edit was made in a path no area governs.
    pub fn outside(&self) -> bool {
        self.outside
    }

    /// Whether it reports no edit at all.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty() && !self.outside
    }
}

/// The claims of `me` the reported edits show work under, as an edit check attributes edits: every
/// live claim of theirs that holds a reported area, declared or gained, from any session or host;
/// and, when an edit was made outside every area, their target claim (of the live claims they work
/// on the one most recently claimed or renewed, else of those in review by the same rule). An area
/// none of their live claims holds shows nothing, and a report of no edit shows no claim.
pub fn claims_shown_by(
    edits: &ReportedEdits,
    me: MemberId,
    claims: &[Claim],
    now: OffsetDateTime,
    stale_after: Duration,
) -> BTreeSet<ItemId> {
    let mine = live_claims_of(me, claims, now, stale_after);
    let mut shown: BTreeSet<ItemId> = mine
        .iter()
        .filter(|claim| edits.areas.iter().any(|area| claim.holds(area)))
        .map(|claim| claim.item)
        .collect();
    if edits.outside
        && let Some(target) = work_target(&mine)
    {
        shown.insert(target.item);
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ALICE, BOB, area, areas, at, claim};

    const STALE_AFTER: Duration = Duration::hours(24);

    fn edits(named: &[&str], outside: bool) -> ReportedEdits {
        ReportedEdits::new(areas(named), outside).unwrap()
    }

    fn shown(edits: &ReportedEdits, claims: &[Claim]) -> Vec<u64> {
        claims_shown_by(edits, BOB, claims, at(60), STALE_AFTER)
            .into_iter()
            .map(|item| item.0)
            .collect()
    }

    #[test]
    fn a_reported_area_shows_every_live_claim_of_the_member_that_holds_it() {
        let claims = [
            claim(1, BOB, &["api:billing"], 0),
            claim(2, BOB, &["api:billing", "api:search"], 10),
            claim(3, BOB, &["api:search"], 20),
            claim(4, ALICE, &["api:billing"], 30),
        ];
        assert_eq!(shown(&edits(&["api:billing"], false), &claims), [1, 2]);
    }

    #[test]
    fn an_area_gained_by_an_edit_is_held_like_a_declared_one() {
        let mut gained = claim(1, BOB, &[], 0);
        gained.areas.gain(area("api:billing"), at(0));
        assert_eq!(shown(&edits(&["api:billing"], false), &[gained]), [1]);
    }

    #[test]
    fn an_area_the_member_does_not_hold_shows_nothing() {
        let claims = [
            claim(1, BOB, &["api:search"], 0),
            claim(2, ALICE, &["api:billing"], 0),
        ];
        assert!(shown(&edits(&["api:billing"], false), &claims).is_empty());
    }

    #[test]
    fn a_stale_claim_is_not_shown_by_an_area_it_holds() {
        let stale = claim(1, BOB, &["api:billing"], 0);
        let now = at(0) + STALE_AFTER + Duration::minutes(1);
        let shown = claims_shown_by(
            &edits(&["api:billing"], true),
            BOB,
            &[stale],
            now,
            STALE_AFTER,
        );
        assert!(shown.is_empty());
    }

    #[test]
    fn an_edit_outside_every_area_shows_only_the_target_claim() {
        let claims = [
            claim(1, BOB, &["api:billing"], 0),
            claim(2, BOB, &["api:search"], 30),
            claim(3, BOB, &[], 10),
            claim(4, ALICE, &[], 50),
        ];
        assert_eq!(shown(&edits(&[], true), &claims), [2]);
    }

    #[test]
    fn the_target_is_a_claim_worked_on_before_one_in_review() {
        use crate::review::{ClaimState, PrRef, Review};

        let pr = PrRef::parse("acme/api#9").unwrap();
        let mut reviewing = claim(2, BOB, &[], 30);
        reviewing.state = ClaimState::InReview(Review::new([pr].into(), at(30), false).unwrap());
        let working = claim(1, BOB, &[], 0);
        assert_eq!(shown(&edits(&[], true), &[working, reviewing.clone()]), [1]);
        assert_eq!(shown(&edits(&[], true), &[reviewing]), [2]);
    }

    #[test]
    fn an_empty_report_shows_no_claim() {
        let claims = [claim(1, BOB, &["api:billing"], 0)];
        let empty = ReportedEdits::default();
        assert!(empty.is_empty());
        assert!(shown(&empty, &claims).is_empty());
    }

    #[test]
    fn a_report_names_at_most_two_hundred_areas() {
        let named = |count: usize| -> BTreeSet<QualifiedArea> {
            (0..count).map(|n| area(&format!("api:area-{n}"))).collect()
        };
        assert!(ReportedEdits::new(named(ReportedEdits::MAX_AREAS), false).is_ok());
        assert_eq!(
            ReportedEdits::new(named(ReportedEdits::MAX_AREAS + 1), false),
            Err(ReportedEditsError::TooManyAreas { max: 200 })
        );
    }
}
