//! A claim's conversation over the in-memory store: everything dibs writes about one claim lands in
//! one conversation addressed to its holder alone, and another claim on the same item, by a
//! take-over or after the first ended, has a conversation of its own.

use time::Duration;

use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::import::{ImportBatch, Listing, ReportedItem};
use dibs_domain::item::{ExternalRef, Priority, SourceId, Title};
use dibs_domain::message::{Author, Body, Importance, Recipients};
use dibs_domain::review::PrRef;

use super::*;
use crate::records::{MessageRecord, ThreadRecord};
use crate::use_cases::testing::{Env, Person};
use crate::use_cases::{
    ClaimItem, ClaimItemInput, Complete, DeleteThread, ForceRelease, Grant, GrantInput,
    ListThreads, PrReport, Prune, ReadThread, ReportPr, ReportedEnd, StartReview, StartReviewInput,
    TakeOver, TakeOverInput, UpsertItems,
};

const CLAIM_42: &str = "Your claim on gh:acme/widgets#42 \"Work on gh:acme/widgets#42\"";

#[test]
fn the_subject_names_the_item_by_its_reference_and_title() {
    assert_eq!(
        claim_subject("gh:acme/widgets#42", "Refund rounding")
            .unwrap()
            .as_str(),
        "Your claim on gh:acme/widgets#42 \"Refund rounding\""
    );
    assert_eq!(
        claim_subject("#7", "Refund rounding").unwrap().as_str(),
        "Your claim on #7 \"Refund rounding\""
    );
}

#[test]
fn a_long_subject_is_cut_to_fit_at_a_character_boundary() {
    let subject = claim_subject("#7", &"ж".repeat(300)).unwrap();
    assert_eq!(subject.as_str().chars().count(), 200);
    assert!(subject.as_str().starts_with("Your claim on #7 \"ж"));
    assert!(subject.as_str().ends_with('ж'));
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

async fn review(env: &Env, who: &Person, item: ItemId, pr: &str) {
    StartReview::new(&env.deps)
        .execute(
            &who.actor,
            StartReviewInput {
                head: None,
                item,
                pr: PrRef::parse(pr).unwrap(),
                more: false,
            },
        )
        .await
        .unwrap();
}

/// An integration reports the pull request closed without being merged.
async fn closed(env: &Env, pr: &str) {
    ReportPr::new(&env.deps)
        .execute(
            &env.importer,
            PrReport {
                pr: PrRef::parse(pr).unwrap(),
                end: ReportedEnd::Closed,
            },
        )
        .await
        .unwrap();
}

/// `who` records a pull request on their claim and an integration reports it closed, so dibs
/// tells them the claim is back from review.
async fn back_from_review(env: &Env, who: &Person, item: ItemId, pr: &str) {
    review(env, who, item, pr).await;
    closed(env, pr).await;
}

/// An item the importer lists from its tracker: one that a claim may bring back after it was
/// completed.
async fn imported(env: &Env, reference: &str) -> ItemId {
    let listed = ReportedItem {
        reference: ExternalRef::parse(reference).unwrap(),
        title: Title::parse(&format!("Work on {reference}")).unwrap(),
        closed: false,
        listing: Some(Listing {
            areas_hint: Vec::new(),
            priority: Priority::default(),
            source_status: None,
            blocked_by: Vec::new(),
        }),
    };
    let batch =
        ImportBatch::new(Some(SourceId::parse("board").unwrap()), false, vec![listed]).unwrap();
    UpsertItems::new(&env.deps)
        .execute(&env.importer, batch)
        .await
        .unwrap()
        .items[0]
        .change
        .id()
        .unwrap()
}

/// The conversation and its messages, as `who` reads them.
async fn read(env: &Env, who: &Person, thread: ThreadId) -> (ThreadRecord, Vec<MessageRecord>) {
    let view = ReadThread::new(&env.deps)
        .execute(&who.actor, thread)
        .await
        .unwrap();
    (
        view.thread.record,
        view.messages
            .into_iter()
            .map(|message| message.record)
            .collect(),
    )
}

/// The conversations addressed to `who` alone, oldest first.
async fn own(env: &Env, who: &Person) -> Vec<ThreadId> {
    let alone = Recipients::Members([who.actor.member].into());
    let mut threads: Vec<ThreadId> = ListThreads::new(&env.deps)
        .execute(&who.actor, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|summary| summary.thread.record.recipients == alone)
        .map(|summary| summary.thread.record.id)
        .collect();
    threads.sort();
    threads
}

/// Asserts that the conversation is a claim's: on the item, started by dibs, addressed to `holder`
/// alone, every message of it written by dibs under its label.
fn assert_claims(thread: &ThreadRecord, messages: &[MessageRecord], item: ItemId, holder: &Person) {
    assert_eq!(thread.item, Some(item), "filed under the claim's item");
    assert_eq!(thread.author, Author::System, "started by dibs");
    assert_eq!(
        thread.recipients,
        Recipients::Members([holder.actor.member].into()),
        "addressed to the holder alone"
    );
    for message in messages {
        assert_eq!(message.author, Author::System);
        assert_eq!(message.label.as_str(), "dibs");
        assert!(!message.ack_required);
    }
}

#[tokio::test]
async fn two_notices_about_one_claim_are_one_conversation_addressed_to_its_holder_alone() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"]).await;
    review(&env, &env.bob, item, "acme/widgets#7").await;
    review(&env, &env.bob, item, "acme/widgets#8").await;
    closed(&env, "acme/widgets#8").await;
    closed(&env, "acme/widgets#7").await;

    let threads = own(&env, &env.bob).await;
    assert_eq!(threads.len(), 1, "one conversation for the claim");
    let (thread, messages) = read(&env, &env.bob, threads[0]).await;
    assert_eq!(thread.subject.as_str(), CLAIM_42);
    assert_claims(&thread, &messages, item, &env.bob);
    let openings: Vec<&str> = messages
        .iter()
        .map(|message| message.body.as_str().split(':').next().unwrap())
        .collect();
    assert_eq!(
        openings,
        ["Set aside in review", "Back from review"],
        "each message says first what happened"
    );
    assert!(
        messages
            .iter()
            .all(|message| message.importance == Importance::Normal)
    );
}

