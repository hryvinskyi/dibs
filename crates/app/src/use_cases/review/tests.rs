//! Review over the in-memory store: a holder records pull requests on a claim, an integration
//! reports merges and closes, and reads the claims in review and the status of their pull requests.

use time::Duration;

use dibs_domain::claim::Claim;
use dibs_domain::event::{
    CompletedByMerge, EventKind, PrReported, ReviewEnded, ReviewStarted as Started,
};
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemStatus;
use dibs_domain::message::{Author, Importance};
use dibs_domain::radar::{BranchName, BranchSource};
use dibs_domain::review::{ClaimState, PrRef};
use dibs_domain::slot::{Purpose, SlotName};

use super::*;
use crate::records::MessageRecord;
use crate::use_cases::testing::{Env, Person};
use crate::use_cases::writing::TITLE_NOTE;
use crate::use_cases::{
    ClaimItem, ClaimItemInput, HoldMode, ListThreads, ReadThread, ReleaseSlot, ReleaseSlotInput,
    Status, TakeSlot, TakeSlotInput, WideInput,
};

const SETTINGS: &str = r#"{
    "stale_after": "24h",
    "areas": [
        {"name": "billing", "paths": ["packages/billing/**"]},
        {"name": "search", "paths": ["packages/search/**"]}
    ],
    "pr_status": {"enabled": true},
    "slots": [
        {"name": "db:migrate", "max_hold": "1h", "freezes": ["refs/heads/main"]}
    ]
}"#;

/// A merge into `main`.
fn merged() -> ReportedEnd {
    ReportedEnd::Merged {
        base: "main".to_owned(),
        merged_at: time::OffsetDateTime::UNIX_EPOCH,
        files: None,
        head: None,
    }
}

fn pr(text: &str) -> PrRef {
    PrRef::parse(text).unwrap()
}

async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str]) {
    ClaimItem::new(&env.deps)
        .execute(
            &who.actor,
            ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item,
                session: who.session,
                areas: areas.iter().map(|area| (*area).to_owned()).collect(),
                kind: None,
            },
        )
        .await
        .unwrap();
}

async fn review(
    env: &Env,
    who: &Person,
    item: ItemId,
    text: &str,
    more: bool,
) -> Result<ReviewedClaim, AppError> {
    StartReview::new(&env.deps)
        .execute(
            &who.actor,
            StartReviewInput {
                head: None,
                item,
                pr: pr(text),
                more,
            },
        )
        .await
}

async fn report(env: &Env, text: &str, end: ReportedEnd) -> PrReportOutcome {
    ReportPr::new(&env.deps)
        .execute(&env.importer, PrReport { pr: pr(text), end })
        .await
        .unwrap()
}

async fn stored(env: &Env, item: ItemId) -> Option<Claim> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.claim(env.project, item).await.unwrap()
}

async fn status_of(env: &Env, item: ItemId) -> ItemStatus {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.item(env.project, item).await.unwrap().unwrap().status
}

/// The subjects of the threads `who` can read.
async fn subjects(env: &Env, who: &Person) -> Vec<String> {
    ListThreads::new(&env.deps)
        .execute(&who.actor, None)
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.thread.record.subject.to_string())
        .collect()
}

/// The one message of the one conversation `who` can read.
async fn the_notice(env: &Env, who: &Person) -> MessageRecord {
    let threads = ListThreads::new(&env.deps)
        .execute(&who.actor, None)
        .await
        .unwrap();
    assert_eq!(threads.len(), 1, "one conversation");
    let view = ReadThread::new(&env.deps)
        .execute(&who.actor, threads[0].thread.record.id)
        .await
        .unwrap();
    assert_eq!(view.messages.len(), 1, "one message");
    view.messages[0].record.clone()
}

/// Asserts that `notice` is a message of normal importance asking no acknowledgement, whose text
/// is `lead` followed by the line on whose words a title is.
fn assert_told(notice: &MessageRecord, lead: &str) {
    assert_eq!(notice.body.as_str(), format!("{lead}\n{TITLE_NOTE}"));
    assert_eq!(notice.importance, Importance::Normal);
    assert!(!notice.ack_required, "it asks no acknowledgement");
}

