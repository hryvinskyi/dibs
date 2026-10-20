//! Review: a claim whose work is in pull requests, and how a merge or a close of one of them moves
//! it on.
//!
//! A claim moves to review when its holder records a pull request on it. It keeps holding its areas
//! while it is in review, so nobody edits the files of a pull request under review, and it never goes
//! stale: the open pull request is its sign of life. A pull request leaves the claim only through a
//! report that it was merged or closed, matched against the pull requests the claim recorded — never
//! by reading a ticket number out of a branch or a description. When the last one is merged the claim
//! is complete, unless its holder said more pull requests of the same item follow; when the last one
//! is closed unmerged the claim is worked on again.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::claim::{Claim, HolderOnly};
use crate::ids::{ItemId, MemberId};
use crate::settings::ProjectSettings;
use crate::text::string_value;

/// A pull request of a forge repository, `<owner>/<name>#<number>`, in one canonical form: the
/// repository's path lowercased (a namespace of several segments kept whole) and the number without
/// leading zeros, so every spelling of one pull request compares equal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PrRef(String);

/// Why a text is not a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PrRefError {
    /// The text is not `<owner>/<name>#<number>`.
    #[error("a pull request is written '<owner>/<name>#<number>', such as 'acme/widgets#42'")]
    Malformed,
}

impl PrRef {
    /// The longest pull request, in bytes.
    pub const MAX_BYTES: usize = 200;

    /// Parses `<owner>/<name>#<number>`, trimming whitespace at both ends. The path has at least two
    /// segments of letters, digits, `.`, `_` and `-`; the number is a positive integer.
    ///
    /// # Errors
    /// [`PrRefError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, PrRefError> {
        let value = value.trim();
        if value.len() > Self::MAX_BYTES {
            return Err(PrRefError::Malformed);
        }
        let (path, number) = value.rsplit_once('#').ok_or(PrRefError::Malformed)?;
        let digits_only = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
        let number: u64 = if digits_only {
            number.parse().map_err(|_| PrRefError::Malformed)?
        } else {
            return Err(PrRefError::Malformed);
        };
        if number == 0 {
            return Err(PrRefError::Malformed);
        }
        let path = path.to_ascii_lowercase();
        let segment_ok = |segment: &&str| {
            !segment.is_empty()
                && *segment != "."
                && *segment != ".."
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        };
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() < 2 || !segments.iter().all(segment_ok) {
            return Err(PrRefError::Malformed);
        }
        Ok(Self(format!("{path}#{number}")))
    }

    /// The repository's path, `<owner>/<name>`.
    pub fn repo_path(&self) -> &str {
        self.0
            .rsplit_once('#')
            .map(|(path, _)| path)
            .unwrap_or_default()
    }

    /// The pull request's number.
    pub fn number(&self) -> u64 {
        self.0
            .rsplit_once('#')
            .and_then(|(_, number)| number.parse().ok())
            .unwrap_or_default()
    }
}

string_value!(PrRef, PrRefError);

/// A claim's review: the pull requests it recorded, never none, since when it is in review, and
/// whether more pull requests of the same item follow once these are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    prs: BTreeSet<PrRef>,
    since: OffsetDateTime,
    more: bool,
}

/// Why a review cannot be made.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReviewError {
    /// It records no pull request.
    #[error("a claim in review records at least one pull request")]
    NoPullRequests,
}

impl Review {
    /// A review of these pull requests, since `since`.
    ///
    /// # Errors
    /// [`ReviewError::NoPullRequests`] when `prs` is empty.
    pub fn new(
        prs: BTreeSet<PrRef>,
        since: OffsetDateTime,
        more: bool,
    ) -> Result<Self, ReviewError> {
        if prs.is_empty() {
            return Err(ReviewError::NoPullRequests);
        }
        Ok(Self { prs, since, more })
    }

    /// The pull requests recorded, in order.
    pub fn prs(&self) -> &BTreeSet<PrRef> {
        &self.prs
    }

