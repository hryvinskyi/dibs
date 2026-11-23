//! Grants dibs gives on a holder's behalf: the test that reads only what dibs stores — the claims,
//! the grants, the footprints of the holder's claim, the revokes that stick — and never the text of
//! a message.

use time::OffsetDateTime;

use super::{AskedGrant, Grant, GrantDecision};
use crate::area::QualifiedArea;
use crate::claim::{Claim, FootprintPaths, idle_areas};
use crate::ids::{ItemId, MemberId};
use crate::paths::RelPath;
use crate::settings::ProjectSettings;

/// A revoke that sticks: once a grant from `holder` to `asker` in `area` is revoked, dibs never
/// grants that asker that area on that holder's behalf again while both claims last. The holder
/// may.
///
/// Each claim is told by when it was claimed, which claiming the item again later changes and
/// renewing it does not, so a block lasts exactly as long as the two claims it was recorded
/// between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantBlock {
    /// The member who had been granted.
    pub asker: MemberId,
    /// The item of the asker's claim.
    pub asker_item: ItemId,
    /// When the asker claimed that item.
    pub asker_claimed_at: OffsetDateTime,
    /// The holder who had granted.
    pub holder: MemberId,
    /// The item of the holder's claim that held the area.
    pub holder_item: ItemId,
    /// When the holder claimed that item.
    pub holder_claimed_at: OffsetDateTime,
    /// The area.
    pub area: QualifiedArea,
    /// When the grant was revoked.
    pub revoked_at: OffsetDateTime,
}

impl GrantBlock {
    /// Whether the block stands between `asker` and `holder` in `area` among these claims: it was
    /// recorded between those two members in that area, and both of its claims are still there,
    /// each as claimed then. Whether either claim is live does not matter, so a claim that went
    /// stale and came back is still blocked. Serves a request and one area of a wide claim's
    /// announcement alike, the wide claimer being the asker.
    pub fn applies(
        &self,
        asker: MemberId,
        holder: MemberId,
        area: &QualifiedArea,
        claims: &[Claim],
    ) -> bool {
        let stands = |member: MemberId, item: ItemId, claimed_at: OffsetDateTime| {
            claims.iter().any(|claim| {
                claim.member == member && claim.item == item && claim.claimed_at == claimed_at
            })
        };
        self.asker == asker
            && self.holder == holder
            && self.area == *area
            && stands(self.asker, self.asker_item, self.asker_claimed_at)
            && stands(self.holder, self.holder_item, self.holder_claimed_at)
    }

    /// Whether the block stands against this request among these claims (see
    /// [`GrantBlock::applies`]).
    pub fn applies_to(&self, asked: &AskedGrant, claims: &[Claim]) -> bool {
        self.applies(asked.ask.asker, asked.holder, &asked.ask.area, claims)
    }
}

/// Whether a revoke between `asker` and `holder` has stopped applying: one was recorded between
/// them in an area `in_area` accepts, and none of those stands among these claims any more (see
/// [`GrantBlock::applies`]), because one of the two claims it was recorded between has ended or
/// was claimed again. With no such revoke recorded at all, the answer is `false`: nothing says one
/// ever stood.
pub fn revoke_outlived(
    blocks: &[GrantBlock],
    asker: MemberId,
    holder: MemberId,
    in_area: impl Fn(&QualifiedArea) -> bool,
    claims: &[Claim],
) -> bool {
    let mut recorded = blocks
        .iter()
        .filter(|block| block.asker == asker && block.holder == holder && in_area(&block.area))
        .peekable();
    recorded.peek().is_some()
        && recorded.all(|block| !block.applies(asker, holder, &block.area, claims))
}

/// What dibs stores that bears on a request it may grant on the holder's behalf.
#[derive(Debug, Clone, Copy)]
pub struct GrantFacts<'a> {
    /// The project's grants.
    pub grants: &'a [Grant],
    /// The project's claims.
    pub claims: &'a [Claim],
    /// The stored footprints of the holder's claim, in any repository.
    pub footprints: &'a [FootprintPaths<'a>],
    /// The revokes that stick.
    pub blocks: &'a [GrantBlock],
}

