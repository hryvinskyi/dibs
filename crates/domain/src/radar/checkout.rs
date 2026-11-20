//! Which claim a session's checkout may record its branch on.
//!
//! Several sessions of one member can share one checkout whose branch changes many times a day,
//! and one session can work in a checkout and, through its agents, in another. A checkout's branch
//! says where a claim's work is only when nothing else can be working there: the session works on
//! one claim, reports no other checkout, and no other session of the member reports this one.

use time::Duration;

use crate::claim::Claim;
use crate::session::SessionCheckout;

/// The claim a session's checkout could feed its branch to before the other sessions' checkouts
/// are known: the one live claim, among `claims`, that the session of `reported` took or last
/// renewed and works on rather than waits in review on. `None` when the session works on no such
/// claim or on more than one; a claim of the session in review neither counts nor is fed.
pub fn checkout_candidate<'a>(
    claims: &'a [Claim],
    reported: &SessionCheckout,
    stale_after: Duration,
) -> Option<&'a Claim> {
    let mut working = claims.iter().filter(|claim| {
        claim.member == reported.member
            && claim.session == reported.session
            && claim.is_working()
            && claim.is_live(reported.reported_at, stale_after)
    });
    let only = working.next()?;
    working.next().is_none().then_some(only)
}

/// The claim the checkout of `reported` records its branch on, if any: the session's one live
/// working claim ([`checkout_candidate`]), provided that, among `checkouts`, the session reported
/// no other checkout and no other session of the same member reported this one. Only reports made
/// within `stale_after` of the time of `reported` count; another member's sessions never do, since
/// a checkout's token tells checkouts apart only within one member.
pub fn checkout_claim<'a>(
    claims: &'a [Claim],
    reported: &SessionCheckout,
    checkouts: &[SessionCheckout],
    stale_after: Duration,
) -> Option<&'a Claim> {
    let claim = checkout_candidate(claims, reported, stale_after)?;
    let since = reported.reported_at - stale_after;
    let shared = checkouts
        .iter()
        .filter(|other| other.member == reported.member && other.reported_at >= since)
        .any(|other| {
            let same_session = other.session == reported.session;
            let same_checkout = other.checkout == reported.checkout;
            same_session != same_checkout
        });
    (!shared).then_some(claim)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::ids::{MemberId, SessionId};
    use crate::review::{ClaimState, Review};
    use crate::session::CheckoutToken;
    use crate::testing::{ALICE, BOB, at, claim};

    const STALE: Duration = Duration::hours(24);

    fn token(digit: char) -> CheckoutToken {
        CheckoutToken::parse(&digit.to_string().repeat(32)).unwrap()
    }

    /// Alice's claim on `item`, taken in `session` and last seen at minute 2000.
    fn taken(item: u64, session: u64) -> Claim {
        let mut made = claim(item, ALICE, &[], 2000);
        made.session = SessionId(session);
        made
    }

    fn in_review(mut made: Claim) -> Claim {
        let prs = BTreeSet::from([crate::review::PrRef::parse("acme/api#1").unwrap()]);
        made.state = ClaimState::InReview(Review::new(prs, at(0), false).unwrap());
        made
    }

    /// That `session` of `member` reported the checkout `digit` at `minutes`.
    fn report(session: u64, member: MemberId, digit: char, minutes: i64) -> SessionCheckout {
        SessionCheckout {
            session: SessionId(session),
            member,
            checkout: token(digit),
            reported_at: at(minutes),
        }
    }

    fn fed(
        claims: &[Claim],
        reported: &SessionCheckout,
        checkouts: &[SessionCheckout],
    ) -> Option<u64> {
        checkout_claim(claims, reported, checkouts, STALE).map(|claim| claim.item.0)
    }

    #[test]
    fn a_session_alone_in_its_checkout_with_one_working_claim_feeds_it() {
        let claims = [taken(1, 7), taken(2, 8)];
        let now = report(7, ALICE, 'a', 10);
        assert_eq!(fed(&claims, &now, std::slice::from_ref(&now)), Some(1));
        assert_eq!(
            fed(&claims, &now, &[]),
            Some(1),
            "its own report may be missing"
        );
    }

    #[test]
    fn two_working_claims_feed_neither_and_a_claim_in_review_does_not_count() {
        let now = report(7, ALICE, 'a', 10);
        let two = [taken(1, 7), taken(2, 7)];
        assert_eq!(fed(&two, &now, std::slice::from_ref(&now)), None);
        assert_eq!(checkout_candidate(&two, &now, STALE), None);
        let one_reviewed = [taken(1, 7), in_review(taken(2, 7))];
        assert_eq!(
            fed(&one_reviewed, &now, std::slice::from_ref(&now)),
            Some(1)
        );
        let only_reviewed = [in_review(taken(2, 7))];
        assert_eq!(fed(&only_reviewed, &now, std::slice::from_ref(&now)), None);
    }

    #[test]
    fn a_stale_claim_or_another_members_claim_is_not_the_sessions_working_claim() {
        let now = report(7, ALICE, 'a', 2000);
        let later = SessionCheckout {
            reported_at: at(2000) + STALE + Duration::minutes(1),
            ..now.clone()
        };
        let claims = [taken(1, 7)];
        assert_eq!(fed(&claims, &later, &[]), None, "gone stale");
        let mut bobs = taken(2, 7);
        bobs.member = BOB;
        assert_eq!(fed(&[bobs], &now, &[]), None);
    }

    #[test]
    fn another_session_of_the_member_in_the_same_checkout_blocks_it() {
        let claims = [taken(1, 7), taken(2, 8)];
        let now = report(7, ALICE, 'a', 10);
        let other = report(8, ALICE, 'a', 5);
        assert_eq!(fed(&claims, &now, &[now.clone(), other]), None);
    }

    #[test]
    fn the_session_reporting_another_checkout_too_blocks_it() {
        let claims = [taken(1, 7)];
        let now = report(7, ALICE, 'a', 10);
        let its_agents = report(7, ALICE, 'b', 9);
        assert_eq!(fed(&claims, &now, &[now.clone(), its_agents]), None);
    }

    #[test]
    fn a_report_older_than_stale_after_blocks_nothing() {
        let claims = [taken(1, 7)];
        let now = report(7, ALICE, 'a', 2000);
        let long_ago = 2000 - STALE.whole_minutes() - 1;
        let old_sharer = report(8, ALICE, 'a', long_ago);
        let old_elsewhere = report(7, ALICE, 'b', long_ago);
        assert_eq!(fed(&claims, &now, &[old_sharer, old_elsewhere]), Some(1));
        let edge = report(8, ALICE, 'a', 2000 - STALE.whole_minutes());
        assert_eq!(
            fed(&claims, &now, &[edge]),
            None,
            "exactly stale_after ago still counts"
        );
    }

    #[test]
    fn another_members_session_on_the_same_token_blocks_nothing() {
        let claims = [taken(1, 7)];
        let now = report(7, ALICE, 'a', 10);
        let bobs = report(9, BOB, 'a', 10);
        assert_eq!(fed(&claims, &now, &[now.clone(), bobs]), Some(1));
    }
}