    /// Since when the claim is in review.
    pub fn since(&self) -> OffsetDateTime {
        self.since
    }

    /// Whether more pull requests of the same item follow these.
    pub fn more(&self) -> bool {
        self.more
    }
}

/// Where a claim's work stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ClaimState {
    /// Its holder works on it, and heartbeats keep it live.
    #[default]
    Working,
    /// Its work waits in pull requests; it is live while they are open.
    InReview(Review),
}

impl ClaimState {
    /// The review, while the claim is in one.
    pub fn review(&self) -> Option<&Review> {
        match self {
            Self::Working => None,
            Self::InReview(review) => Some(review),
        }
    }

    /// Whether the claim is in review.
    pub fn is_in_review(&self) -> bool {
        self.review().is_some()
    }

    /// Whether the claim's review recorded the pull request.
    pub fn records(&self, pr: &PrRef) -> bool {
        self.review().is_some_and(|review| review.prs.contains(pr))
    }

    /// The stable name: `working` or `in_review`.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::InReview(_) => "in_review",
        }
    }
}

/// A holder's request to record a pull request on their claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    /// The item of the claim.
    pub item: ItemId,
    /// The member asking.
    pub by: MemberId,
    /// The pull request.
    pub pr: PrRef,
    /// Whether more pull requests of the item follow this one.
    pub more: bool,
}

/// What recording a pull request did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewStarted {
    /// The claim to store, in review.
    pub claim: Claim,
    /// Whether the pull request is new to the claim; recording it again changes at most whether
    /// more follow.
    pub added: bool,
    /// Whether anything changed at all.
    pub changed: bool,
}

/// Why a pull request cannot be recorded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReviewRefused {
    /// Nobody, or another member, holds the item.
    #[error(transparent)]
    HolderOnly(#[from] HolderOnly),
    /// The holder's claim is stale; they claim the item again first.
    #[error("your claim on this item is stale; claim it again before asking for review")]
    Stale,
    /// Another claim recorded the pull request.
    #[error("the claim on work item #{item} recorded this pull request already")]
    RecordedElsewhere {
        /// The item of that claim.
        item: ItemId,
        /// Its holder.
        member: MemberId,
    },
    /// The pull request belongs to no repository the project lists.
    #[error("the pull request {pr} is in no repository this project lists")]
    UnknownRepository {
        /// The pull request.
        pr: PrRef,
    },
}

/// Decides recording a pull request on a claim. Only the holder records one, on their live claim,
/// of a repository the project lists, and a pull request belongs to one claim: one another claim
/// recorded is refused. The claim moves
/// to review, keeping its areas and the time it went into review when it was in review already;
/// whether more pull requests follow is what the latest request says. Asking is a sign of life.
///
/// # Errors
/// [`ReviewRefused`] naming why not.
pub fn decide_review(
    current: Option<&Claim>,
    others: &[Claim],
    request: &ReviewRequest,
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> Result<ReviewStarted, ReviewRefused> {
    let claim = current.ok_or(HolderOnly::NoClaim)?;
    if claim.member != request.by {
        return Err(HolderOnly::NotHolder {
            member: claim.member,
        }
        .into());
    }
    if !claim.is_live(now, settings.stale_after()) {
        return Err(ReviewRefused::Stale);
    }
    if settings.repo_of_pr(&request.pr).is_none() {
        return Err(ReviewRefused::UnknownRepository {
            pr: request.pr.clone(),
        });
    }
    if let Some(other) = others
        .iter()
        .find(|other| other.item != claim.item && other.state.records(&request.pr))
    {
        return Err(ReviewRefused::RecordedElsewhere {
            item: other.item,
            member: other.member,
        });
    }
    let (mut prs, since) = match &claim.state {
        ClaimState::Working => (BTreeSet::new(), now),
        ClaimState::InReview(review) => (review.prs.clone(), review.since),
    };
    let added = prs.insert(request.pr.clone());
    let review = Review {
        prs,
        since,
        more: request.more,
    };
    let state = ClaimState::InReview(review);
    let changed = state != claim.state;
    Ok(ReviewStarted {
        claim: Claim {
            state,
            last_seen: now,
            ..claim.clone()
        },
        added,
        changed,
    })
}

/// What a pull request leaving a claim did to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrSettled {
    /// It was merged and other recorded pull requests are still open; the claim stays in review
    /// without it.
    StillInReview {
        /// The claim to store.
        claim: Claim,
    },
    /// It left without delivering the work — closed unmerged, or merged into a branch that is not
    /// protected — while other recorded pull requests are still open. The claim stays in review
    /// without it, and from now on more pull requests follow: merging the others no longer
    /// completes it, since the work of this one is not in. Its holder is told.
    SetAside {
        /// The claim to store.
        claim: Claim,
    },
    /// Its last pull request left and the work goes on: the claim is worked on again, keeping its
    /// areas, and counts as seen now — a claim that sat in review for days is not stale on arrival.
    Working {
        /// The claim to store.
        claim: Claim,
    },
    /// Its last pull request was merged into a branch that counts and no more follow: the claim is
    /// complete.
    Completed {
        /// The claim that ends.
        claim: Claim,
    },
}