#[tokio::test]
async fn the_holder_moves_a_claim_to_review_and_its_areas_stay_held() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    let before = env.events().await.len();
    let reviewed = review(&env, &env.bob, item, "acme/widgets#7", false)
        .await
        .unwrap();
    assert!(reviewed.claim.claim.state.is_in_review());
    assert_eq!(reviewed.claim.holder.as_str(), "bob");
    assert_eq!(
        env.kinds_after(before).await,
        [EventKind::ReviewStarted(Started {
            item,
            member: env.bob.actor.member,
            prs: [pr("acme/widgets#7")].into(),
        })]
    );
    let again = env.events().await.len();
    review(&env, &env.bob, item, "Acme/Widgets#7", false)
        .await
        .unwrap();
    assert_eq!(
        env.events().await.len(),
        again,
        "the same pull request again"
    );
    env.advance(Duration::days(30));
    let status = Status::new(&env.deps)
        .execute(&env.alice.actor)
        .await
        .unwrap();
    assert_eq!(
        status
            .live
            .iter()
            .map(|held| held.item.item.id)
            .collect::<Vec<_>>(),
        [item],
        "a claim in review never goes stale while its pull request is open"
    );
    assert_eq!(status.holds.len(), 1, "its area is still held");
}

#[tokio::test]
async fn only_the_holder_asks_for_review_and_an_integration_cannot() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    let refused = review(&env, &env.alice, item, "acme/widgets#7", false)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "not_holder");
    let nobody = env.item("gh:acme/widgets#43").await;
    assert_eq!(
        review(&env, &env.bob, nobody, "acme/widgets#8", false)
            .await
            .unwrap_err()
            .code(),
        "no_claim"
    );
    let bot = StartReview::new(&env.deps)
        .execute(
            &env.importer,
            StartReviewInput {
                head: None,
                item,
                pr: pr("acme/widgets#7"),
                more: false,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bot.code(), "forbidden");
    let other = env.item("gh:acme/widgets#44").await;
    claim(&env, &env.alice, other, &[]).await;
    review(&env, &env.bob, item, "acme/widgets#7", false)
        .await
        .unwrap();
    assert_eq!(
        review(&env, &env.alice, other, "acme/widgets#7", false)
            .await
            .unwrap_err()
            .code(),
        "pr_recorded"
    );
    env.advance(Duration::hours(25));
    assert_eq!(
        review(&env, &env.alice, other, "acme/widgets#9", false)
            .await
            .unwrap_err()
            .code(),
        "claim_stale"
    );
}

#[tokio::test]
async fn the_last_recorded_pull_request_merged_completes_the_claim_and_tells_its_holder_once() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    review(&env, &env.bob, item, "acme/widgets#7", false)
        .await
        .unwrap();
    let before = env.events().await.len();
    assert_eq!(
        report(&env, "acme/widgets#7", merged()).await,
        PrReportOutcome::Completed(item)
    );
    assert_eq!(stored(&env, item).await, None, "the claim ended");
    assert_eq!(status_of(&env, item).await, ItemStatus::Done);
    let kinds = env.kinds_after(before).await;
    let bob = env.bob.actor.member;
    assert_eq!(
        kinds[..2],
        [
            EventKind::PrMerged(PrReported {
                item,
                member: bob,
                pr: pr("acme/widgets#7"),
            }),
            EventKind::CompletedByMerge(CompletedByMerge { item, member: bob }),
        ]
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, EventKind::MessagePosted(_)))
            .count(),
        1,
        "one message to the holder"
    );
    assert_eq!(
        subjects(&env, &env.bob).await,
        ["Your claim on gh:acme/widgets#42 \"Work on gh:acme/widgets#42\""]
    );
    assert_told(
        &the_notice(&env, &env.bob).await,
        "Done by merge: pull request acme/widgets#7 merged; the claim on work item gh:acme/widgets#42 \"Work on gh:acme/widgets#42\" is done. Its areas are free and the item is done.",
    );
    let threads = ListThreads::new(&env.deps)
        .execute(&env.bob.actor, None)
        .await
        .unwrap();
    assert_eq!(threads[0].thread.record.author, Author::System);
    let status = Status::new(&env.deps)
        .execute(&env.alice.actor)
        .await
        .unwrap();
    assert!(status.holds.is_empty(), "its areas are free");
    let again = env.events().await.len();
    assert_eq!(
        report(&env, "acme/widgets#7", merged()).await,
        PrReportOutcome::Unmatched,
        "a repeated report matches nothing"
    );
    assert_eq!(env.events().await.len(), again, "and changes nothing");
}

