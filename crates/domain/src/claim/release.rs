//! Which claims the server releases because their work stopped showing.

use time::{Duration, OffsetDateTime};

use super::Claim;

impl Claim {
    /// Whether the server releases the claim at `now`: its holder works on it, and its last sign
    /// of its own work is more than `release_after` before `now`. A claim in review is never due,
    /// since the pull requests it waits for are its sign of life; a claim seen in the future
    /// (another machine's clock running ahead) is not due either.
    pub fn is_due_for_release(&self, now: OffsetDateTime, release_after: Duration) -> bool {
        self.is_working() && now - self.last_seen > release_after
    }
}

/// The claims due for release at `now` ([`Claim::is_due_for_release`]), in the order given.
pub fn due_for_release(
    claims: &[Claim],
    now: OffsetDateTime,
    release_after: Duration,
) -> Vec<&Claim> {
    claims
        .iter()
        .filter(|claim| claim.is_due_for_release(now, release_after))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::{ClaimState, PrRef, Review};
    use crate::testing::{ALICE, BOB, at, claim};

    const WEEK: Duration = Duration::days(7);

    fn in_review(item: u64) -> Claim {
        let prs = [PrRef::parse("acme/widgets#42").unwrap()].into();
        Claim {
            state: ClaimState::InReview(Review::new(prs, at(0), false).unwrap()),
            ..claim(item, BOB, &["api:billing"], 0)
        }
    }

    #[test]
    fn a_working_claim_is_due_once_it_went_longer_than_the_delay_without_a_sign_of_its_work() {
        let unseen = claim(1, ALICE, &["api:billing"], 0);
        assert!(!unseen.is_due_for_release(at(0) + WEEK - Duration::minutes(1), WEEK));
        assert!(
            !unseen.is_due_for_release(at(0) + WEEK, WEEK),
            "exactly the delay is not more than it"
        );
        assert!(unseen.is_due_for_release(at(0) + WEEK + Duration::minutes(1), WEEK));
    }

    #[test]
    fn a_claim_in_review_is_never_due() {
        let reviewing = in_review(2);
        assert!(!reviewing.is_due_for_release(at(0) + Duration::days(365), WEEK));
    }

    #[test]
    fn a_claim_seen_in_the_future_is_not_due() {
        let ahead = claim(3, ALICE, &[], 60);
        assert!(!ahead.is_due_for_release(at(0), WEEK));
        assert!(!ahead.is_due_for_release(at(0), Duration::days(1)));
    }

    #[test]
    fn only_the_due_claims_are_answered_in_the_order_given() {
        let now = at(0) + WEEK + Duration::minutes(1);
        let claims = [
            claim(1, ALICE, &[], 0),
            in_review(2),
            claim(3, BOB, &[], 0),
            claim(4, BOB, &[], 1),
            claim(5, ALICE, &[], 2),
        ];
        let due: Vec<u64> = due_for_release(&claims, now, WEEK)
            .into_iter()
            .map(|claim| claim.item.get())
            .collect();
        assert_eq!(due, [1, 3], "a claim seen within the delay is not due");
    }
}