impl PrSettled {
    /// The claim the pull request left.
    pub fn claim(&self) -> &Claim {
        match self {
            Self::StillInReview { claim }
            | Self::SetAside { claim }
            | Self::Working { claim }
            | Self::Completed { claim } => claim,
        }
    }
}

/// How a pull request left the claim that recorded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrEnd {
    /// It was merged into a branch that delivers the work (see [`merge_counts`]).
    Merged,
    /// It was merged into another branch, such as the branch of a pull request it was stacked on.
    MergedElsewhere,
    /// It was closed without being merged.
    Closed,
}

impl PrEnd {
    /// Whether the work of the pull request reached a branch that delivers it.
    pub const fn delivers(self) -> bool {
        matches!(self, Self::Merged)
    }
}

/// Whether a merge of `pr` into `base` (`main` or `refs/heads/main`) delivers its work: always when
/// its repository protects no branch, or the project lists none it belongs to; otherwise only into
/// one of its protected branches. A stacked pull request merged into a feature branch delivers
/// nothing yet.
pub fn merge_counts(settings: &ProjectSettings, pr: &PrRef, base: &str) -> bool {
    let Some(repo) = settings.repo_of_pr(pr) else {
        return true;
    };
    if repo.protected_refs.is_empty() {
        return true;
    }
    repo.branch(base)
        .is_some_and(|branch| repo.protected_refs.iter().any(|rule| rule.target == branch))
}

/// Decides what a merged or closed pull request does: it leaves the claim in review that recorded
/// it. A merge that delivers the work, with others left, keeps the claim in review; with none
/// left, it completes the claim unless more pull requests follow. A close, or a merge that
/// delivers nothing, with others left, sets the pull request aside and marks that more follow; with
/// none left — and after a delivering merge when more follow — the claim is worked on again, seen
/// at `now`. `None` when no claim in review recorded the pull request: one nobody recorded, or one
/// whose report was already applied, changes nothing.
pub fn decide_pr_end(
    claims: &[Claim],
    pr: &PrRef,
    end: PrEnd,
    now: OffsetDateTime,
) -> Option<PrSettled> {
    let claim = claims
        .iter()
        .filter(|claim| claim.state.records(pr))
        .min_by_key(|claim| claim.item)?;
    let review = claim.state.review()?;
    let mut prs = review.prs.clone();
    prs.remove(pr);
    let more = review.more || !end.delivers();
    if let Ok(left) = Review::new(prs, review.since, more) {
        let claim = Claim {
            state: ClaimState::InReview(left),
            ..claim.clone()
        };
        return Some(if end.delivers() {
            PrSettled::StillInReview { claim }
        } else {
            PrSettled::SetAside { claim }
        });
    }
    if !more {
        return Some(PrSettled::Completed {
            claim: claim.clone(),
        });
    }
    Some(PrSettled::Working {
        claim: back_at_work(claim, now),
    })
}

