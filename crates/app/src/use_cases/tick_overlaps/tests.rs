//! The sweep of the overlaps' orders over the in-memory store: a claim entering review or a
//! priority changing moves which claim lands first, and both holders are told; a change to a tie
//! goes to the arbiters; an order that holds writes nothing; and an overlap stored before orders
//! were is told its first order once.

use time::Duration;

use dibs_domain::escalation::EscalationSubject;
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::item::Priority;
use dibs_domain::member::Role;
use dibs_domain::message::Importance;
use dibs_domain::paths::RelPath;
use dibs_domain::radar::{BranchName, CommitSha, FootprintBase, OrderRule, Overlap, OverlapOrder};
use dibs_domain::review::PrRef;

use super::*;
use crate::records::EscalationRecord;
use crate::use_cases::testing::{Env, Person, START, in_checkout, new_member, session};
use crate::use_cases::{
    Authenticate, ClaimItem, ClaimItemInput, CreateMember, CreateMemberInput, FootprintInput,
    Heartbeat, ListThreads, ReadThread, RecordFootprint, StartReview, StartReviewInput,
};

/// Two areas, with `carol` as the project's arbiter.
const SETTINGS: &str = r#"{
    "arbiters": ["carol"],
    "areas": [
        {"name": "billing", "paths": ["packages/billing/**"]},
        {"name": "search", "paths": ["packages/search/**"]}
    ]
}"#;

/// Claims `item` for `who`, whose checkout says the work is on `branch`.
async fn claim_on(env: &Env, who: &Person, item: ItemId, branch: &str) {
    ClaimItem::new(&env.deps)
        .execute(
            &who.actor,
            ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item,
                session: who.session,
                areas: Vec::new(),
                kind: None,
            },
        )
        .await
        .unwrap();
    Heartbeat::new(&env.deps)
        .execute(
            &who.actor,
            who.session,
            Some(&in_checkout('a', Some(branch))),
        )
        .await
        .unwrap();
}

/// What `item`'s `branch` changes against `main`.
async fn report(env: &Env, item: ItemId, branch: &str, paths: &[&str]) {
    RecordFootprint::new(&env.deps)
        .execute(
            &env.importer,
            FootprintInput {
                item,
                repo: "default".to_owned(),
                branch: BranchName::parse(branch).unwrap(),
                base: FootprintBase {
                    branch: BranchName::parse("main").unwrap(),
                    sha: CommitSha::parse("abcd").unwrap(),
                    at: START,
                },
                paths: paths
                    .iter()
                    .map(|text| RelPath::parse(text).unwrap())
                    .collect(),
                truncated: false,
            },
        )
        .await
        .unwrap();
}

/// A member `handle` of the project, with a session of their own.
async fn person(env: &Env, handle: &str) -> Person {
    let token = CreateMember::new(&env.deps)
        .execute(
            &env.alice.actor,
            CreateMemberInput {
                member: new_member(handle),
                role: Role::Member,
            },
        )
        .await
        .unwrap()
        .token;
    let actor = Authenticate::new(&env.deps)
        .execute(token.expose())
        .await
        .unwrap();
    let session = session(&env.deps, &actor, "box", &format!("box/{handle}")).await;
    Person { actor, session }
}

/// Gives `item` this priority, as an import would.
async fn prioritize(env: &Env, item: ItemId, priority: u8) {
    let mut tx = env.deps.store.begin().await.unwrap();
    let mut record = tx.item(env.project, item).await.unwrap().unwrap();
    record.priority = Priority::new(priority).unwrap();
    tx.update_item(env.project, &record).await.unwrap();
    tx.commit().await.unwrap();
}

async fn overlaps(env: &Env) -> Vec<Overlap> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.overlaps(env.project).await.unwrap()
}

async fn standing_escalations(env: &Env) -> Vec<EscalationRecord> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.open_escalations(env.project).await.unwrap()
}