#[tokio::test]
async fn a_claim_made_again_after_the_first_was_completed_has_a_conversation_of_its_own() {
    let env = Env::new().await;
    let item = imported(&env, "gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    back_from_review(&env, &env.bob, item, "acme/widgets#7").await;
    Complete::new(&env.deps)
        .execute(&env.bob.actor, item)
        .await
        .unwrap();
    env.advance(Duration::hours(1));
    claim(&env, &env.bob, item, &[]).await;
    back_from_review(&env, &env.bob, item, "acme/widgets#8").await;

    let threads = own(&env, &env.bob).await;
    assert_eq!(threads.len(), 2, "one conversation per claim");
    for thread in threads {
        let (record, messages) = read(&env, &env.bob, thread).await;
        assert_eq!(record.subject.as_str(), CLAIM_42);
        assert_eq!(messages.len(), 1);
    }
}

#[tokio::test]
async fn a_takeover_is_told_in_the_previous_claims_conversation_and_the_new_claim_has_its_own() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    back_from_review(&env, &env.bob, item, "acme/widgets#7").await;
    let bobs = own(&env, &env.bob).await;
    assert_eq!(bobs.len(), 1);
    env.advance(Duration::hours(25));
    let taken = TakeOver::new(&env.deps)
        .execute(
            &env.alice.actor,
            TakeOverInput {
                item,
                session: env.alice.session,
                areas: Vec::new(),
                reason: Body::parse("Finishing the refund fix.").unwrap(),
                kind: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        taken.thread, bobs[0],
        "the conversation an earlier notice started"
    );
    let (thread, messages) = read(&env, &env.bob, bobs[0]).await;
    assert_claims(&thread, &messages, item, &env.bob);
    assert_eq!(messages.len(), 2);
    assert!(
        messages[1]
            .body
            .as_str()
            .starts_with("alice took over your stale claim"),
        "{}",
        messages[1].body.as_str()
    );
    assert_eq!(messages[1].importance, Importance::High);

    back_from_review(&env, &env.alice, item, "acme/widgets#8").await;
    let alices = own(&env, &env.alice).await;
    assert_eq!(alices.len(), 1, "the new claim's conversation");
    assert_ne!(alices[0], bobs[0]);
    let (thread, messages) = read(&env, &env.alice, alices[0]).await;
    assert_claims(&thread, &messages, item, &env.alice);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        read(&env, &env.bob, bobs[0]).await.1.len(),
        2,
        "nothing about the new claim reaches the previous holder"
    );
}

