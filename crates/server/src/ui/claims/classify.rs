//! How a claim stands, read from the claim and its item alone: whether its ticket is finished,
//! whether it went silent, whether it has been held over a week, and whether a branch it recorded is
//! named after another ticket. Each is a pure function over the domain's values, so a page can count
//! claims by them without building their views.

use time::{Duration, OffsetDateTime};

use dibs_app::records::ItemRecord;
use dibs_domain::claim::Claim;
use dibs_domain::item::ItemStatus;
use dibs_domain::repo::RepoId;

/// How long a claim may be held before it counts as held over a week.
const A_WEEK: Duration = Duration::days(7);

/// Whether the item's ticket is finished: closed in its tracker, or done in dibs. No status name of
/// a tracker counts.
pub fn finished(item: &ItemRecord) -> bool {
    item.closed || item.status == ItemStatus::Done
}

/// Whether the claim went silent: it is not in review and was last seen longer than `stale_after`
/// before `now`.
pub fn silent(claim: &Claim, now: OffsetDateTime, stale_after: Duration) -> bool {
    !claim.is_live(now, stale_after)
}

/// Whether a live claim its holder works on, whose ticket is not finished, was claimed more than
/// seven days before `now`.
pub fn held_over_a_week(
    claim: &Claim,
    item: &ItemRecord,
    now: OffsetDateTime,
    stale_after: Duration,
) -> bool {
    claim.is_working()
        && !silent(claim, now, stale_after)
        && !finished(item)
        && now - claim.claimed_at > A_WEEK
}

/// The first branch the claim recorded that is named after another ticket than its item's, with
/// its repository and that ticket's number: its last path segment (after the last `/`) starts with
/// a number that ends the segment or is followed by `-`, `_` or `.` (`alice/42-fix`, `42`), and the
/// number differs from the one its item's reference ends in (`#N`). A segment that does not start
/// with a digit names no ticket (`main`, `release-2026`, `foo-1.2.3`), and an item whose reference
/// ends in no number never counts.
pub fn other_ticket<'a>(claim: &'a Claim, item: &ItemRecord) -> Option<(&'a RepoId, u64)> {
    let own = item
        .external_ref
        .as_ref()
        .and_then(|external| trailing_number(external.value()))?;
    claim.branch_names().find_map(|(repo, branch)| {
        let segment = branch.as_str().rsplit('/').next().unwrap_or_default();
        let named = leading_number(segment)?;
        (named != own).then_some((repo, named))
    })
}