/// The claim worked on again, seen at `now`, its areas kept.
fn back_at_work(claim: &Claim, now: OffsetDateTime) -> Claim {
    Claim {
        state: ClaimState::Working,
        last_seen: now,
        ..claim.clone()
    }
}

/// A holder's request to remove a pull request they recorded by mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropRequest {
    /// The member asking.
    pub by: MemberId,
    /// The pull request.
    pub pr: PrRef,
}

/// Why a pull request cannot be removed from a claim.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DropRefused {
    /// Nobody, or another member, holds the item.
    #[error(transparent)]
    HolderOnly(#[from] HolderOnly),
    /// The claim did not record that pull request.
    #[error("your claim did not record the pull request {pr}")]
    NotRecorded {
        /// The pull request.
        pr: PrRef,
    },
}

/// Decides removing a pull request from the holder's claim. With others left the claim stays in
/// review as it was; with none left it is worked on again, seen at `now`.
///
/// # Errors
/// [`DropRefused`] when the caller holds no claim on the item, or it did not record the pull
/// request.
pub fn decide_drop_pr(
    current: Option<&Claim>,
    request: &DropRequest,
    now: OffsetDateTime,
) -> Result<PrSettled, DropRefused> {
    let claim = current.ok_or(HolderOnly::NoClaim)?;
    if claim.member != request.by {
        return Err(HolderOnly::NotHolder {
            member: claim.member,
        }
        .into());
    }
    let not_recorded = || DropRefused::NotRecorded {
        pr: request.pr.clone(),
    };
    let review = claim
        .state
        .review()
        .filter(|review| review.prs.contains(&request.pr))
        .ok_or_else(not_recorded)?;
    let mut prs = review.prs.clone();
    prs.remove(&request.pr);
    Ok(match Review::new(prs, review.since, review.more) {
        Ok(left) => PrSettled::StillInReview {
            claim: Claim {
                state: ClaimState::InReview(left),
                ..claim.clone()
            },
        },
        Err(_) => PrSettled::Working {
            claim: back_at_work(claim, now),
        },
    })
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::testing::{ALICE, BOB, at, claim, two_repo_settings};

    fn pr(text: &str) -> PrRef {
        PrRef::parse(text).unwrap()
    }

    fn review_request(item: u64, by: MemberId, text: &str, more: bool) -> ReviewRequest {
        ReviewRequest {
            item: ItemId(item),
            by,
            pr: pr(text),
            more,
        }
    }

    fn in_review(item: u64, member: MemberId, prs: &[&str], more: bool, minutes: i64) -> Claim {
        Claim {
            state: ClaimState::InReview(
                Review::new(prs.iter().map(|text| pr(text)).collect(), at(minutes), more).unwrap(),
            ),
            ..claim(item, member, &["api:billing"], minutes)
        }
    }

    #[test]
    fn every_spelling_of_a_pull_request_is_one() {
        for spelling in [
            "acme/widgets#42",
            "Acme/Widgets#42",
            " acme/widgets#042 ",
            "ACME/WIDGETS#42",
        ] {
            let parsed = pr(spelling);
            assert_eq!(parsed.as_str(), "acme/widgets#42", "{spelling:?}");
            assert_eq!(parsed.repo_path(), "acme/widgets");
            assert_eq!(parsed.number(), 42);
        }
        assert_eq!(pr("acme/tools/api#7").repo_path(), "acme/tools/api");
        for bad in [
            "",
            "#42",
            "widgets#42",
            "acme/widgets",
            "acme/widgets#",
            "acme/widgets#0",
            "acme/widgets#-1",
            "acme/widgets#4x",
            "acme//widgets#4",
            "acme/../widgets#4",
            "acme/wid gets#4",
            "acme/widgets#99999999999999999999999",
        ] {
            assert!(PrRef::parse(bad).is_err(), "{bad:?}");
        }
        assert!(PrRef::parse(&format!("acme/{}#1", "w".repeat(200))).is_err());
    }

    #[test]
    fn a_review_records_at_least_one_pull_request() {
        assert_eq!(
            Review::new(BTreeSet::new(), at(0), false),
            Err(ReviewError::NoPullRequests)
        );
    }

    #[test]
    fn the_holder_moves_their_live_claim_to_review_keeping_its_areas() {
        let settings = two_repo_settings();
        let working = claim(1, ALICE, &["api:billing"], 0);
        let started = decide_review(
            Some(&working),
            &[],
            &review_request(1, ALICE, "acme/api#12", false),
            at(5),
            &settings,
        )
        .unwrap();
        assert!(started.added && started.changed);
        assert_eq!(started.claim.areas, working.areas);
        assert_eq!(started.claim.last_seen, at(5));
        let review = started.claim.state.review().unwrap();
        assert_eq!(review.prs(), &BTreeSet::from([pr("acme/api#12")]));
        assert_eq!(review.since(), at(5));
        assert!(!review.more());
    }

    #[test]
    fn a_second_pull_request_joins_the_review_and_the_latest_request_says_whether_more_follow() {
        let settings = two_repo_settings();
        let first = in_review(1, ALICE, &["acme/api#12"], true, 0);
        let second = decide_review(
            Some(&first),
            &[],
            &review_request(1, ALICE, "acme/web#3", false),
            at(9),
            &settings,
        )
        .unwrap();
        let review = second.claim.state.review().unwrap();
        assert_eq!(
            review.prs(),
            &BTreeSet::from([pr("acme/api#12"), pr("acme/web#3")])
        );
        assert_eq!(review.since(), at(0), "in review since the first one");
        assert!(!review.more());
        let again = decide_review(
            Some(&second.claim),
            &[],
            &review_request(1, ALICE, "acme/web#3", false),
            at(10),
            &settings,
        )
        .unwrap();
        assert!(
            !again.added && !again.changed,
            "recording it again changes nothing"
        );
    }

    #[test]
    fn only_the_holder_of_a_live_claim_asks_and_a_pull_request_belongs_to_one_claim() {
        let settings = two_repo_settings();
        let stale_after = settings.stale_after();
        let working = claim(1, ALICE, &[], 0);
        let ask = review_request(1, ALICE, "acme/api#12", false);
        assert_eq!(
            decide_review(None, &[], &ask, at(1), &settings),
            Err(ReviewRefused::HolderOnly(HolderOnly::NoClaim))
        );
        assert_eq!(
            decide_review(
                Some(&working),
                &[],
                &review_request(1, BOB, "acme/api#12", false),
                at(1),
                &settings
            ),
            Err(ReviewRefused::HolderOnly(HolderOnly::NotHolder {
                member: ALICE
            }))
        );
        let late = at(0) + stale_after + Duration::minutes(1);
        assert_eq!(
            decide_review(Some(&working), &[], &ask, late, &settings),
            Err(ReviewRefused::Stale)
        );
        let others = [in_review(2, BOB, &["acme/api#12"], false, 0)];
        assert_eq!(
            decide_review(Some(&working), &others, &ask, at(1), &settings),
            Err(ReviewRefused::RecordedElsewhere {
                item: ItemId(2),
                member: BOB
            })
        );
        assert_eq!(
            decide_review(
                Some(&working),
                &[],
                &review_request(1, ALICE, "acme/elsewhere#1", false),
                at(1),
                &settings
            ),
            Err(ReviewRefused::UnknownRepository {
                pr: pr("acme/elsewhere#1")
            })
        );
    }

    #[test]
    fn a_claim_in_review_never_goes_stale_while_a_recorded_pull_request_is_open() {
        let settings = two_repo_settings();
        let stale_after = settings.stale_after();
        let reviewing = in_review(1, ALICE, &["acme/api#12"], false, 0);
        let months_later = at(0) + Duration::days(90);
        assert!(reviewing.is_live(months_later, stale_after));
        let working = claim(1, ALICE, &[], 0);
        assert!(!working.is_live(months_later, stale_after));
    }

    #[test]
    fn the_last_recorded_pull_request_merged_completes_the_claim() {
        let claims = [in_review(1, ALICE, &["acme/api#12"], false, 0)];
        let settled = decide_pr_end(&claims, &pr("acme/api#12"), PrEnd::Merged, at(30)).unwrap();
        assert_eq!(
            settled,
            PrSettled::Completed {
                claim: claims[0].clone()
            }
        );
    }

    #[test]
    fn a_merge_with_more_to_follow_returns_the_claim_to_work_seen_now() {
        let claims = [in_review(1, ALICE, &["acme/api#12"], true, 0)];
        let settled = decide_pr_end(&claims, &pr("acme/api#12"), PrEnd::Merged, at(30)).unwrap();
        let PrSettled::Working { claim } = settled else {
            panic!("expected the claim back at work, got {settled:?}");
        };
        assert_eq!(claim.state, ClaimState::Working);
        assert_eq!(claim.areas, claims[0].areas, "its areas are kept");
        assert_eq!(claim.last_seen, at(30));
    }

    #[test]
    fn a_claim_with_two_pull_requests_completes_only_after_both() {
        let claims = [in_review(
            1,
            ALICE,
            &["acme/api#12", "acme/web#3"],
            false,
            0,
        )];
        let first = decide_pr_end(&claims, &pr("acme/web#3"), PrEnd::Merged, at(10)).unwrap();
        let PrSettled::StillInReview { claim } = &first else {
            panic!("expected the claim still in review, got {first:?}");
        };
        assert_eq!(
            claim.state.review().map(Review::prs),
            Some(&BTreeSet::from([pr("acme/api#12")]))
        );
        let second = decide_pr_end(
            std::slice::from_ref(claim),
            &pr("acme/api#12"),
            PrEnd::Merged,
            at(20),
        )
        .unwrap();
        assert!(matches!(second, PrSettled::Completed { .. }), "{second:?}");
    }

    #[test]
    fn an_unrecorded_or_repeated_report_matches_nothing() {
        let claims = [
            in_review(1, ALICE, &["acme/api#12"], false, 0),
            claim(2, BOB, &[], 0),
        ];
        assert_eq!(
            decide_pr_end(&claims, &pr("acme/api#13"), PrEnd::Merged, at(1)),
            None
        );
        let after = [claim(1, ALICE, &[], 0)];
        assert_eq!(
            decide_pr_end(&after, &pr("acme/api#12"), PrEnd::Merged, at(1)),
            None,
            "a claim that is not in review records nothing"
        );
    }

    #[test]
    fn a_pull_request_closed_unmerged_returns_the_claim_to_work_seen_now() {
        let claims = [in_review(1, ALICE, &["acme/api#12"], false, 0)];
        let settled = decide_pr_end(&claims, &pr("acme/api#12"), PrEnd::Closed, at(60)).unwrap();
        let PrSettled::Working { claim } = settled else {
            panic!("expected the claim back at work, got {settled:?}");
        };
        assert_eq!(claim.last_seen, at(60));
        assert_eq!(claim.state, ClaimState::Working);
        let two = [in_review(
            1,
            ALICE,
            &["acme/api#12", "acme/api#14"],
            false,
            0,
        )];
        assert!(matches!(
            decide_pr_end(&two, &pr("acme/api#14"), PrEnd::Closed, at(60)),
            Some(PrSettled::SetAside { .. })
        ));
    }

    #[test]
    fn a_pull_request_closed_while_others_wait_means_their_merges_no_longer_complete_the_claim() {
        for end in [PrEnd::Closed, PrEnd::MergedElsewhere] {
            let claims = [in_review(
                1,
                ALICE,
                &["acme/api#12", "acme/web#3"],
                false,
                0,
            )];
            let settled = decide_pr_end(&claims, &pr("acme/web#3"), end, at(10)).unwrap();
            let PrSettled::SetAside { claim } = settled else {
                panic!("{end:?}: expected the pull request set aside, got {settled:?}");
            };
            let review = claim.state.review().unwrap();
            assert_eq!(review.prs(), &BTreeSet::from([pr("acme/api#12")]));
            assert!(review.more(), "{end:?}: more follow now");
            let after = decide_pr_end(
                std::slice::from_ref(&claim),
                &pr("acme/api#12"),
                PrEnd::Merged,
                at(20),
            )
            .unwrap();
            let PrSettled::Working { claim } = after else {
                panic!("{end:?}: the merge of the other completes nothing, got {after:?}");
            };
            assert_eq!(claim.last_seen, at(20));
        }
    }

    #[test]
    fn a_merge_into_a_branch_that_is_not_protected_delivers_nothing() {
        let claims = [in_review(1, ALICE, &["acme/api#12"], false, 0)];
        let settled =
            decide_pr_end(&claims, &pr("acme/api#12"), PrEnd::MergedElsewhere, at(5)).unwrap();
        assert!(
            matches!(settled, PrSettled::Working { .. }),
            "back at work, not complete: {settled:?}"
        );
        let protected = crate::testing::settings(
            r#"{"repos":[
                {"id":"api","remote":"example.com/acme/api",
                 "protected_refs":[{"ref":"refs/heads/main","pushers":["alice"]}]},
                {"id":"web","remote":"example.com/acme/web"}]}"#,
        );
        assert!(merge_counts(&protected, &pr("acme/api#12"), "main"));
        assert!(merge_counts(
            &protected,
            &pr("acme/api#12"),
            "refs/heads/main"
        ));
        assert!(!merge_counts(&protected, &pr("acme/api#12"), "bob/42-base"));
        assert!(!merge_counts(
            &protected,
            &pr("acme/api#12"),
            "refs/tags/v1"
        ));
        assert!(
            merge_counts(&protected, &pr("acme/web#3"), "anything"),
            "a repository that protects no branch counts any base"
        );
        assert!(
            merge_counts(&protected, &pr("acme/other#3"), "anything"),
            "a repository the project no longer lists counts any base"
        );
    }

    #[test]
    fn the_holder_drops_a_pull_request_recorded_by_mistake() {
        let claim = in_review(1, ALICE, &["acme/api#12", "acme/api#13"], true, 0);
        let drop = |by: MemberId, text: &str| DropRequest { by, pr: pr(text) };
        let settled = decide_drop_pr(Some(&claim), &drop(ALICE, "acme/api#13"), at(9)).unwrap();
        let PrSettled::StillInReview { claim: left } = settled else {
            panic!("expected the claim still in review, got {settled:?}");
        };
        let review = left.state.review().unwrap();
        assert_eq!(review.prs(), &BTreeSet::from([pr("acme/api#12")]));
        assert!(review.more() && review.since() == at(0));
        let last = decide_drop_pr(Some(&left), &drop(ALICE, "acme/api#12"), at(9)).unwrap();
        let PrSettled::Working { claim: working } = last else {
            panic!("expected the claim back at work, got {last:?}");
        };
        assert_eq!(working.last_seen, at(9));
        assert_eq!(
            decide_drop_pr(Some(&claim), &drop(BOB, "acme/api#12"), at(9)),
            Err(DropRefused::HolderOnly(HolderOnly::NotHolder {
                member: ALICE
            }))
        );
        assert_eq!(
            decide_drop_pr(Some(&claim), &drop(ALICE, "acme/api#99"), at(9)),
            Err(DropRefused::NotRecorded {
                pr: pr("acme/api#99")
            })
        );
        assert_eq!(
            decide_drop_pr(None, &drop(ALICE, "acme/api#12"), at(9)),
            Err(DropRefused::HolderOnly(HolderOnly::NoClaim))
        );
    }
}
