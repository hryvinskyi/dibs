//! Where a claim's branch was learned from, and which recording of a branch may replace the one a
//! claim holds.
//!
//! A claim's branch in a repository can be learned four ways. From strongest to weakest: the head
//! branch of a pull request the claim recorded for review; the branch of a pool worktree held for
//! the claim's item; a branch the claim named when it was taken or renewed; the branch of the
//! checkout a session works in. A weaker recording never replaces a stronger one while the stronger
//! one still holds, and two recordings of equal strength but another identity — another pull
//! request, another worktree — never replace each other while the recorded one holds, so a claim
//! with two pull requests, or two worktrees, does not flip between their branches.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::claim::Claim;
use crate::event::BranchRecorded;
use crate::radar::BranchName;
use crate::repo::RepoId;
use crate::report::MachineId;
use crate::review::PrRef;
use crate::worktree::{PoolId, PoolKey, WorktreeName};

/// One worktree of one pool: the machine the pool lives on, the pool there and the worktree's name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorktreeRef {
    /// The machine.
    pub machine: MachineId,
    /// The pool on that machine.
    pub pool: PoolId,
    /// The worktree of that pool.
    pub worktree: WorktreeName,
}

impl WorktreeRef {
    /// The worktree `worktree` of the pool `key`.
    pub fn of(key: &PoolKey, worktree: &WorktreeName) -> Self {
        Self {
            machine: key.machine.clone(),
            pool: key.pool.clone(),
            worktree: worktree.clone(),
        }
    }
}

/// Where a claim's branch was learned from, with what identifies it: the pull request, or the pool
/// worktree. A branch the claim named, and the branch of a session's checkout, need nothing more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchSource {
    /// The head branch of a pull request the claim recorded for review.
    PullRequest(PrRef),
    /// The branch of a pool worktree held for the claim's item.
    Worktree(WorktreeRef),
    /// A branch the claim named when it was taken or renewed.
    Named,
    /// The branch of the checkout a session works in.
    Checkout,
}

/// The name of where a branch was learned from, as the log and the views carry it. A branch
/// recorded before sources were kept was learned from a checkout, so that is the default.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum BranchSourceKind {
    /// A pull request's head branch.
    PullRequest,
    /// A pool worktree's branch.
    Worktree,
    /// A branch the claim named.
    Named,
    /// A checkout's branch.
    #[default]
    Checkout,
}

impl BranchSourceKind {
    /// The stable name: `pull_request`, `worktree`, `named` or `checkout`.
    pub const fn code(self) -> &'static str {
        match self {
            Self::PullRequest => "pull_request",
            Self::Worktree => "worktree",
            Self::Named => "named",
            Self::Checkout => "checkout",
        }
    }

    /// How strong a branch learned this way is: a pull request's head over a pool worktree's
    /// branch, over a named branch, over a checkout's branch.
    pub const fn precedence(self) -> u8 {
        match self {
            Self::PullRequest => 4,
            Self::Worktree => 3,
            Self::Named => 2,
            Self::Checkout => 1,
        }
    }
}

/// Why the parts of a stored source do not make one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BranchSourceError {
    /// A pull request's head that names no pull request.
    #[error("a branch learned from a pull request names the pull request")]
    NoPullRequest,
    /// A pool worktree's branch that names no worktree.
    #[error("a branch learned from a pool worktree names the worktree")]
    NoWorktree,
    /// A pull request or a worktree named by a source of another kind.
    #[error("a source names a pull request or a worktree only when it is learned from one")]
    Stray,
}

/// What the caller knows of the pool worktrees: whether one is still held for the claim's item. It
/// is asked only about a branch learned from a pool worktree, so a caller can leave the pools
/// unread until a recorded source is one.
pub trait HeldWorktrees {
    /// Whether `worktree` is still held for the claim's item.
    fn holds(&self, worktree: &WorktreeRef) -> bool;
}

impl<F: Fn(&WorktreeRef) -> bool> HeldWorktrees for F {
    fn holds(&self, worktree: &WorktreeRef) -> bool {
        self(worktree)
    }
}