#[tokio::test]
async fn a_takeover_of_a_claim_dibs_never_wrote_about_starts_its_conversation() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    env.advance(Duration::hours(25));
    let taken = TakeOver::new(&env.deps)
        .execute(
            &env.alice.actor,
            TakeOverInput {
                item,
                session: env.alice.session,
                areas: Vec::new(),
                reason: Body::parse("Finishing the refund fix.").unwrap(),
                kind: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(own(&env, &env.bob).await, [taken.thread]);
    let (thread, messages) = read(&env, &env.bob, taken.thread).await;
    assert_eq!(thread.subject.as_str(), CLAIM_42);
    assert_claims(&thread, &messages, item, &env.bob);
}

#[tokio::test]
async fn a_conversation_an_admin_deleted_is_started_again_at_the_next_notice() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    review(&env, &env.bob, item, "acme/widgets#7").await;
    review(&env, &env.bob, item, "acme/widgets#8").await;
    closed(&env, "acme/widgets#8").await;
    let first = own(&env, &env.bob).await;
    assert_eq!(first.len(), 1);
    DeleteThread::new(&env.deps)
        .execute(&env.alice.actor, first[0])
        .await
        .unwrap();
    closed(&env, "acme/widgets#7").await;
    let again = own(&env, &env.bob).await;
    assert_eq!(again.len(), 1, "a new conversation");
    assert_ne!(again[0], first[0]);
    let (thread, messages) = read(&env, &env.bob, again[0]).await;
    assert_claims(&thread, &messages, item, &env.bob);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].body.as_str().starts_with("Back from review: "));
}

#[tokio::test]
async fn an_admin_ending_a_claim_tells_its_holder_in_its_conversation() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    back_from_review(&env, &env.bob, item, "acme/widgets#7").await;
    ForceRelease::new(&env.deps)
        .execute(&env.alice.actor, item)
        .await
        .unwrap();
    let threads = own(&env, &env.bob).await;
    assert_eq!(threads.len(), 1, "the claim's conversation");
    let (thread, messages) = read(&env, &env.bob, threads[0]).await;
    assert_claims(&thread, &messages, item, &env.bob);
    assert_eq!(messages.len(), 2);
    let told = &messages[1];
    assert!(
        told.body.as_str().starts_with(
            "Released: alice, an admin, ended your claim on work item gh:acme/widgets#42"
        ),
        "{}",
        told.body.as_str()
    );
    assert!(
        told.body.as_str().contains("The item is open again"),
        "{}",
        told.body.as_str()
    );
    assert!(
        told.body
            .as_str()
            .contains("claim it again (dibs claim gh:acme/widgets#42)"),
        "{}",
        told.body.as_str()
    );
    assert_eq!(told.importance, Importance::High);
}

#[tokio::test]
async fn a_grant_lands_in_the_grantees_claim_conversation() {
    let env = Env::new().await;
    let alices = env.item("gh:acme/widgets#41").await;
    let bobs = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.alice, alices, &["billing"]).await;
    claim(&env, &env.bob, bobs, &[]).await;
    back_from_review(&env, &env.bob, bobs, "acme/widgets#7").await;
    Grant::new(&env.deps)
        .execute(
            &env.alice.actor,
            GrantInput {
                item: bobs,
                area: "billing".to_owned(),
                to: Handle::parse("bob").unwrap(),
            },
        )
        .await
        .unwrap();
    let threads = own(&env, &env.bob).await;
    assert_eq!(threads.len(), 1, "the grantee's claim conversation");
    let (thread, messages) = read(&env, &env.bob, threads[0]).await;
    assert_claims(&thread, &messages, bobs, &env.bob);
    assert_eq!(messages.len(), 2);
    let granted = messages[1].body.as_str();
    assert!(
        granted.starts_with("alice granted you default:billing"),
        "{granted}"
    );
    assert!(
        granted.contains("To write to alice: dibs send alice."),
        "{granted}"
    );
    assert_eq!(messages[1].importance, Importance::High);
}

#[tokio::test]
async fn a_pruned_claim_conversation_goes_and_the_next_claims_notice_starts_another() {
    let env = Env::new().await;
    let item = imported(&env, "gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &[]).await;
    back_from_review(&env, &env.bob, item, "acme/widgets#7").await;
    let first = own(&env, &env.bob).await;
    assert_eq!(first.len(), 1);
    Complete::new(&env.deps)
        .execute(&env.bob.actor, item)
        .await
        .unwrap();
    env.advance(Duration::days(181));
    let report = Prune::new(&env.deps).execute().await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(
        report.pruned[0].1.threads, 1,
        "the done item's conversation"
    );
    assert!(own(&env, &env.bob).await.is_empty());

    claim(&env, &env.bob, item, &[]).await;
    back_from_review(&env, &env.bob, item, "acme/widgets#8").await;
    let next = own(&env, &env.bob).await;
    assert_eq!(next.len(), 1);
    assert_ne!(next[0], first[0]);
    assert_eq!(read(&env, &env.bob, next[0]).await.1.len(), 1);
}
