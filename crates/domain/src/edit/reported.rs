//! The edits a machine's hooks let through without asking the server, as a heartbeat reports them,
//! and the claims those edits show work under.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use super::{live_claims_of, taker_of, target_for, work_target};
use crate::area::QualifiedArea;
use crate::claim::{Claim, held_by_other};
use crate::ids::{ItemId, MemberId};

/// What a member's hooks on one machine let through without asking the server, since the last
/// heartbeat that reported it: the areas of edits answered from what the machine knew the member
/// holds or let through while the server gave no decision — an area nobody holds included — and
/// whether such an edit was made in a path no area governs. An edit the server decided is not in
/// it, since the check itself counted. [`claims_shown_by`] says which claims each part shows.
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

/// The claims of `me` the reported edits show work under, each with the reported areas attributed
/// to it, as an edit check attributes edits. An edit in an area one of their live claims answers
/// for goes to one claim, the one an edit check would take it to: of their live claims, from any
/// session or host, the target among those that declared the area, else among their wide claims
/// that name it, else among those that only gained it, and that claim is shown with the area.
/// Their other claims that hold the area are not shown by it, so a claim whose work stopped does
/// not stay alive on the edits of another claim sharing its area. An area another member's live
/// claim holds shows nothing. An area no live claim of anybody holds and none of theirs answers
/// for shows their target claim among those that do not wait for the area, with no area of its
/// own: the edit check would give that claim the area, and only the edit check does. An edit
/// outside every area shows their target claim (of the live claims they work on the one most
/// recently claimed or renewed, else of those in review by the same rule), with no area of its own.
/// A member with no live claim, and a report of no edit, show no claim.
pub fn claims_shown_by(
    edits: &ReportedEdits,
    me: MemberId,
    claims: &[Claim],
    now: OffsetDateTime,
    stale_after: Duration,
) -> BTreeMap<ItemId, BTreeSet<QualifiedArea>> {
    let mine = live_claims_of(me, claims, now, stale_after);
    let mut shown: BTreeMap<ItemId, BTreeSet<QualifiedArea>> = BTreeMap::new();
    for area in &edits.areas {
        if held_by_other(area, me, claims, now, stale_after).is_some() {
            continue;
        }
        if let Some(taker) = taker_of(&mine, area) {
            shown.entry(taker.item).or_default().insert(area.clone());
        } else if let Some(target) = target_for(&mine, area) {
            shown.entry(target.item).or_default();
        }
    }
    if edits.outside
        && let Some(target) = work_target(&mine)
    {
        shown.entry(target.item).or_default();
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ALICE, BOB, area, areas, at, claim};
    use crate::wide::{WideReason, WideScope};

    const STALE_AFTER: Duration = Duration::hours(24);

    fn edits(named: &[&str], outside: bool) -> ReportedEdits {
        ReportedEdits::new(areas(named), outside).unwrap()
    }

    fn billing_scope() -> WideScope {
        WideScope::new(
            areas(&["api:billing"]),
            WideReason::parse("rename money").unwrap(),
        )
        .unwrap()
    }

    /// The claim waiting for the area from the claim on `from`.
    fn waiting_for(mut claim: Claim, text: &str, from: u64) -> Claim {
        claim.pending.insert(
            area(text),
            crate::handover::PendingArea::waiting_for(ItemId(from)),
        );
        claim
    }

    fn shown(edits: &ReportedEdits, claims: &[Claim]) -> Vec<u64> {
        claims_shown_by(edits, BOB, claims, at(60), STALE_AFTER)
            .into_keys()
            .map(|item| item.0)
            .collect()
    }

    fn areas_of(edits: &ReportedEdits, claims: &[Claim]) -> Vec<(u64, Vec<String>)> {
        claims_shown_by(edits, BOB, claims, at(60), STALE_AFTER)
            .into_iter()
            .map(|(item, areas)| (item.0, areas.iter().map(ToString::to_string).collect()))
            .collect()
    }

    #[test]
    fn each_reported_area_shows_one_live_claim_of_the_member_with_that_area() {
        let claims = [
            claim(1, BOB, &["api:billing"], 0),
            claim(2, BOB, &["api:billing", "api:search"], 10),
            claim(3, BOB, &["api:search"], 20),
            claim(4, ALICE, &["api:checkout"], 30),
        ];
        assert_eq!(
            areas_of(&edits(&["api:billing", "api:search"], false), &claims),
            [
                (2, vec!["api:billing".to_owned()]),
                (3, vec!["api:search".to_owned()]),
            ]
        );
    }

    #[test]
    fn an_area_two_claims_of_the_member_declared_shows_only_the_one_an_edit_check_targets() {
        let claims = [
            claim(41, BOB, &["api:billing"], 0),
            claim(42, BOB, &["api:billing"], 30),
        ];
        assert_eq!(shown(&edits(&["api:billing"], false), &claims), [42]);
    }

    #[test]
    fn a_claim_that_declared_the_area_is_shown_before_a_wide_one_and_a_wide_one_before_a_gainer() {
        let mut gainer = claim(3, BOB, &[], 30);
        gainer.areas.gain(area("api:billing"), at(30));
        let mut wide = claim(2, BOB, &[], 20);
        wide.wide = Some(billing_scope());
        let declarer = claim(1, BOB, &["api:billing"], 0);
        let billing = edits(&["api:billing"], false);
        let all = [declarer, wide.clone(), gainer.clone()];
        assert_eq!(shown(&billing, &all), [1]);
        assert_eq!(shown(&billing, &[wide, gainer.clone()]), [2]);
        assert_eq!(shown(&billing, &[gainer]), [3]);
    }

    #[test]
    fn an_area_another_member_holds_shows_nothing_though_a_wide_claim_names_it() {
        let mut wide = claim(1, BOB, &[], 0);
        wide.wide = Some(billing_scope());
        let claims = [wide, claim(2, ALICE, &["api:billing"], 10)];
        assert!(shown(&edits(&["api:billing"], false), &claims).is_empty());
    }

    #[test]
    fn the_target_shown_by_an_edit_outside_every_area_takes_no_area_of_its_own() {
        let claims = [
            claim(1, BOB, &["api:billing"], 0),
            claim(2, BOB, &["api:search"], 30),
        ];
        assert_eq!(
            areas_of(&edits(&["api:billing"], true), &claims),
            [(1, vec!["api:billing".to_owned()]), (2, vec![])]
        );
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
    fn an_area_nobody_holds_shows_the_target_claim_with_no_area_of_its_own() {
        let claims = [claim(1, BOB, &["api:billing"], 0)];
        assert_eq!(
            areas_of(&edits(&["api:search"], false), &claims),
            [(1, vec![])]
        );
        let older_working = claim(2, BOB, &["api:checkout"], 10);
        let newer = claim(3, BOB, &["api:payments"], 40);
        let newer_waiting = waiting_for(newer.clone(), "api:search", 7);
        assert_eq!(
            shown(
                &edits(&["api:search"], false),
                &[older_working.clone(), newer]
            ),
            [3]
        );
        assert_eq!(
            shown(
                &edits(&["api:search"], false),
                &[older_working, newer_waiting]
            ),
            [2],
            "as the edit check does, a claim that waits for the area is not its target"
        );
    }

    #[test]
    fn an_area_nobody_holds_shows_nothing_to_a_member_without_a_live_claim() {
        let stale = claim(1, BOB, &["api:billing"], 0);
        let now = at(0) + STALE_AFTER + Duration::minutes(1);
        let shown = claims_shown_by(
            &edits(&["api:search"], false),
            BOB,
            &[stale, claim(2, ALICE, &["api:checkout"], 0)],
            now,
            STALE_AFTER,
        );
        assert!(shown.is_empty());
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