impl BranchSource {
    /// A source from its stored parts: its kind, and the pull request or the worktree only that
    /// kind names.
    ///
    /// # Errors
    /// [`BranchSourceError`] when the kind lacks what identifies it, or a part names what only
    /// another kind names.
    pub fn from_parts(
        kind: BranchSourceKind,
        pr: Option<PrRef>,
        worktree: Option<WorktreeRef>,
    ) -> Result<Self, BranchSourceError> {
        match (kind, pr, worktree) {
            (BranchSourceKind::PullRequest, Some(pr), None) => Ok(Self::PullRequest(pr)),
            (BranchSourceKind::PullRequest, None, _) => Err(BranchSourceError::NoPullRequest),
            (BranchSourceKind::Worktree, None, Some(worktree)) => Ok(Self::Worktree(worktree)),
            (BranchSourceKind::Worktree, _, None) => Err(BranchSourceError::NoWorktree),
            (BranchSourceKind::Named, None, None) => Ok(Self::Named),
            (BranchSourceKind::Checkout, None, None) => Ok(Self::Checkout),
            _ => Err(BranchSourceError::Stray),
        }
    }

    /// Its name.
    pub const fn kind(&self) -> BranchSourceKind {
        match self {
            Self::PullRequest(_) => BranchSourceKind::PullRequest,
            Self::Worktree(_) => BranchSourceKind::Worktree,
            Self::Named => BranchSourceKind::Named,
            Self::Checkout => BranchSourceKind::Checkout,
        }
    }

    /// The pull request it names, for a pull request's head.
    pub const fn pr(&self) -> Option<&PrRef> {
        match self {
            Self::PullRequest(pr) => Some(pr),
            _ => None,
        }
    }

    /// The worktree it names, for a pool worktree's branch.
    pub const fn worktree(&self) -> Option<&WorktreeRef> {
        match self {
            Self::Worktree(worktree) => Some(worktree),
            _ => None,
        }
    }

    /// Whether a branch learned this way is stronger than one learned the `other` way.
    pub const fn outranks(&self, other: &Self) -> bool {
        self.kind().precedence() > other.kind().precedence()
    }

    /// Whether the branch `claim` learned this way still stands against a recording of another
    /// identity or a weaker one. A pull request's head holds while the claim is in review and
    /// still records that pull request; a pool worktree's branch while `worktrees` says the
    /// worktree is held for the claim's item; a named branch until another recording of the same
    /// strength or a stronger one replaces it; a checkout's branch never stands against anything.
    pub fn holds(&self, claim: &Claim, worktrees: &impl HeldWorktrees) -> bool {
        match self {
            Self::PullRequest(pr) => claim.state.records(pr),
            Self::Worktree(worktree) => worktrees.holds(worktree),
            Self::Named => true,
            Self::Checkout => false,
        }
    }
}

/// The branch a claim's work is on in one repository, and where it was learned from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedBranch {
    /// The branch.
    pub branch: BranchName,
    /// Where it was learned from.
    pub source: BranchSource,
}

impl RecordedBranch {
    /// `branch`, learned from `source`.
    pub const fn new(branch: BranchName, source: BranchSource) -> Self {
        Self { branch, source }
    }
}

/// What recording a branch on a claim does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchRecording {
    /// Nothing changes: the claim records that branch from that source already, or records one
    /// the recording may not replace.
    Kept,
    /// The claim records the same branch, learned from another source now. Only the claim is
    /// stored: its footprint still measures that branch, and the log is not told.
    SourceChanged(Claim),
    /// The claim records another branch now. The claim is stored, `recorded` is appended to the
    /// log, and the claim's footprint of the repository, which measured the branch before, goes.
    BranchChanged {
        /// The claim to store.
        claim: Claim,
        /// The event to append.
        recorded: BranchRecorded,
    },
}