/// Every message about an overlap or its order `who` can read, without the note on quoted titles,
/// with its importance, by conversation and then oldest first.
async fn told(env: &Env, who: &Person) -> Vec<(String, Importance)> {
    let mut threads: Vec<ThreadId> = ListThreads::new(&env.deps)
        .execute(&who.actor, None)
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.thread.record.id)
        .collect();
    threads.sort();
    let mut told = Vec::new();
    for id in threads {
        let thread = ReadThread::new(&env.deps)
            .execute(&who.actor, id)
            .await
            .unwrap();
        for message in thread.messages {
            let body = message.record.body.to_string();
            let text = body.lines().next().unwrap_or_default().to_owned();
            if text.starts_with("Overlap: ") || text.starts_with("The order changed: ") {
                told.push((text, message.record.importance));
            }
        }
    }
    told
}

async fn tick(env: &Env) -> OverlapSweep {
    let swept = TickOverlaps::new(&env.deps).execute().await.unwrap();
    assert!(swept.failed.is_empty(), "{:?}", swept.failed);
    swept
}

/// alice claims `gh:acme/widgets#1`, and `between` later bob claims `gh:acme/widgets#2`; both
/// branches change `config/app.yaml`.
async fn colliding(env: &Env, between: Duration) -> (ItemId, ItemId) {
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(env, &env.alice, alices, "alice/1-x").await;
    env.advance(between);
    claim_on(env, &env.bob, bobs, "bob/2-y").await;
    report(env, alices, "alice/1-x", &["config/app.yaml", "a.rs"]).await;
    report(env, bobs, "bob/2-y", &["config/app.yaml"]).await;
    (alices, bobs)
}

#[tokio::test]
async fn a_claim_entering_review_takes_the_lead_and_both_holders_are_told_the_order_changed() {
    let env = Env::with_settings(SETTINGS).await;
    let (alices, bobs) = colliding(&env, Duration::minutes(5)).await;
    assert_eq!(
        overlaps(&env).await[0].order(),
        Some(&OverlapOrder::First {
            item: alices,
            because: OrderRule::Older
        })
    );
    StartReview::new(&env.deps)
        .execute(
            &env.bob.actor,
            StartReviewInput {
                head: None,
                item: bobs,
                pr: PrRef::parse("acme/widgets#12").unwrap(),
                more: false,
            },
        )
        .await
        .unwrap();

    let swept = tick(&env).await;
    assert_eq!(swept.reordered, 1);
    assert_eq!(
        overlaps(&env).await[0].order(),
        Some(&OverlapOrder::First {
            item: bobs,
            because: OrderRule::InReview
        }),
        "the stored order follows the claims"
    );
    let bobs = told(&env, &env.bob).await;
    assert_eq!(bobs.len(), 2, "{bobs:?}");
    assert_eq!(
        bobs[1],
        (
            "The order changed: yours now lands first, because it is in review. Work item gh:acme/widgets#1 rebases onto it once it merges; go on with your work.".to_owned(),
            Importance::High
        )
    );
    let alices = told(&env, &env.alice).await;
    assert_eq!(alices.len(), 2, "{alices:?}");
    assert_eq!(
        alices[1],
        (
            "The order changed: work item gh:acme/widgets#2 now lands first, because it is in review. Once it merges, rebase onto the base and resolve the shared files (default:config/app.yaml), then say in this conversation that you rebased.".to_owned(),
            Importance::High
        )
    );
    assert!(standing_escalations(&env).await.is_empty());
}

#[tokio::test]
async fn a_change_of_reason_alone_is_stored_and_told_to_nobody() {
    let env = Env::with_settings(SETTINGS).await;
    let (alices, _) = colliding(&env, Duration::minutes(5)).await;
    StartReview::new(&env.deps)
        .execute(
            &env.alice.actor,
            StartReviewInput {
                head: None,
                item: alices,
                pr: PrRef::parse("acme/widgets#11").unwrap(),
                more: false,
            },
        )
        .await
        .unwrap();
    let alice_told = told(&env, &env.alice).await;
    let bob_told = told(&env, &env.bob).await;
    let events = env.events().await.len();

    let swept = tick(&env).await;
    assert_eq!(swept.reordered, 1, "the stored order changed");
    assert_eq!(
        overlaps(&env).await[0].order(),
        Some(&OverlapOrder::First {
            item: alices,
            because: OrderRule::InReview
        }),
        "the new reason is stored"
    );
    assert_eq!(
        told(&env, &env.alice).await,
        alice_told,
        "alice still lands first"
    );
    assert_eq!(
        told(&env, &env.bob).await,
        bob_told,
        "bob still lands second"
    );
    assert_eq!(env.events().await.len(), events, "no message is posted");
}

