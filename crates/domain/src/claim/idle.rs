//! Which areas of a claim lie idle: held for a long time with no sign of the claim's work in them.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use super::Claim;
use crate::area::QualifiedArea;
use crate::paths::RelPath;
use crate::repo::RepoId;
use crate::settings::ProjectSettings;

/// One stored footprint of a claim as the idle rule reads it: the repository, the paths the
/// claim's branch changes there against its base, and whether its forge gave fewer paths than the
/// branch changes.
#[derive(Debug, Clone, Copy)]
pub struct FootprintPaths<'a> {
    /// The repository.
    pub repo: &'a RepoId,
    /// The paths.
    pub paths: &'a BTreeSet<RelPath>,
    /// Whether the paths are fewer than the branch changes.
    pub truncated: bool,
}

impl Claim {
    /// The areas that may lie idle, read from the claim alone: when it is live (in review, or
    /// seen within `stale_after`), the held areas whose clock is more than the project's
    /// `idle_area_after` before `now`. A claim that is not live has none, since it is left to the
    /// release rule. Only the areas named here need the claim's footprints to decide.
    pub fn idle_candidates(
        &self,
        now: OffsetDateTime,
        settings: &ProjectSettings,
    ) -> BTreeSet<QualifiedArea> {
        if !self.is_live(now, settings.stale_after()) {
            return BTreeSet::new();
        }
        self.areas.untouched_since(now - settings.idle_area_after())
    }
}

/// What the idle rule finds among a claim's [`Claim::idle_candidates`], given every stored
/// footprint of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdleAreas {
    /// The candidates that lie idle: to give back.
    pub idle: BTreeSet<QualifiedArea>,
    /// The candidates the claim's footprints keep: a path of them falls in the area, or the
    /// footprint of the area's repository is truncated. Their clocks start again at the sweep
    /// ([`IdleAreas::restart_kept`]), so that they are not candidates again until the idle delay
    /// has passed once more.
    pub kept: BTreeSet<QualifiedArea>,
}

impl IdleAreas {
    /// Whether the rule found no candidate at all.
    pub fn is_empty(&self) -> bool {
        self.idle.is_empty() && self.kept.is_empty()
    }

    /// Starts the clock of every kept area of `claim` again at `now`. It moves nothing else: when
    /// the claim was last seen stays as it was, since a footprint read at a sweep is no sign of the
    /// claim's work.
    pub fn restart_kept(&self, claim: &mut Claim, now: OffsetDateTime) {
        for area in &self.kept {
            claim.areas.restart_clock(area, now);
        }
    }
}