#[tokio::test]
async fn a_merge_with_more_to_follow_returns_the_claim_to_work_with_its_areas() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    review(&env, &env.bob, item, "acme/widgets#7", true)
        .await
        .unwrap();
    env.advance(Duration::days(3));
    let before = env.events().await.len();
    assert_eq!(
        report(&env, "acme/widgets#7", merged()).await,
        PrReportOutcome::Working(item)
    );
    let claim = stored(&env, item).await.unwrap();
    assert_eq!(claim.state, ClaimState::Working);
    assert_eq!(claim.areas.len(), 1, "its areas are kept");
    assert_eq!(claim.last_seen, env.now(), "seen now, not stale on arrival");
    let bob = env.bob.actor.member;
    assert_eq!(
        env.kinds_after(before).await,
        [
            EventKind::PrMerged(PrReported {
                item,
                member: bob,
                pr: pr("acme/widgets#7"),
            }),
            EventKind::ReviewEnded(ReviewEnded { item, member: bob }),
        ]
    );
    assert_eq!(status_of(&env, item).await, ItemStatus::Claimed);
}

#[tokio::test]
async fn a_claim_with_two_pull_requests_completes_only_after_both_merge() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    review(&env, &env.bob, item, "acme/api#7", false)
        .await
        .unwrap();
    review(&env, &env.bob, item, "acme/web#3", false)
        .await
        .unwrap();
    assert_eq!(
        report(&env, "acme/web#3", merged()).await,
        PrReportOutcome::StillInReview(item)
    );
    assert!(stored(&env, item).await.unwrap().state.is_in_review());
    assert_eq!(
        report(&env, "acme/api#7", merged()).await,
        PrReportOutcome::Completed(item)
    );
}

#[tokio::test]
async fn an_unrecorded_pull_request_matches_nothing_and_changes_nothing() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    let before = env.events().await.len();
    assert_eq!(
        report(&env, "acme/widgets#42", merged()).await,
        PrReportOutcome::Unmatched,
        "naming the item's ticket number is not recording a pull request"
    );
    assert_eq!(env.events().await.len(), before);
    assert_eq!(stored(&env, item).await.unwrap().state, ClaimState::Working);
}

#[tokio::test]
async fn a_pull_request_closed_unmerged_returns_the_claim_to_work_and_says_so() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    review(&env, &env.bob, item, "acme/widgets#7", false)
        .await
        .unwrap();
    env.advance(Duration::days(5));
    assert_eq!(
        report(&env, "acme/widgets#7", ReportedEnd::Closed).await,
        PrReportOutcome::Working(item)
    );
    let claim = stored(&env, item).await.unwrap();
    assert_eq!(claim.state, ClaimState::Working);
    assert_eq!(claim.last_seen, env.now());
    assert_eq!(
        subjects(&env, &env.bob).await,
        ["Your claim on gh:acme/widgets#42 \"Work on gh:acme/widgets#42\""]
    );
    assert_told(
        &the_notice(&env, &env.bob).await,
        "Back from review: pull request acme/widgets#7 was closed without being merged, so your claim on work item gh:acme/widgets#42 \"Work on gh:acme/widgets#42\" is no longer in review. You still hold it and its areas; work on it again, record the pull request that brings the work in, or release it.",
    );
}

