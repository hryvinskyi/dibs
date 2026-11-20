//! The checkout a member's request comes from: storing that a session reported it, and finding the
//! one claim whose branch it may record.

use time::{Duration, OffsetDateTime};

use dibs_domain::claim::Claim;
use dibs_domain::ids::{MemberId, SessionId};
use dibs_domain::radar::{checkout_candidate, checkout_claim};
use dibs_domain::session::{CheckoutToken, SessionCheckout};

use crate::error::AppError;
use crate::use_cases::radar::OnBranch;
use crate::work::Work;

/// The checkout a member's request comes from: the token its client names it by and, when it is
/// on a branch of a repository, that repository and branch. A request that names no checkout
/// records no branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FromCheckout {
    /// The checkout.
    pub token: CheckoutToken,
    /// Its repository and the branch it is on; none on a detached head.
    pub branch: Option<OnBranch>,
}

/// Stores that `session` of `member` reported working in the checkout `token` at `now`, whether or
/// not the checkout is on a branch, and answers the report.
///
/// # Errors
/// [`AppError::Store`] when the store fails.
pub(crate) async fn report_checkout(
    work: &mut Work,
    member: MemberId,
    session: SessionId,
    token: &CheckoutToken,
    now: OffsetDateTime,
) -> Result<SessionCheckout, AppError> {
    let reported = SessionCheckout {
        session,
        member,
        checkout: token.clone(),
        reported_at: now,
    };
    let project = work.project_id();
    work.save_session_checkout(project, &reported).await?;
    Ok(reported)
}

/// The claim, among `claims`, that the checkout of `reported` records its branch on: the session's
/// one live working claim, when `eligible` accepts it, the session reported no other checkout and
/// no other session of the member reported this one, all within `stale_after`. The member's
/// reported checkouts are read only once a claim is eligible.
///
/// # Errors
/// [`AppError::Store`] when the store fails.
pub(crate) async fn fed_by_checkout<'a>(
    work: &mut Work,
    reported: &SessionCheckout,
    claims: &'a [Claim],
    stale_after: Duration,
    eligible: impl Fn(&Claim) -> bool,
) -> Result<Option<&'a Claim>, AppError> {
    let candidate = checkout_candidate(claims, reported, stale_after);
    if !candidate.is_some_and(eligible) {
        return Ok(None);
    }
    let project = work.project_id();
    let since = reported.reported_at - stale_after;
    let checkouts = work
        .session_checkouts(project, reported.member, since)
        .await?;
    Ok(checkout_claim(claims, reported, &checkouts, stale_after))
}