/// The candidates of the claim at `now` ([`Claim::idle_candidates`]), sorted by every stored
/// footprint of it: a candidate that a path of those footprints falls in is kept, and so is every
/// candidate of a repository whose footprint is truncated, since a path the forge left out may fall
/// in it; the other candidates lie idle. A path falls in the first area of its repository, in
/// declaration order, that contains it, as an edit of it would; a path of a repository the settings
/// no longer list falls in none. An area of a repository with no footprint is idle on its clock
/// alone.
pub fn idle_areas(
    claim: &Claim,
    footprints: &[FootprintPaths<'_>],
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> IdleAreas {
    let mut idle = claim.idle_candidates(now, settings);
    let mut kept = BTreeSet::new();
    if idle.is_empty() {
        return IdleAreas { idle, kept };
    }
    for footprint in footprints {
        if footprint.truncated {
            let (covered, rest): (BTreeSet<_>, BTreeSet<_>) = idle
                .into_iter()
                .partition(|area| area.repo == *footprint.repo);
            kept.extend(covered);
            idle = rest;
            continue;
        }
        let Some(spec) = settings.repo(footprint.repo) else {
            continue;
        };
        for path in footprint.paths {
            if let Some(area) = spec.area_for(path) {
                let area = spec.qualify(&area.name);
                if idle.remove(&area) {
                    kept.insert(area);
                }
            }
        }
    }
    IdleAreas { idle, kept }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::review::{ClaimState, PrRef, Review};
    use crate::testing::{BOB, area, areas, at, claim, two_repo_settings};

    const WEEK: i64 = 7 * 24 * 60;

    fn paths(texts: &[&str]) -> BTreeSet<RelPath> {
        texts
            .iter()
            .map(|text| RelPath::parse(text).unwrap())
            .collect()
    }

    /// Bob's claim on item 1 holding `held`, declared at the fixed instant and seen `seen` minutes
    /// after it.
    fn held_since_then(held: &[&str], seen: i64) -> Claim {
        let mut held = claim(1, BOB, held, 0);
        held.last_seen = at(seen);
        held
    }

    #[test]
    fn an_area_held_past_the_delay_with_no_path_of_the_branch_in_it_is_idle() {
        let settings = two_repo_settings();
        let now = at(WEEK + 1);
        let claim = held_since_then(&["api:billing", "api:search"], WEEK);
        let api = RepoId::parse("api").unwrap();
        let changed = paths(&["packages/search/index.rs", "README.md"]);
        let footprints = [FootprintPaths {
            repo: &api,
            paths: &changed,
            truncated: false,
        }];
        assert_eq!(
            idle_areas(&claim, &footprints, &settings, now).idle,
            areas(&["api:billing"]),
            "a path of the branch in search keeps it"
        );
        assert_eq!(
            idle_areas(&claim, &footprints, &settings, now).kept,
            areas(&["api:search"]),
            "kept by the footprint"
        );
    }

    #[test]
    fn an_area_whose_clock_is_within_the_delay_is_not_idle() {
        let settings = two_repo_settings();
        let mut claim = held_since_then(&["api:billing", "api:search"], WEEK);
        claim.areas.note_edit(&area("api:search"), at(60));
        assert_eq!(
            idle_areas(&claim, &[], &settings, at(WEEK)).idle,
            areas(&[]),
            "seven days are not more than seven days"
        );
        assert_eq!(
            idle_areas(&claim, &[], &settings, at(WEEK + 1)).idle,
            areas(&["api:billing"])
        );
    }

    #[test]
    fn a_truncated_footprint_keeps_every_area_of_its_repository_only() {
        let settings = two_repo_settings();
        let claim = held_since_then(&["api:billing", "web:checkout"], WEEK);
        let api = RepoId::parse("api").unwrap();
        let some = paths(&["README.md"]);
        let footprints = [FootprintPaths {
            repo: &api,
            paths: &some,
            truncated: true,
        }];
        assert_eq!(
            idle_areas(&claim, &footprints, &settings, at(WEEK + 1)).idle,
            areas(&["web:checkout"]),
            "an area of another repository with no footprint there goes on its clock alone"
        );
        assert_eq!(
            idle_areas(&claim, &footprints, &settings, at(WEEK + 1)).kept,
            areas(&["api:billing"])
        );
    }

    #[test]
    fn a_gained_area_is_idle_alike() {
        let settings = two_repo_settings();
        let mut claim = held_since_then(&[], WEEK);
        claim.areas.gain(area("web:billing"), at(0));
        assert_eq!(
            idle_areas(&claim, &[], &settings, at(WEEK + 1)).idle,
            areas(&["web:billing"])
        );
    }

    #[test]
    fn restarting_the_kept_areas_moves_their_clocks_alone() {
        let settings = two_repo_settings();
        let now = at(WEEK + 1);
        let mut claim = held_since_then(&["api:billing", "api:search"], WEEK);
        let api = RepoId::parse("api").unwrap();
        let changed = paths(&["packages/search/index.rs"]);
        let footprints = [FootprintPaths {
            repo: &api,
            paths: &changed,
            truncated: false,
        }];
        let found = idle_areas(&claim, &footprints, &settings, now);
        assert!(!found.is_empty());
        let seen = claim.last_seen;
        found.restart_kept(&mut claim, now);
        assert_eq!(claim.areas.clock(&area("api:search")), Some(now));
        assert_eq!(claim.areas.clock(&area("api:billing")), Some(at(0)));
        assert_eq!(claim.last_seen, seen, "no sign of the claim's work");
        assert_eq!(
            idle_areas(&claim, &footprints, &settings, now),
            IdleAreas {
                idle: areas(&["api:billing"]),
                kept: areas(&[]),
            },
            "a kept area is no candidate until the delay passes again"
        );
        assert!(idle_areas(&claim, &[], &settings, at(0)).is_empty());
    }

    #[test]
    fn a_path_falls_in_the_first_area_that_contains_it() {
        let settings = two_repo_settings();
        let claim = held_since_then(&["api:billing", "api:billing-docs"], WEEK);
        let api = RepoId::parse("api").unwrap();
        let docs = paths(&["packages/billing/docs/guide.md"]);
        let footprints = [FootprintPaths {
            repo: &api,
            paths: &docs,
            truncated: false,
        }];
        assert_eq!(
            idle_areas(&claim, &footprints, &settings, at(WEEK + 1)).idle,
            areas(&["api:billing-docs"])
        );
    }

    #[test]
    fn a_claim_that_is_not_live_has_no_idle_area_and_one_in_review_has() {
        let settings = two_repo_settings();
        let stale = held_since_then(&["api:billing"], 0);
        let now = at(WEEK + 1);
        assert!(stale.idle_candidates(now, &settings).is_empty());
        assert!(idle_areas(&stale, &[], &settings, now).idle.is_empty());
        let mut reviewing = stale.clone();
        let pr = PrRef::parse("acme/api#9").unwrap();
        reviewing.state = ClaimState::InReview(Review::new([pr].into(), at(0), false).unwrap());
        assert_eq!(
            idle_areas(&reviewing, &[], &settings, now).idle,
            areas(&["api:billing"])
        );
        assert_eq!(
            reviewing.idle_candidates(at(0) + Duration::days(7), &settings),
            areas(&[])
        );
    }
}