#[tokio::test]
async fn a_holder_in_review_is_asked_by_a_wide_claim_once_back_at_work() {
    let env = Env::with_settings(SETTINGS).await;
    let held = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, held, &["billing"]).await;
    review(&env, &env.bob, held, "acme/widgets#7", false)
        .await
        .unwrap();
    let wide = env.item("gh:acme/widgets#50").await;
    ClaimItem::new(&env.deps)
        .execute(
            &env.alice.actor,
            ClaimItemInput {
                branches: Default::default(),
                item: wide,
                session: env.alice.session,
                areas: Vec::new(),
                wide: Some(WideInput {
                    areas: vec!["billing".to_owned()],
                    reason: dibs_domain::wide::WideReason::parse("Rename the money type").unwrap(),
                }),
                kind: None,
            },
        )
        .await
        .unwrap();
    assert!(
        subjects(&env, &env.bob).await.is_empty(),
        "no announcement while the holder is in review"
    );
    report(&env, "acme/widgets#7", ReportedEnd::Closed).await;
    let bobs = subjects(&env, &env.bob).await;
    assert_eq!(bobs.len(), 2, "{bobs:?}");
    assert!(
        bobs.iter().any(|subject| subject.contains("billing")),
        "the announcement reached them: {bobs:?}"
    );
}

#[tokio::test]
async fn the_claims_in_review_are_listed_with_their_pull_requests_for_an_integration() {
    let env = Env::with_settings(SETTINGS).await;
    let reviewing = env.item("gh:acme/widgets#42").await;
    let working = env.item("gh:acme/widgets#43").await;
    claim(&env, &env.bob, reviewing, &["billing"]).await;
    claim(&env, &env.alice, working, &["search"]).await;
    review(&env, &env.bob, reviewing, "acme/widgets#7", true)
        .await
        .unwrap();
    let listed = ReviewClaims::new(&env.deps)
        .execute(&env.importer)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].item.id, reviewing);
    assert_eq!(listed[0].claim.holder.as_str(), "bob");
    assert_eq!(listed[0].prs, [pr("acme/widgets#7")].into());
    assert!(listed[0].more);
    assert_eq!(
        ReviewClaims::new(&env.deps)
            .execute(&env.bob.actor)
            .await
            .unwrap_err()
            .code(),
        "forbidden",
        "a member is no integration"
    );
    assert_eq!(
        ReportPr::new(&env.deps)
            .execute(
                &env.bob.actor,
                PrReport {
                    pr: pr("acme/widgets#7"),
                    end: merged()
                }
            )
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
}