#[tokio::test]
async fn a_change_to_a_tie_goes_to_the_arbiters_once() {
    let env = Env::with_settings(SETTINGS).await;
    person(&env, "carol").await;
    let alices = env.item("gh:acme/widgets#1").await;
    prioritize(&env, alices, 1).await;
    let bobs = env.item("gh:acme/widgets#2").await;
    prioritize(&env, bobs, 3).await;
    claim_on(&env, &env.alice, alices, "alice/1-x").await;
    claim_on(&env, &env.bob, bobs, "bob/2-y").await;
    report(&env, alices, "alice/1-x", &["config/app.yaml"]).await;
    report(&env, bobs, "bob/2-y", &["config/app.yaml"]).await;
    assert_eq!(
        overlaps(&env).await[0].order(),
        Some(&OverlapOrder::First {
            item: alices,
            because: OrderRule::Priority
        })
    );
    assert!(standing_escalations(&env).await.is_empty());
    prioritize(&env, alices, 3).await;

    assert_eq!(tick(&env).await.reordered, 1);
    assert_eq!(overlaps(&env).await[0].order(), Some(&OverlapOrder::Tied));
    let escalations = standing_escalations(&env).await;
    assert_eq!(escalations.len(), 1, "{escalations:?}");
    assert_eq!(escalations[0].subject, EscalationSubject::Item(alices));
    let last = told(&env, &env.alice).await.pop().unwrap();
    assert_eq!(
        last.0,
        "The order changed: the order rule cannot decide whether yours or work item gh:acme/widgets#2 lands first: both are at work, their work items have the same priority and their claims were made at the same time. dibs asked the project's arbiters."
    );

    assert_eq!(tick(&env).await.reordered, 0, "a tie that holds is left");
    assert_eq!(standing_escalations(&env).await.len(), 1);
}

#[tokio::test]
async fn an_order_that_holds_writes_nothing() {
    let env = Env::with_settings(SETTINGS).await;
    colliding(&env, Duration::minutes(5)).await;
    let stored = overlaps(&env).await;
    let events = env.events().await.len();
    let alice_told = told(&env, &env.alice).await.len();

    let swept = tick(&env).await;
    assert_eq!(swept.reordered, 0);
    assert_eq!(swept.projects, 1);
    assert_eq!(env.events().await.len(), events, "nothing is written");
    assert_eq!(overlaps(&env).await, stored);
    assert_eq!(told(&env, &env.alice).await.len(), alice_told);
}

#[tokio::test]
async fn an_overlap_stored_without_an_order_is_told_its_first_order_once() {
    let env = Env::with_settings(SETTINGS).await;
    let (alices, _) = colliding(&env, Duration::minutes(5)).await;
    let unordered = overlaps(&env).await[0].clone().with_order(None).unwrap();
    {
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.save_overlap(env.project, &unordered).await.unwrap();
        tx.commit().await.unwrap();
    }
    assert_eq!(overlaps(&env).await[0].order(), None);

    assert_eq!(tick(&env).await.reordered, 1);
    assert_eq!(
        overlaps(&env).await[0].order(),
        Some(&OverlapOrder::First {
            item: alices,
            because: OrderRule::Older
        })
    );
    let alice_told = told(&env, &env.alice).await;
    assert_eq!(alice_told.len(), 2, "{alice_told:?}");
    assert!(
        alice_told[1].0.starts_with("Overlap: your branch alice/1-x ")
            && alice_told[1]
                .0
                .ends_with("Yours lands first, because its claim is older. Work item gh:acme/widgets#2 rebases onto it once it merges; go on with your work."),
        "a first notice, not a change: {alice_told:?}"
    );
    let bob_told = told(&env, &env.bob).await;
    assert_eq!(bob_told.len(), 2, "{bob_told:?}");
    assert!(
        bob_told[1]
            .0
            .contains("Work item gh:acme/widgets#1 lands first, because its claim is older."),
        "{bob_told:?}"
    );
    assert!(!bob_told[1].0.contains("The order changed"), "{bob_told:?}");

    let events = env.events().await.len();
    assert_eq!(tick(&env).await.reordered, 0);
    assert_eq!(env.events().await.len(), events, "told once");
    assert_eq!(told(&env, &env.alice).await.len(), 2);
}