/// What dibs does with a request for a grant on the holder's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnBehalf {
    /// Grant it now, as the holder's grant.
    Grant {
        /// The item of the holder's claim that holds the area.
        holder_item: ItemId,
        /// Why dibs grants it.
        because: GrantDecision,
    },
    /// Nothing lets dibs grant it yet; at `until` the wait does.
    Wait {
        /// When the request has waited the project's `grant_after`.
        until: OffsetDateTime,
    },
    /// The request does not wait for this holder for now — a claim went stale, a grant is in
    /// force, another member holds the area — and may wait again later.
    Lapsed,
    /// A revoke between these claims sticks: dibs never grants it, and the holder may.
    LeftToHolder,
    /// The claim the request was asked under has ended, or its holder holds the area through no
    /// claim at all: it is settled, and never judged again.
    Closed,
}

/// Decides whether dibs grants a request on the holder's behalf, from stored facts only.
///
/// The request is **closed** when the asker holds no claim, live or stale, on its item that was
/// claimed at or before `asked_at`: a request asked under a claim that has since ended never comes
/// back, not even when the asker claims the item again. It is closed too when its holder holds the
/// area through no claim, live or stale — they released the claim, or narrowed the area away — so
/// that a holder who holds the area again later, through a new claim, is not granted at once for
/// a wait timed from a request they had given up. A holder whose claim only went stale keeps the
/// request, since a stale claim still names the area. It has **lapsed** while it does not wait
/// for its holder ([`AskedGrant::waits_on`]). It is **left to the holder** when a revoke between
/// the asker and the holder in the area sticks ([`GrantBlock::applies_to`]). Otherwise the
/// holder's live claim that holds the area is judged by [`judge_area`]; when that gives no reason,
/// the request waits until it has waited the project's `grant_after`, and is granted from then on.
pub fn decide_on_holders_behalf(
    asked: &AskedGrant,
    asked_at: OffsetDateTime,
    facts: &GrantFacts<'_>,
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> OnBehalf {
    let asked_under = facts.claims.iter().any(|claim| {
        claim.member == asked.ask.asker
            && claim.item == asked.ask.item
            && claim.claimed_at <= asked_at
    });
    let holder_holds = facts
        .claims
        .iter()
        .any(|claim| claim.member == asked.holder && claim.holds(&asked.ask.area));
    if !asked_under || !holder_holds {
        return OnBehalf::Closed;
    }
    let Some(holder_item) = asked.waits_on(facts.grants, facts.claims, now, settings) else {
        return OnBehalf::Lapsed;
    };
    if facts
        .blocks
        .iter()
        .any(|block| block.applies_to(asked, facts.claims))
    {
        return OnBehalf::LeftToHolder;
    }
    let Some(holder_claim) = facts
        .claims
        .iter()
        .find(|claim| claim.member == asked.holder && claim.item == holder_item)
    else {
        return OnBehalf::Lapsed;
    };
    if let Some(because) = judge_area(
        holder_claim,
        &asked.ask.area,
        facts.footprints,
        settings,
        now,
    ) {
        return OnBehalf::Grant {
            holder_item,
            because,
        };
    }
    let until = asked_at + settings.grant_after();
    if now >= until {
        OnBehalf::Grant {
            holder_item,
            because: GrantDecision::Waited,
        }
    } else {
        OnBehalf::Wait { until }
    }
}

/// Why dibs may give edits in `area` on behalf of the holder of `holder_claim`, the claim that
/// holds it, at once; `None` when only waiting lets it, which is the caller's to apply. The first
/// that holds names the reason:
///
/// 1. the claim is in review, whatever its pull requests change: [`GrantDecision::InReview`];
/// 2. the area lies idle on the claim by the idle rule ([`idle_areas`]), not merely by its clock: a
///    candidate a path of the footprints falls in, or of a repository whose footprint is
///    truncated, is kept by that rule and is not idle: [`GrantDecision::Idle`];
/// 3. the claim has a footprint in the area's repository, none truncated, and none of its paths is
///    in the area: [`GrantDecision::Untouched`]. A path any glob of the area matches is in it, even
///    when an area declared earlier contains it too; so is a path the repository's hot-file rules
///    make a path of the area, as the edit check decides it ([`RepoSpec::hot_area_for`]: a file of
///    an `owner` rule naming the area). With no footprint there, or a truncated one, or an area the
///    settings do not declare, this never holds.
///
/// [`RepoSpec::hot_area_for`]: crate::settings::RepoSpec::hot_area_for
pub fn judge_area(
    holder_claim: &Claim,
    area: &QualifiedArea,
    footprints: &[FootprintPaths<'_>],
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> Option<GrantDecision> {
    if holder_claim.state.is_in_review() {
        return Some(GrantDecision::InReview);
    }
    if idle_areas(holder_claim, footprints, settings, now)
        .idle
        .contains(area)
    {
        return Some(GrantDecision::Idle);
    }
    let repo = settings.repo(&area.repo)?;
    let declared = repo.area(&area.name)?;
    let in_area = |path: &RelPath| declared.contains(path) || repo.hot_area_for(path) == Some(area);
    let mut in_repo = footprints
        .iter()
        .filter(|footprint| *footprint.repo == area.repo)
        .peekable();
    in_repo.peek()?;
    let untouched =
        in_repo.all(|footprint| !footprint.truncated && !footprint.paths.iter().any(in_area));
    untouched.then_some(GrantDecision::Untouched)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::Duration;

    use super::*;
    use crate::grant::GrantAsk;
    use crate::ids::MessageId;
    use crate::paths::RelPath;
    use crate::repo::RepoId;
    use crate::review::{ClaimState, PrRef, Review};
    use crate::testing::{ALICE, BOB, CAROL, area, at, claim, two_repo_settings};

    const WEEK: i64 = 7 * 24 * 60;

    /// Bob asks Alice, at `asked_at`, for edits in `area_text` for his item 2.
    fn bobs_request(area_text: &str) -> AskedGrant {
        AskedGrant {
            message: MessageId(91),
            ask: GrantAsk {
                asker: BOB,
                item: ItemId(2),
                area: area(area_text),
            },
            holder: ALICE,
        }
    }

    fn paths(texts: &[&str]) -> BTreeSet<RelPath> {
        texts
            .iter()
            .map(|text| RelPath::parse(text).unwrap())
            .collect()
    }

    fn api() -> RepoId {
        RepoId::parse("api").unwrap()
    }

    fn in_review(mut held: Claim) -> Claim {
        let pr = PrRef::parse("acme/api#9").unwrap();
        held.state = ClaimState::InReview(Review::new([pr].into(), at(0), false).unwrap());
        held
    }

    /// Alice holding `held` on item 1 and Bob working on item 2, both claimed at the fixed instant.
    fn both(held: &[&str]) -> Vec<Claim> {
        vec![claim(1, ALICE, held, 0), claim(2, BOB, &[], 0)]
    }

    /// The same claims, their areas declared at the fixed instant and both seen a week later, so
    /// that the areas' clocks are past the idle delay while the claims are live.
    fn a_week_on(held: &[&str]) -> Vec<Claim> {
        let mut claims = both(held);
        for one in &mut claims {
            one.last_seen = at(WEEK);
        }
        claims
    }

    fn decide(
        request: &AskedGrant,
        asked_at: OffsetDateTime,
        claims: &[Claim],
        footprints: &[FootprintPaths<'_>],
        now: OffsetDateTime,
    ) -> OnBehalf {
        let facts = GrantFacts {
            grants: &[],
            claims,
            footprints,
            blocks: &[],
        };
        decide_on_holders_behalf(request, asked_at, &facts, &two_repo_settings(), now)
    }

    fn granted(because: GrantDecision) -> OnBehalf {
        OnBehalf::Grant {
            holder_item: ItemId(1),
            because,
        }
    }

    #[test]
    fn a_claim_in_review_grants_even_with_a_path_of_its_branch_in_the_area() {
        let claims = [
            in_review(claim(1, ALICE, &["api:billing"], 0)),
            claim(2, BOB, &[], 0),
        ];
        let changed = paths(&["packages/billing/rates.rs"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &changed,
            truncated: false,
        }];
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(1),
                &claims,
                &footprints,
                at(2)
            ),
            granted(GrantDecision::InReview)
        );
    }

    #[test]
    fn an_idle_area_grants() {
        let claims = a_week_on(&["api:billing"]);
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(WEEK),
                &claims,
                &[],
                at(WEEK + 1)
            ),
            granted(GrantDecision::Idle),
            "an idle candidate of a repository with no footprint is idle on its clock alone"
        );
    }

    #[test]
    fn an_area_no_path_of_the_branch_is_in_grants() {
        let claims = both(&["api:billing"]);
        let changed = paths(&["packages/search/index.rs", "README.md"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &changed,
            truncated: false,
        }];
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(1),
                &claims,
                &footprints,
                at(2)
            ),
            granted(GrantDecision::Untouched)
        );
    }

    #[test]
    fn a_path_the_areas_globs_match_waits_until_the_grant_delay_has_passed() {
        let claims = both(&["api:billing-docs"]);
        let changed = paths(&["packages/billing/docs/guide.md"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &changed,
            truncated: false,
        }];
        assert_eq!(
            decide(
                &bobs_request("api:billing-docs"),
                at(1),
                &claims,
                &footprints,
                at(2)
            ),
            OnBehalf::Wait { until: at(11) }
        );
        let wider = both(&["api:billing"]);
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(1),
                &wider,
                &footprints,
                at(2)
            ),
            OnBehalf::Wait { until: at(11) },
            "a path in an area declared later still touches the earlier area whose globs match it"
        );
    }

    #[test]
    fn a_truncated_footprint_waits() {
        let claims = both(&["api:billing"]);
        let changed = paths(&["README.md"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &changed,
            truncated: true,
        }];
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(1),
                &claims,
                &footprints,
                at(2)
            ),
            OnBehalf::Wait { until: at(11) }
        );
    }

    #[test]
    fn no_footprint_in_the_areas_repository_waits() {
        let claims = both(&["api:billing"]);
        let web = RepoId::parse("web").unwrap();
        let changed = paths(&["src/checkout/cart.ts"]);
        let elsewhere = [FootprintPaths {
            repo: &web,
            paths: &changed,
            truncated: false,
        }];
        for footprints in [&[][..], &elsewhere[..]] {
            assert_eq!(
                decide(
                    &bobs_request("api:billing"),
                    at(1),
                    &claims,
                    footprints,
                    at(2)
                ),
                OnBehalf::Wait { until: at(11) }
            );
        }
    }

    #[test]
    fn past_the_grant_delay_dibs_grants_for_having_waited() {
        let claims = both(&["api:billing"]);
        assert_eq!(
            decide(&bobs_request("api:billing"), at(1), &claims, &[], at(30)),
            granted(GrantDecision::Waited)
        );
    }

    #[test]
    fn exactly_at_the_end_of_the_grant_delay_dibs_grants() {
        let claims = both(&["api:billing"]);
        let request = bobs_request("api:billing");
        let end = at(1) + two_repo_settings().grant_after();
        assert_eq!(
            decide(&request, at(1), &claims, &[], end - Duration::seconds(1)),
            OnBehalf::Wait { until: end }
        );
        assert_eq!(
            decide(&request, at(1), &claims, &[], end),
            granted(GrantDecision::Waited)
        );
    }

    #[test]
    fn a_grant_already_in_force_lapses_the_request() {
        let claims = both(&["api:billing"]);
        let grants = [Grant {
            item: ItemId(2),
            area: area("api:billing"),
            from: ALICE,
            to: BOB,
        }];
        let facts = GrantFacts {
            grants: &grants,
            claims: &claims,
            footprints: &[],
            blocks: &[],
        };
        assert_eq!(
            decide_on_holders_behalf(
                &bobs_request("api:billing"),
                at(1),
                &facts,
                &two_repo_settings(),
                at(30)
            ),
            OnBehalf::Lapsed
        );
    }

    #[test]
    fn a_stale_holder_claim_lapses_the_request() {
        let mut claims = both(&["api:billing"]);
        let late = at(0) + two_repo_settings().stale_after() + Duration::minutes(1);
        claims[1].last_seen = late;
        assert_eq!(
            decide(&bobs_request("api:billing"), at(1), &claims, &[], late),
            OnBehalf::Lapsed
        );
    }

    #[test]
    fn a_request_whose_claim_ended_is_closed_even_when_the_item_is_claimed_again() {
        let request = bobs_request("api:billing");
        let released = [claim(1, ALICE, &["api:billing"], 0)];
        assert_eq!(
            decide(&request, at(1), &released, &[], at(30)),
            OnBehalf::Closed
        );
        let claimed_again = [claim(1, ALICE, &["api:billing"], 0), claim(2, BOB, &[], 20)];
        assert_eq!(
            decide(&request, at(1), &claimed_again, &[], at(30)),
            OnBehalf::Closed,
            "a new claim never revives a request asked under an old one"
        );
        let the_same = both(&["api:billing"]);
        assert_eq!(
            decide(&request, at(0), &the_same, &[], at(30)),
            granted(GrantDecision::Waited),
            "a request asked at the instant of the claim is asked under it"
        );
    }

    #[test]
    fn a_request_whose_holder_holds_the_area_through_no_claim_is_closed() {
        let request = bobs_request("api:billing");
        let narrowed = both(&["api:search"]);
        assert_eq!(
            decide(&request, at(1), &narrowed, &[], at(30)),
            OnBehalf::Closed,
            "the holder narrowed the area away"
        );
        let released = [claim(2, BOB, &[], 0)];
        assert_eq!(
            decide(&request, at(1), &released, &[], at(30)),
            OnBehalf::Closed,
            "the holder released the claim"
        );
    }

    #[test]
    fn a_request_whose_holder_only_went_stale_is_not_closed() {
        let mut claims = both(&["api:billing"]);
        let late = at(0) + two_repo_settings().stale_after() + Duration::minutes(1);
        claims[1].last_seen = late;
        assert_eq!(
            decide(&bobs_request("api:billing"), at(1), &claims, &[], late),
            OnBehalf::Lapsed,
            "a stale claim still names the area"
        );
    }

    /// The holder gives the area up between two sweeps and holds it again later through a new
    /// claim. The sweep in the gap sees the request closed and settles it, so the request is never
    /// judged again; its wait is not timed again from when it was asked, days before.
    #[test]
    fn a_request_is_closed_in_the_gap_before_its_holder_holds_the_area_again() {
        let request = bobs_request("api:billing");
        let held = both(&["api:billing"]);
        assert_eq!(
            decide(&request, at(1), &held, &[], at(5)),
            OnBehalf::Wait { until: at(11) },
            "while the holder holds the area, the request waits"
        );
        let gap = [claim(2, BOB, &[], 0)];
        assert_eq!(
            decide(&request, at(1), &gap, &[], at(6)),
            OnBehalf::Closed,
            "once the holder holds it through no claim, the request is closed"
        );
    }

    #[test]
    fn a_file_a_hot_rule_gives_the_area_touches_it() {
        let changed = paths(&["deploy/billing.yaml"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &changed,
            truncated: false,
        }];
        let claims = both(&["api:billing", "api:search"]);
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(1),
                &claims,
                &footprints,
                at(2)
            ),
            OnBehalf::Wait { until: at(11) },
            "the edit check decides the file as a path of billing, so billing is touched"
        );
        assert_eq!(
            decide(
                &bobs_request("api:search"),
                at(1),
                &claims,
                &footprints,
                at(2)
            ),
            granted(GrantDecision::Untouched),
            "the rule gives the file to billing, not to search"
        );
    }

    #[test]
    fn an_idle_candidate_a_path_of_the_branch_lies_in_is_not_idle() {
        let claims = a_week_on(&["api:billing"]);
        let changed = paths(&["packages/billing/rates.rs"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &changed,
            truncated: false,
        }];
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(WEEK),
                &claims,
                &footprints,
                at(WEEK + 1)
            ),
            OnBehalf::Wait {
                until: at(WEEK + 10)
            },
            "the idle rule keeps it, so it waits for the grant delay"
        );
    }

    #[test]
    fn an_idle_candidate_whose_branch_changes_a_file_a_hot_rule_gives_it_is_not_idle() {
        let claims = a_week_on(&["api:billing"]);
        let changed = paths(&["deploy/billing.yaml"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &changed,
            truncated: false,
        }];
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(WEEK),
                &claims,
                &footprints,
                at(WEEK + 1)
            ),
            OnBehalf::Wait {
                until: at(WEEK + 10)
            },
            "the edit check decides the file as a path of billing, so the idle rule keeps billing"
        );
    }

    #[test]
    fn an_idle_candidate_in_a_repository_with_no_footprint_is_idle() {
        let claims = a_week_on(&["api:billing"]);
        let web = RepoId::parse("web").unwrap();
        let changed = paths(&["src/billing/invoice.ts"]);
        let footprints = [FootprintPaths {
            repo: &web,
            paths: &changed,
            truncated: false,
        }];
        assert_eq!(
            decide(
                &bobs_request("api:billing"),
                at(WEEK),
                &claims,
                &footprints,
                at(WEEK + 1)
            ),
            granted(GrantDecision::Idle)
        );
    }

    #[test]
    fn the_first_reason_that_holds_names_the_grant() {
        let untouched = paths(&["README.md"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &untouched,
            truncated: false,
        }];
        let request = bobs_request("api:billing");
        let mut reviewing = a_week_on(&["api:billing"]);
        reviewing[0] = in_review(reviewing[0].clone());
        assert_eq!(
            decide(&request, at(WEEK), &reviewing, &footprints, at(WEEK + 1)),
            granted(GrantDecision::InReview),
            "in review wins over idle and untouched"
        );
        let idle = a_week_on(&["api:billing"]);
        assert_eq!(
            decide(&request, at(WEEK), &idle, &footprints, at(WEEK + 1)),
            granted(GrantDecision::Idle),
            "idle wins over untouched"
        );
        let recent = both(&["api:billing"]);
        assert_eq!(
            decide(&request, at(1), &recent, &footprints, at(30)),
            granted(GrantDecision::Untouched),
            "untouched wins over having waited"
        );
    }

    #[test]
    fn judging_an_area_alone_leaves_the_wait_to_the_caller() {
        let settings = two_repo_settings();
        let holder = claim(1, ALICE, &["api:billing"], 0);
        assert_eq!(
            judge_area(&holder, &area("api:billing"), &[], &settings, at(60)),
            None
        );
        let changed = paths(&["README.md"]);
        let footprints = [FootprintPaths {
            repo: &api(),
            paths: &changed,
            truncated: false,
        }];
        assert_eq!(
            judge_area(
                &holder,
                &area("api:billing"),
                &footprints,
                &settings,
                at(60)
            ),
            Some(GrantDecision::Untouched)
        );
        let undeclared = QualifiedArea::parse("api:payments").unwrap();
        assert_eq!(
            judge_area(&holder, &undeclared, &footprints, &settings, at(60)),
            None,
            "an area the settings do not declare has no globs to judge"
        );
    }

    /// The revoke of Alice's grant to Bob in billing, recorded between their claims as they are
    /// claimed at the fixed instant.
    fn revoked_in(area_text: &str) -> GrantBlock {
        GrantBlock {
            asker: BOB,
            asker_item: ItemId(2),
            asker_claimed_at: at(0),
            holder: ALICE,
            holder_item: ItemId(1),
            holder_claimed_at: at(0),
            area: area(area_text),
            revoked_at: at(1),
        }
    }

    fn decide_blocked(claims: &[Claim], block: &GrantBlock) -> OnBehalf {
        let blocks = std::slice::from_ref(block);
        let facts = GrantFacts {
            grants: &[],
            claims,
            footprints: &[],
            blocks,
        };
        decide_on_holders_behalf(
            &bobs_request("api:billing"),
            at(2),
            &facts,
            &two_repo_settings(),
            at(30),
        )
    }

    #[test]
    fn a_revoke_between_the_same_claims_leaves_the_request_to_the_holder() {
        let reviewing = [
            in_review(claim(1, ALICE, &["api:billing"], 0)),
            claim(2, BOB, &[], 0),
        ];
        assert_eq!(
            decide_blocked(&reviewing, &revoked_in("api:billing")),
            OnBehalf::LeftToHolder,
            "even a claim in review"
        );
        assert_eq!(
            decide_blocked(&both(&["api:billing"]), &revoked_in("api:billing")),
            OnBehalf::LeftToHolder,
            "even past the grant delay"
        );
    }

    #[test]
    fn a_revoke_keeps_applying_when_the_holder_renews_the_claim() {
        let mut claims = both(&["api:billing"]);
        claims[0].renewed_at = at(20);
        claims[0].last_seen = at(20);
        assert_eq!(
            decide_blocked(&claims, &revoked_in("api:billing")),
            OnBehalf::LeftToHolder
        );
    }

    #[test]
    fn a_revoke_does_not_apply_once_the_holder_claimed_again() {
        let claims = [claim(1, ALICE, &["api:billing"], 1), claim(2, BOB, &[], 0)];
        assert_eq!(
            decide_blocked(&claims, &revoked_in("api:billing")),
            granted(GrantDecision::Waited)
        );
    }

    #[test]
    fn a_revoke_in_another_area_does_not_apply() {
        assert_eq!(
            decide_blocked(&both(&["api:billing"]), &revoked_in("api:search")),
            granted(GrantDecision::Waited)
        );
    }

    #[test]
    fn a_revoke_applies_only_between_its_own_two_members_and_serves_a_wide_area_alike() {
        let block = revoked_in("api:billing");
        let claims = both(&["api:billing"]);
        let billing = area("api:billing");
        assert!(block.applies(BOB, ALICE, &billing, &claims));
        assert!(!block.applies(CAROL, ALICE, &billing, &claims));
        assert!(!block.applies(BOB, CAROL, &billing, &claims));
        assert!(
            !block.applies(BOB, ALICE, &billing, &claims[..1]),
            "the asker's claim ended"
        );
        let mut claimed_again = claims.clone();
        claimed_again[1].claimed_at = at(5);
        assert!(
            !block.applies(BOB, ALICE, &billing, &claimed_again),
            "the asker claimed the item again"
        );
    }

    #[test]
    fn a_revoke_is_outlived_once_none_recorded_between_the_members_in_the_area_applies() {
        let block = revoked_in("api:billing");
        let blocks = std::slice::from_ref(&block);
        let billing = |area: &QualifiedArea| *area == self::area("api:billing");
        let standing = both(&["api:billing"]);
        assert!(
            !revoke_outlived(blocks, BOB, ALICE, billing, &standing),
            "it still applies"
        );
        let elsewhere = [claim(3, ALICE, &["api:billing"], 1), claim(2, BOB, &[], 0)];
        assert!(
            revoke_outlived(blocks, BOB, ALICE, billing, &elsewhere),
            "the holder holds the area through another claim"
        );
        assert!(
            !revoke_outlived(&[], BOB, ALICE, billing, &elsewhere),
            "with no revoke recorded, none was outlived"
        );
        assert!(
            !revoke_outlived(blocks, CAROL, ALICE, billing, &elsewhere),
            "a revoke recorded for other members does not count"
        );
        assert!(
            !revoke_outlived(
                blocks,
                BOB,
                ALICE,
                |area: &QualifiedArea| *area == self::area("api:search"),
                &elsewhere
            ),
            "a revoke recorded in another area does not count"
        );
        let mut again = block.clone();
        again.holder_item = ItemId(3);
        again.holder_claimed_at = at(1);
        assert!(
            !revoke_outlived(&[block, again], BOB, ALICE, billing, &elsewhere),
            "one recorded between the new claims still applies"
        );
    }
}