/// Records `branch`, learned from `source`, as the branch of `claim`'s work in `repo`. It replaces
/// what the claim records there when the claim records nothing there, when `source` is stronger
/// than the recorded source, when it is the recorded source itself (the same pull request, the
/// same worktree, a named branch over a named one, a checkout over a checkout), or when the
/// recorded source no longer holds; `worktrees` answers that for a branch learned from a pool
/// worktree.
pub fn record_branch(
    claim: &Claim,
    repo: &RepoId,
    branch: &BranchName,
    source: BranchSource,
    worktrees: &impl HeldWorktrees,
) -> BranchRecording {
    let recorded = claim.branches.get(repo);
    if let Some(recorded) = recorded {
        if recorded.branch == *branch && recorded.source == source {
            return BranchRecording::Kept;
        }
        let replaces = source.outranks(&recorded.source)
            || source == recorded.source
            || !recorded.source.holds(claim, worktrees);
        if !replaces {
            return BranchRecording::Kept;
        }
    }
    let same_branch = recorded.is_some_and(|recorded| recorded.branch == *branch);
    let kind = source.kind();
    let mut changed = claim.clone();
    changed
        .branches
        .insert(repo.clone(), RecordedBranch::new(branch.clone(), source));
    if same_branch {
        return BranchRecording::SourceChanged(changed);
    }
    let recorded = BranchRecorded {
        item: changed.item,
        member: changed.member,
        repo: repo.clone(),
        branch: branch.clone(),
        source: kind,
    };
    BranchRecording::BranchChanged {
        claim: changed,
        recorded,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::review::{ClaimState, Review};
    use crate::testing::{ALICE, at, claim};

    fn api() -> RepoId {
        RepoId::parse("api").unwrap()
    }

    fn branch(text: &str) -> BranchName {
        BranchName::parse(text).unwrap()
    }

    fn pr(text: &str) -> PrRef {
        PrRef::parse(text).unwrap()
    }

    fn worktree(name: &str) -> WorktreeRef {
        WorktreeRef {
            machine: MachineId::from_bits(1),
            pool: PoolId::parse("a1").unwrap(),
            worktree: WorktreeName::parse(name).unwrap(),
        }
    }

    fn from_pr(text: &str) -> BranchSource {
        BranchSource::PullRequest(pr(text))
    }

    fn from_worktree(name: &str) -> BranchSource {
        BranchSource::Worktree(worktree(name))
    }

    /// Holds no pool worktree.
    fn none_held(_: &WorktreeRef) -> bool {
        false
    }

    /// Holds every pool worktree.
    fn all_held(_: &WorktreeRef) -> bool {
        true
    }

    /// Alice's claim on item 1, its work on `name` in `api` learned from `source`.
    fn on(name: &str, source: BranchSource) -> Claim {
        let mut made = claim(1, ALICE, &[], 0);
        made.branches
            .insert(api(), RecordedBranch::new(branch(name), source));
        made
    }

    /// `made` in review of `prs`.
    fn in_review(mut made: Claim, prs: &[&str]) -> Claim {
        let prs: BTreeSet<PrRef> = prs.iter().map(|text| pr(text)).collect();
        made.state = ClaimState::InReview(Review::new(prs, at(1), false).unwrap());
        made
    }

    /// What the claim records in `api` after the recording, or `None` when it is kept as it was.
    fn after(recording: &BranchRecording) -> Option<(String, BranchSource)> {
        let claim = match recording {
            BranchRecording::Kept => return None,
            BranchRecording::SourceChanged(claim)
            | BranchRecording::BranchChanged { claim, .. } => claim,
        };
        let recorded = claim.branches.get(&api())?;
        Some((recorded.branch.to_string(), recorded.source.clone()))
    }

    fn record(
        made: &Claim,
        name: &str,
        source: BranchSource,
        worktrees: &impl HeldWorktrees,
    ) -> BranchRecording {
        record_branch(made, &api(), &branch(name), source, worktrees)
    }

    #[test]
    fn a_pull_request_outranks_a_worktree_which_outranks_a_named_branch_then_a_checkout() {
        let order = [
            from_pr("acme/api#1"),
            from_worktree("worktree1"),
            BranchSource::Named,
            BranchSource::Checkout,
        ];
        for (stronger, source) in order.iter().enumerate() {
            for (weaker, other) in order.iter().enumerate() {
                assert_eq!(
                    source.outranks(other),
                    stronger < weaker,
                    "{source:?} over {other:?}"
                );
            }
        }
        let kinds: Vec<&str> = order.iter().map(|source| source.kind().code()).collect();
        assert_eq!(kinds, ["pull_request", "worktree", "named", "checkout"]);
        assert_eq!(BranchSourceKind::default(), BranchSourceKind::Checkout);
    }

    #[test]
    fn a_first_branch_is_recorded_with_its_source_and_an_event() {
        let made = claim(1, ALICE, &[], 0);
        let recording = record(&made, "alice/1-x", BranchSource::Named, &none_held);
        let BranchRecording::BranchChanged { claim, recorded } = recording else {
            panic!("a first branch changes the branch: {recording:?}");
        };
        assert_eq!(claim.branch(&api()), Some(&branch("alice/1-x")));
        assert_eq!(claim.branch_source(&api()), Some(&BranchSource::Named));
        assert_eq!(
            recorded,
            BranchRecorded {
                item: made.item,
                member: ALICE,
                repo: api(),
                branch: branch("alice/1-x"),
                source: BranchSourceKind::Named,
            }
        );
        let mut both = claim.clone();
        both.branches.insert(
            RepoId::parse("web").unwrap(),
            RecordedBranch::new(branch("main"), BranchSource::Checkout),
        );
        let moved = record(&both, "alice/1-y", BranchSource::Named, &none_held);
        let names: Vec<(String, String)> = match &moved {
            BranchRecording::BranchChanged { claim, .. } => claim
                .branch_names()
                .map(|(repo, name)| (repo.to_string(), name.to_string()))
                .collect(),
            other => panic!("another named branch replaces the named one: {other:?}"),
        };
        assert_eq!(
            names,
            [
                ("api".to_owned(), "alice/1-y".to_owned()),
                ("web".to_owned(), "main".to_owned())
            ],
            "one branch per repository, the other repository's untouched"
        );
    }

    #[test]
    fn the_same_branch_from_the_same_source_changes_nothing() {
        for source in [
            from_pr("acme/api#1"),
            from_worktree("worktree1"),
            BranchSource::Named,
            BranchSource::Checkout,
        ] {
            let made = in_review(on("alice/1-x", source.clone()), &["acme/api#1"]);
            assert_eq!(
                record(&made, "alice/1-x", source.clone(), &all_held),
                BranchRecording::Kept,
                "{source:?}"
            );
        }
    }

    #[test]
    fn a_stronger_source_replaces_a_weaker_one_whether_or_not_it_holds() {
        let checkout = on("alice/1-x", BranchSource::Checkout);
        let named = record(&checkout, "alice/1-y", BranchSource::Named, &all_held);
        assert_eq!(
            after(&named),
            Some(("alice/1-y".to_owned(), BranchSource::Named))
        );
        let held = on("alice/1-x", from_worktree("worktree1"));
        let reviewed = in_review(held, &["acme/api#2"]);
        let head = record(&reviewed, "alice/1-pr", from_pr("acme/api#2"), &all_held);
        assert_eq!(
            after(&head),
            Some(("alice/1-pr".to_owned(), from_pr("acme/api#2")))
        );
        let named = on("alice/1-x", BranchSource::Named);
        let from_tree = record(&named, "wt-1", from_worktree("worktree1"), &all_held);
        assert_eq!(
            after(&from_tree),
            Some(("wt-1".to_owned(), from_worktree("worktree1")))
        );
    }

    #[test]
    fn a_weaker_source_waits_while_the_recorded_one_holds_and_replaces_it_after() {
        let named = on("alice/1-x", BranchSource::Named);
        assert_eq!(
            record(&named, "other", BranchSource::Checkout, &all_held),
            BranchRecording::Kept,
            "a named branch holds until it is replaced"
        );

        let reviewed = in_review(on("alice/1-x", from_pr("acme/api#1")), &["acme/api#1"]);
        for source in [
            from_worktree("worktree1"),
            BranchSource::Named,
            BranchSource::Checkout,
        ] {
            assert_eq!(
                record(&reviewed, "other", source.clone(), &all_held),
                BranchRecording::Kept,
                "a pull request still recorded holds against {source:?}"
            );
        }
        let ended = on("alice/1-x", from_pr("acme/api#1"));
        assert_eq!(
            after(&record(&ended, "other", BranchSource::Checkout, &all_held)),
            Some(("other".to_owned(), BranchSource::Checkout)),
            "a review that ended no longer holds the pull request's head"
        );
        let dropped = in_review(on("alice/1-x", from_pr("acme/api#1")), &["acme/api#2"]);
        assert_eq!(
            after(&record(&dropped, "other", BranchSource::Named, &all_held)),
            Some(("other".to_owned(), BranchSource::Named)),
            "a pull request the review no longer records no longer holds"
        );

        let from_tree = on("alice/1-x", from_worktree("worktree1"));
        assert_eq!(
            record(&from_tree, "other", BranchSource::Named, &all_held),
            BranchRecording::Kept,
            "a worktree still held holds"
        );
        let asked = std::cell::RefCell::new(Vec::new());
        let held_elsewhere = |asked_about: &WorktreeRef| {
            asked.borrow_mut().push(asked_about.clone());
            false
        };
        assert_eq!(
            after(&record(
                &from_tree,
                "other",
                BranchSource::Checkout,
                &held_elsewhere
            )),
            Some(("other".to_owned(), BranchSource::Checkout)),
            "a worktree no longer held no longer holds"
        );
        assert_eq!(asked.into_inner(), [worktree("worktree1")]);
    }

    #[test]
    fn the_same_pull_request_moves_its_head_and_another_waits_while_the_first_is_recorded() {
        let both = ["acme/api#1", "acme/api#2"];
        let first = in_review(on("alice/1-x", from_pr("acme/api#1")), &both);
        assert_eq!(
            after(&record(
                &first,
                "alice/1-y",
                from_pr("acme/api#1"),
                &all_held
            )),
            Some(("alice/1-y".to_owned(), from_pr("acme/api#1"))),
            "a new head of the same pull request"
        );
        assert_eq!(
            record(&first, "alice/1-z", from_pr("acme/api#2"), &all_held),
            BranchRecording::Kept,
            "another pull request while the first is still recorded"
        );
        let merged = in_review(on("alice/1-x", from_pr("acme/api#1")), &["acme/api#2"]);
        assert_eq!(
            after(&record(
                &merged,
                "alice/1-z",
                from_pr("acme/api#2"),
                &all_held
            )),
            Some(("alice/1-z".to_owned(), from_pr("acme/api#2"))),
            "the next pull request once the first is no longer recorded"
        );
    }

    #[test]
    fn the_same_worktree_moves_its_branch_and_another_waits_while_the_first_is_held() {
        let first = on("wt-1", from_worktree("worktree1"));
        assert_eq!(
            after(&record(
                &first,
                "wt-1b",
                from_worktree("worktree1"),
                &all_held
            )),
            Some(("wt-1b".to_owned(), from_worktree("worktree1")))
        );
        assert_eq!(
            record(&first, "wt-2", from_worktree("worktree2"), &all_held),
            BranchRecording::Kept,
            "another worktree while the first is held"
        );
        assert_eq!(
            after(&record(
                &first,
                "wt-2",
                from_worktree("worktree2"),
                &none_held
            )),
            Some(("wt-2".to_owned(), from_worktree("worktree2"))),
            "another worktree once the first is no longer held"
        );
    }

    #[test]
    fn a_named_branch_replaces_a_named_one_and_anything_replaces_a_checkouts() {
        let named = on("alice/1-x", BranchSource::Named);
        assert_eq!(
            after(&record(&named, "alice/1-y", BranchSource::Named, &all_held)),
            Some(("alice/1-y".to_owned(), BranchSource::Named))
        );
        let checkout = in_review(on("alice/1-x", BranchSource::Checkout), &["acme/api#1"]);
        for source in [
            from_pr("acme/api#1"),
            from_worktree("worktree1"),
            BranchSource::Named,
            BranchSource::Checkout,
        ] {
            assert_eq!(
                after(&record(&checkout, "alice/1-y", source.clone(), &all_held)),
                Some(("alice/1-y".to_owned(), source))
            );
        }
    }

    #[test]
    fn the_same_branch_from_another_source_changes_the_source_alone() {
        let checkout = on("alice/1-x", BranchSource::Checkout);
        let recording = record(&checkout, "alice/1-x", BranchSource::Named, &all_held);
        let BranchRecording::SourceChanged(claim) = recording else {
            panic!("only the source changes: {recording:?}");
        };
        assert_eq!(claim.branch(&api()), Some(&branch("alice/1-x")));
        assert_eq!(claim.branch_source(&api()), Some(&BranchSource::Named));
        let ended = on("alice/1-x", from_pr("acme/api#1"));
        assert!(
            matches!(
                record(&ended, "alice/1-x", BranchSource::Checkout, &all_held),
                BranchRecording::SourceChanged(_)
            ),
            "a weaker source on the same branch once the recorded one no longer holds"
        );
    }

    #[test]
    fn a_stored_source_has_what_identifies_it_and_nothing_another_kind_names() {
        let ok = [
            (BranchSourceKind::PullRequest, Some(pr("acme/api#1")), None),
            (
                BranchSourceKind::Worktree,
                None,
                Some(worktree("worktree1")),
            ),
            (BranchSourceKind::Named, None, None),
            (BranchSourceKind::Checkout, None, None),
        ];
        for (kind, with_pr, with_worktree) in ok {
            let source = BranchSource::from_parts(kind, with_pr.clone(), with_worktree.clone());
            let source = source.unwrap();
            assert_eq!(
                (
                    source.kind(),
                    source.pr().cloned(),
                    source.worktree().cloned()
                ),
                (kind, with_pr, with_worktree)
            );
        }
        let refused = [
            (
                BranchSourceKind::PullRequest,
                None,
                None,
                BranchSourceError::NoPullRequest,
            ),
            (
                BranchSourceKind::Worktree,
                None,
                None,
                BranchSourceError::NoWorktree,
            ),
            (
                BranchSourceKind::PullRequest,
                Some(pr("acme/api#1")),
                Some(worktree("worktree1")),
                BranchSourceError::Stray,
            ),
            (
                BranchSourceKind::Named,
                Some(pr("acme/api#1")),
                None,
                BranchSourceError::Stray,
            ),
            (
                BranchSourceKind::Checkout,
                None,
                Some(worktree("worktree1")),
                BranchSourceError::Stray,
            ),
        ];
        for (kind, with_pr, with_worktree, error) in refused {
            assert_eq!(
                BranchSource::from_parts(kind, with_pr, with_worktree),
                Err(error),
                "{kind:?}"
            );
        }
    }
}