#[tokio::test]
async fn the_pr_status_fails_while_a_freezing_slot_is_held_and_passes_after_release() {
    let env = Env::with_settings(SETTINGS).await;
    let env = &env;
    let read = |base: &'static str| async move {
        ReadPrStatus::new(&env.deps)
            .execute(
                &env.importer,
                PrStatusQuery {
                    repo: "default".to_owned(),
                    base: base.to_owned(),
                },
            )
            .await
    };
    let free = read("main").await.unwrap();
    assert!(free.enabled);
    assert_eq!(free.context, "dibs");
    assert_eq!(free.state, PrState::Success);
    TakeSlot::new(&env.deps)
        .execute(
            &env.alice.actor,
            TakeSlotInput {
                slot: SlotName::parse("db:migrate").unwrap(),
                session: env.alice.session,
                purpose: Purpose::parse("rename the invoices table").unwrap(),
                item: None,
                mode: HoldMode::Session,
            },
        )
        .await
        .unwrap();
    let held = read("refs/heads/main").await.unwrap();
    assert_eq!(held.state, PrState::Failure);
    assert!(
        held.description
            .starts_with("A freezing action is in progress until "),
        "{}",
        held.description
    );
    assert!(!held.description.contains("alice"));
    assert!(!held.description.contains("invoices"));
    assert_eq!(read("release").await.unwrap().state, PrState::Success);
    ReleaseSlot::new(&env.deps)
        .execute(
            &env.alice.actor,
            ReleaseSlotInput {
                slot: SlotName::parse("db:migrate").unwrap(),
                run: None,
                exit: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(read("main").await.unwrap().state, PrState::Success);
    assert_eq!(read("refs/tags/v1").await.unwrap_err().code(), "not_found");
    let unknown = ReadPrStatus::new(&env.deps)
        .execute(
            &env.importer,
            PrStatusQuery {
                repo: "acme/elsewhere".to_owned(),
                base: "main".to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.code(), "not_found");
    let member = ReadPrStatus::new(&env.deps)
        .execute(
            &env.bob.actor,
            PrStatusQuery {
                repo: "default".to_owned(),
                base: "main".to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(member.code(), "forbidden");
}

#[tokio::test]
async fn a_pull_request_closed_while_another_waits_is_told_and_the_other_merge_no_longer_completes()
{
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    review(&env, &env.bob, item, "acme/api#7", false)
        .await
        .unwrap();
    review(&env, &env.bob, item, "acme/web#3", false)
        .await
        .unwrap();
    assert_eq!(
        report(&env, "acme/web#3", ReportedEnd::Closed).await,
        PrReportOutcome::SetAside(item)
    );
    let held = stored(&env, item).await.unwrap();
    let review = held.state.review().unwrap();
    assert!(review.more(), "merges alone no longer complete it");
    assert_eq!(review.prs(), &[pr("acme/api#7")].into());
    assert_eq!(
        subjects(&env, &env.bob).await,
        ["Your claim on gh:acme/widgets#42 \"Work on gh:acme/widgets#42\""]
    );
    assert_told(
        &the_notice(&env, &env.bob).await,
        "Set aside in review: pull request acme/web#3 was closed without being merged. Your claim on work item gh:acme/widgets#42 \"Work on gh:acme/widgets#42\" stays in review on acme/api#7, but their merges no longer complete it: record the pull request that brings this work in (dibs claim review gh:acme/widgets#42 --pr <owner>/<repo>#<number>), or mark the item done yourself once it is in.",
    );
    assert_eq!(
        report(&env, "acme/api#7", merged()).await,
        PrReportOutcome::Working(item),
        "the other merge returns the claim to work rather than completing the item"
    );
    assert_eq!(status_of(&env, item).await, ItemStatus::Claimed);
}

/// The single repository protects `main` for alice.
const PROTECTED: &str = r#"{
    "stale_after": "24h",
    "areas": [{"name": "billing", "paths": ["packages/billing/**"]}],
    "protected_refs": [{"ref": "refs/heads/main", "pushers": ["alice"]}]
}"#;

#[tokio::test]
async fn a_stacked_pull_request_merged_into_a_branch_that_is_not_protected_completes_nothing() {
    let env = Env::with_settings(PROTECTED).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    review(&env, &env.bob, item, "acme/widgets#7", false)
        .await
        .unwrap();
    review(&env, &env.bob, item, "acme/widgets#8", false)
        .await
        .unwrap();
    let stacked = ReportedEnd::Merged {
        base: "bob/42-base".to_owned(),
        merged_at: time::OffsetDateTime::UNIX_EPOCH,
        files: None,
        head: None,
    };
    assert_eq!(
        report(&env, "acme/widgets#8", stacked.clone()).await,
        PrReportOutcome::SetAside(item)
    );
    assert!(
        stored(&env, item)
            .await
            .unwrap()
            .state
            .review()
            .unwrap()
            .more()
    );
    assert_eq!(
        report(
            &env,
            "acme/widgets#7",
            ReportedEnd::Merged {
                base: "refs/heads/main".to_owned(),
                merged_at: time::OffsetDateTime::UNIX_EPOCH,
                files: None,
                head: None,
            }
        )
        .await,
        PrReportOutcome::Working(item)
    );
    review(&env, &env.bob, item, "acme/widgets#9", false)
        .await
        .unwrap();
    assert_eq!(
        report(&env, "acme/widgets#9", stacked).await,
        PrReportOutcome::Working(item),
        "the last one merged elsewhere returns the claim to work, never completing it"
    );
    review(&env, &env.bob, item, "acme/widgets#10", false)
        .await
        .unwrap();
    assert_eq!(
        report(&env, "acme/widgets#10", merged()).await,
        PrReportOutcome::Completed(item)
    );
}

#[tokio::test]
async fn without_protected_branches_a_merge_into_any_base_counts() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    review(&env, &env.bob, item, "acme/widgets#7", false)
        .await
        .unwrap();
    assert_eq!(
        report(
            &env,
            "acme/widgets#7",
            ReportedEnd::Merged {
                base: "develop".to_owned(),
                merged_at: time::OffsetDateTime::UNIX_EPOCH,
                files: None,
                head: None,
            }
        )
        .await,
        PrReportOutcome::Completed(item)
    );
}

async fn drop_pr(
    env: &Env,
    who: &Person,
    item: ItemId,
    text: &str,
) -> Result<ReviewedClaim, AppError> {
    DropPr::new(&env.deps)
        .execute(&who.actor, DropPrInput { item, pr: pr(text) })
        .await
}

#[tokio::test]
async fn the_holder_drops_a_pull_request_recorded_by_mistake_and_leaves_review_with_the_last() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    review(&env, &env.bob, item, "acme/widgets#7", false)
        .await
        .unwrap();
    review(&env, &env.bob, item, "acme/widgets#8", false)
        .await
        .unwrap();
    let kept = drop_pr(&env, &env.bob, item, "acme/widgets#8")
        .await
        .unwrap();
    assert!(kept.claim.claim.state.records(&pr("acme/widgets#7")));
    assert!(!kept.claim.claim.state.records(&pr("acme/widgets#8")));
    env.advance(Duration::hours(3));
    let before = env.events().await.len();
    let left = drop_pr(&env, &env.bob, item, "acme/widgets#7")
        .await
        .unwrap();
    assert_eq!(left.claim.claim.state, ClaimState::Working);
    assert_eq!(left.claim.claim.last_seen, env.now());
    let bob = env.bob.actor.member;
    assert_eq!(
        env.kinds_after(before).await,
        [
            EventKind::PrDropped(PrReported {
                item,
                member: bob,
                pr: pr("acme/widgets#7"),
            }),
            EventKind::ReviewEnded(ReviewEnded { item, member: bob }),
        ]
    );
    assert_eq!(
        report(&env, "acme/widgets#7", merged()).await,
        PrReportOutcome::Unmatched,
        "a dropped pull request completes nothing"
    );
    assert_eq!(
        drop_pr(&env, &env.bob, item, "acme/widgets#7")
            .await
            .unwrap_err()
            .code(),
        "pr_not_recorded"
    );
    assert_eq!(
        drop_pr(&env, &env.alice, item, "acme/widgets#7")
            .await
            .unwrap_err()
            .code(),
        "not_holder"
    );
}

#[tokio::test]
async fn a_pull_request_another_claim_recorded_is_refused_naming_its_holder_and_item() {
    let env = Env::with_settings(SETTINGS).await;
    let held = env.item("gh:acme/widgets#42").await;
    let other = env.item("gh:acme/widgets#43").await;
    claim(&env, &env.bob, held, &[]).await;
    claim(&env, &env.alice, other, &[]).await;
    review(&env, &env.bob, held, "acme/widgets#7", false)
        .await
        .unwrap();
    let refused = review(&env, &env.alice, other, "acme/widgets#7", false)
        .await
        .unwrap_err();
    let AppError::Review(refusal) = refused else {
        panic!("not a review refusal: {refused:?}");
    };
    assert_eq!(refusal.member.as_ref().map(|h| h.as_str()), Some("bob"));
    assert_eq!(
        refusal
            .item_ref
            .as_deref()
            .map(ToString::to_string)
            .as_deref(),
        Some("gh:acme/widgets#42")
    );
}

/// `who` records `text` on their claim of `item`, with its head branch `head`.
async fn review_head(
    env: &Env,
    who: &Person,
    item: ItemId,
    text: &str,
    more: bool,
    head: &str,
) -> ReviewedClaim {
    StartReview::new(&env.deps)
        .execute(
            &who.actor,
            StartReviewInput {
                item,
                pr: pr(text),
                more,
                head: Some(BranchName::parse(head).unwrap()),
            },
        )
        .await
        .unwrap()
}

/// `who` renews their claim of `item`, naming `branch` in the project's one repository.
async fn name_branch(env: &Env, who: &Person, item: ItemId, branch: &str) {
    ClaimItem::new(&env.deps)
        .execute(
            &who.actor,
            ClaimItemInput {
                branches: [("default".to_owned(), BranchName::parse(branch).unwrap())].into(),
                item,
                session: who.session,
                areas: Vec::new(),
                wide: None,
                kind: None,
            },
        )
        .await
        .unwrap();
}

/// The branch `item`'s claim records in the project's one repository, and where it was learned.
async fn branch_of(env: &Env, item: ItemId) -> Option<(String, BranchSource)> {
    let claim = stored(env, item).await?;
    let recorded = claim.branches.get(&dibs_domain::repo::RepoId::implicit())?;
    Some((recorded.branch.to_string(), recorded.source.clone()))
}

/// The branches the log recorded after the first `skip` events, with their sources.
async fn recordings(env: &Env, skip: usize) -> Vec<(String, dibs_domain::radar::BranchSourceKind)> {
    env.kinds_after(skip)
        .await
        .into_iter()
        .filter_map(|kind| match kind {
            EventKind::BranchRecorded(recorded) => {
                Some((recorded.branch.to_string(), recorded.source))
            }
            _ => None,
        })
        .collect()
}

fn from_pr(branch: &str, text: &str) -> Option<(String, BranchSource)> {
    Some((branch.to_owned(), BranchSource::PullRequest(pr(text))))
}

#[tokio::test]
async fn a_head_named_with_a_pull_request_is_kept_against_weaker_recordings_while_in_review() {
    use crate::use_cases::Heartbeat;
    use crate::use_cases::testing::in_checkout;
    use dibs_domain::radar::BranchSourceKind;

    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    let before = env.events().await.len();
    let reviewed = review_head(
        &env,
        &env.bob,
        item,
        "acme/widgets#7",
        false,
        "bob/42-refunds",
    )
    .await;
    let names: Vec<&str> = env
        .kinds_after(before)
        .await
        .iter()
        .map(EventKind::name)
        .collect();
    assert_eq!(names, ["review_started", "branch_recorded"]);
    assert_eq!(
        recordings(&env, before).await,
        [("bob/42-refunds".to_owned(), BranchSourceKind::PullRequest)]
    );
    assert_eq!(
        reviewed
            .claim
            .claim
            .branch_source(&dibs_domain::repo::RepoId::implicit()),
        Some(&BranchSource::PullRequest(pr("acme/widgets#7"))),
        "the answer shows the claim as it stands after"
    );

    let later = env.events().await.len();
    Heartbeat::new(&env.deps)
        .execute(
            &env.bob.actor,
            env.bob.session,
            Some(&in_checkout('a', Some("bob/42-checkout"))),
        )
        .await
        .unwrap();
    assert_eq!(
        branch_of(&env, item).await,
        from_pr("bob/42-refunds", "acme/widgets#7"),
        "a claim in review is not fed by its checkout at all"
    );
    name_branch(&env, &env.bob, item, "bob/42-named").await;
    assert!(recordings(&env, later).await.is_empty());
    assert_eq!(
        branch_of(&env, item).await,
        from_pr("bob/42-refunds", "acme/widgets#7")
    );

    review_head(
        &env,
        &env.bob,
        item,
        "acme/widgets#7",
        false,
        "bob/42-refunds-2",
    )
    .await;
    assert_eq!(
        branch_of(&env, item).await,
        from_pr("bob/42-refunds-2", "acme/widgets#7"),
        "a new head of the same pull request moves the branch"
    );
}

#[tokio::test]
async fn another_pull_requests_head_waits_while_the_first_is_recorded_and_takes_over_once_merged() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    review_head(&env, &env.bob, item, "acme/widgets#7", true, "bob/42-first").await;
    let before = env.events().await.len();
    review_head(
        &env,
        &env.bob,
        item,
        "acme/widgets#8",
        true,
        "bob/42-second",
    )
    .await;
    assert!(recordings(&env, before).await.is_empty());
    assert_eq!(
        branch_of(&env, item).await,
        from_pr("bob/42-first", "acme/widgets#7")
    );
    assert_eq!(
        report(&env, "acme/widgets#7", merged()).await,
        PrReportOutcome::StillInReview(item)
    );
    review_head(
        &env,
        &env.bob,
        item,
        "acme/widgets#8",
        true,
        "bob/42-second",
    )
    .await;
    assert_eq!(
        branch_of(&env, item).await,
        from_pr("bob/42-second", "acme/widgets#8"),
        "the claim no longer records the first pull request"
    );
}