/// The number a text ends in after its last `#`.
fn trailing_number(text: &str) -> Option<u64> {
    let (_, number) = text.rsplit_once('#')?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

/// The number a branch's path segment starts with, when the digits end the segment or are followed
/// by `-`, `_` or `.`.
fn leading_number(segment: &str) -> Option<u64> {
    let end = segment
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(segment.len());
    let digits = segment.get(..end)?;
    let rest = segment.get(end..)?;
    let ends = rest.is_empty() || rest.starts_with(['-', '_', '.']);
    if digits.is_empty() || !ends {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use time::macros::datetime;

    use dibs_domain::handover::ClaimKind;
    use dibs_domain::ids::{ItemId, MemberId, SessionId};
    use dibs_domain::item::{ExternalRef, ItemRepository, Title};
    use dibs_domain::radar::{BranchName, BranchSource, RecordedBranch};
    use dibs_domain::review::{ClaimState, PrRef, Review};
    use dibs_domain::session::HostName;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-03-10 12:00:00 UTC);

    fn stale_after() -> Duration {
        Duration::hours(24)
    }

    fn item(reference: Option<&str>) -> ItemRecord {
        ItemRecord::new(
            ItemId(42),
            reference.map(|text| ExternalRef::parse(text).unwrap()),
            Title::parse("Rates").unwrap(),
            ItemRepository::Unnamed,
            MemberId(1),
            NOW - Duration::days(30),
        )
    }

    /// A working claim made `held` before now and seen `seen` before now.
    fn claim(held: Duration, seen: Duration) -> Claim {
        Claim {
            item: ItemId(42),
            member: MemberId(1),
            session: SessionId(1),
            host: HostName::parse("laptop").unwrap(),
            areas: BTreeSet::new(),
            wide: None,
            claimed_at: NOW - held,
            renewed_at: NOW - held,
            last_seen: NOW - seen,
            state: ClaimState::Working,
            branches: BTreeMap::new(),
            kind: ClaimKind::Feature,
            pending: BTreeMap::new(),
        }
    }

    fn on_branches(names: &[&str]) -> Claim {
        let mut claim = claim(Duration::hours(1), Duration::minutes(1));
        let repos = ["api", "web", "docs"];
        claim.branches = names
            .iter()
            .zip(repos)
            .map(|(name, repo)| {
                (
                    RepoId::parse(repo).unwrap(),
                    RecordedBranch::new(BranchName::parse(name).unwrap(), BranchSource::Checkout),
                )
            })
            .collect();
        claim
    }

    fn in_review(mut claim: Claim) -> Claim {
        let prs = BTreeSet::from([PrRef::parse("acme/widgets#12").unwrap()]);
        claim.state = ClaimState::InReview(Review::new(prs, NOW, false).unwrap());
        claim
    }

    #[test]
    fn a_ticket_is_finished_when_closed_in_its_tracker_or_done_here() {
        let open = item(Some("gh:acme/widgets#42"));
        assert!(!finished(&open));
        let mut closed = open.clone();
        closed.closed = true;
        assert!(finished(&closed));
        let mut done = open.clone();
        done.status = ItemStatus::Done;
        assert!(finished(&done));
        let mut claimed = open;
        claimed.status = ItemStatus::Claimed;
        assert!(!finished(&claimed));
    }

    #[test]
    fn a_claim_goes_silent_once_unseen_for_longer_than_stale_after_unless_in_review() {
        let edge = claim(Duration::days(2), stale_after());
        assert!(!silent(&edge, NOW, stale_after()));
        let past = claim(Duration::days(2), stale_after() + Duration::seconds(1));
        assert!(silent(&past, NOW, stale_after()));
        assert!(!silent(&in_review(past), NOW, stale_after()));
    }

    #[test]
    fn a_claim_is_held_over_a_week_only_after_seven_days_exactly() {
        let ticket = item(Some("gh:acme/widgets#42"));
        let seen = Duration::minutes(5);
        let week = claim(Duration::days(7), seen);
        assert!(!held_over_a_week(&week, &ticket, NOW, stale_after()));
        let over = claim(Duration::days(7) + Duration::seconds(1), seen);
        assert!(held_over_a_week(&over, &ticket, NOW, stale_after()));
    }

    #[test]
    fn a_claim_in_review_silent_or_on_a_finished_ticket_is_never_held_over_a_week() {
        let ticket = item(Some("gh:acme/widgets#42"));
        let long = Duration::days(30);
        let reviewed = in_review(claim(long, Duration::days(29)));
        assert!(!held_over_a_week(&reviewed, &ticket, NOW, stale_after()));
        let quiet = claim(long, Duration::days(2));
        assert!(!held_over_a_week(&quiet, &ticket, NOW, stale_after()));
        let mut closed = ticket.clone();
        closed.closed = true;
        let working = claim(long, Duration::minutes(5));
        assert!(!held_over_a_week(&working, &closed, NOW, stale_after()));
        let mut done = ticket;
        done.status = ItemStatus::Done;
        assert!(!held_over_a_week(&working, &done, NOW, stale_after()));
    }

    #[test]
    fn a_branch_named_after_another_number_points_at_another_ticket() {
        let fix = on_branches(&["alice/42-fix"]);
        assert_eq!(other_ticket(&fix, &item(Some("gh:acme/widgets#42"))), None);
        let seven = item(Some("gh:acme/widgets#7"));
        let (repo, number) = other_ticket(&fix, &seven).unwrap();
        assert_eq!((repo.as_str(), number), ("api", 42));
        assert_eq!(other_ticket(&on_branches(&["main"]), &seven), None);
        for named in ["42", "bob/42_fix", "bob/42.fix"] {
            assert_eq!(
                other_ticket(&on_branches(&[named]), &seven).map(|(_, n)| n),
                Some(42),
                "{named}"
            );
        }
        for unnamed in [
            "release-2026",
            "bob/fix-42",
            "deps/npm/widget-1.2.3",
            "bob/42fix",
        ] {
            assert_eq!(
                other_ticket(&on_branches(&[unnamed]), &seven),
                None,
                "{unnamed}"
            );
        }
        assert_eq!(
            other_ticket(&on_branches(&["main", "bob/77-other"]), &seven)
                .map(|(repo, n)| (repo.as_str(), n)),
            Some(("web", 77))
        );
    }

    #[test]
    fn only_the_last_segment_counts_and_a_reference_without_a_number_never_does() {
        let seven = item(Some("gh:acme/widgets#7"));
        assert_eq!(other_ticket(&on_branches(&["team-9/fix"]), &seven), None);
        assert_eq!(other_ticket(&on_branches(&["007-fix"]), &seven), None);
        let fix = on_branches(&["alice/42-fix"]);
        assert_eq!(other_ticket(&fix, &item(Some("jira:ABC-12"))), None);
        assert_eq!(other_ticket(&fix, &item(None)), None);
    }
}