#[tokio::test]
async fn once_a_claim_leaves_review_a_named_branch_replaces_the_pull_requests_head() {
    let env = Env::with_settings(SETTINGS).await;
    let closed = env.item("gh:acme/widgets#42").await;
    let dropped = env.item("gh:acme/widgets#43").await;
    for (item, text) in [(closed, "acme/widgets#7"), (dropped, "acme/widgets#9")] {
        claim(&env, &env.bob, item, &[]).await;
        review_head(&env, &env.bob, item, text, false, "bob/head").await;
    }
    assert_eq!(
        report(&env, "acme/widgets#7", ReportedEnd::Closed).await,
        PrReportOutcome::Working(closed)
    );
    drop_pr(&env, &env.bob, dropped, "acme/widgets#9")
        .await
        .unwrap();
    for item in [closed, dropped] {
        name_branch(&env, &env.bob, item, "bob/named").await;
        assert_eq!(
            branch_of(&env, item).await,
            Some(("bob/named".to_owned(), BranchSource::Named))
        );
    }
}

#[tokio::test]
async fn a_second_head_dropped_while_the_first_holds_is_recorded_by_its_next_state_report() {
    use crate::use_cases::{PrStateReport, ReportPrState};

    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    review_head(&env, &env.bob, item, "acme/widgets#7", true, "bob/42-first").await;
    assert_eq!(
        branch_of(&env, item).await,
        from_pr("bob/42-first", "acme/widgets#7")
    );

    let before = env.events().await.len();
    let second = review_head(
        &env,
        &env.bob,
        item,
        "acme/widgets#8",
        true,
        "bob/42-second",
    )
    .await;
    let repo = dibs_domain::repo::RepoId::implicit();
    assert_eq!(
        second.claim.claim.branch(&repo).map(ToString::to_string),
        Some("bob/42-first".to_owned()),
        "the answer shows the second head was not recorded"
    );
    assert!(recordings(&env, before).await.is_empty());

    assert_eq!(
        report(&env, "acme/widgets#7", merged()).await,
        PrReportOutcome::StillInReview(item)
    );
    let claim = stored(&env, item).await.unwrap();
    assert!(claim.state.records(&pr("acme/widgets#8")));
    assert!(!claim.state.records(&pr("acme/widgets#7")));

    let merged_at = env.events().await.len();
    ReportPrState::new(&env.deps)
        .execute(
            &env.importer,
            PrStateReport {
                pr: pr("acme/widgets#8"),
                mergeable: true,
                head: Some(BranchName::parse("bob/42-second").unwrap()),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        branch_of(&env, item).await,
        from_pr("bob/42-second", "acme/widgets#8"),
        "the next state report carrying the second head records it"
    );
    assert_eq!(
        recordings(&env, merged_at).await,
        [(
            "bob/42-second".to_owned(),
            dibs_domain::radar::BranchSourceKind::PullRequest
        )]
    );
}
