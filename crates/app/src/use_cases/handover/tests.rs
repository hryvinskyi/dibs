//! Taking an area from debt work over the in-memory store: a feature claim waits for an area a debt
//! claim holds, the debt holder is asked, the area moves in the transaction that lets it go, and the
//! sweep hands over from stale claims and escalates what nobody answered.

use time::Duration;

use dibs_domain::area::QualifiedArea;
use dibs_domain::event::{AreaPassing, ClaimNarrowed, EventKind};
use dibs_domain::handover::ClaimKind;
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::message::{Author, Importance, Recipients};
use dibs_domain::review::PrRef;

use super::*;
use crate::records::{MessageRecord, ThreadRecord};
use crate::use_cases::testing::{Env, Person};
use crate::use_cases::{
    ClaimItem, ClaimItemInput, ClaimMade, Complete, DropPr, DropPrInput, Release, StartReview,
    StartReviewInput, TakeOver, TakeOverInput,
};

const SETTINGS: &str = r#"{
    "stale_after": "24h",
    "arbiters": ["alice"],
    "areas": [
        {"name": "billing", "paths": ["packages/billing/**"]},
        {"name": "search", "paths": ["packages/search/**"]}
    ]
}"#;

fn area(text: &str) -> QualifiedArea {
    QualifiedArea::parse(text).unwrap()
}

async fn claim(
    env: &Env,
    who: &Person,
    item: ItemId,
    areas: &[&str],
    kind: Option<ClaimKind>,
) -> Result<ClaimMade, AppError> {
    ClaimItem::new(&env.deps)
        .execute(
            &who.actor,
            ClaimItemInput {
                item,
                session: who.session,
                areas: areas.iter().map(|area| (*area).to_owned()).collect(),
                wide: None,
                kind,
            },
        )
        .await
}

/// Alice's debt claim on `gh:acme/widgets#7` holds billing and search; answers its item and the
/// item of bob's feature work, `gh:acme/widgets#42`, not claimed yet.
async fn alice_holds_debt(env: &Env) -> (ItemId, ItemId) {
    let debt = env.item("gh:acme/widgets#7").await;
    let feature = env.item("gh:acme/widgets#42").await;
    claim(
        env,
        &env.alice,
        debt,
        &["billing", "search"],
        Some(ClaimKind::Debt),
    )
    .await
    .unwrap();
    (debt, feature)
}

async fn narrow(env: &Env, who: &Person, item: ItemId, areas: &[&str]) -> NarrowedClaim {
    NarrowClaim::new(&env.deps)
        .execute(
            &who.actor,
            NarrowClaimInput {
                item,
                areas: areas.iter().map(|area| (*area).to_owned()).collect(),
            },
        )
        .await
        .unwrap()
}

async fn stored(env: &Env, item: ItemId) -> Option<dibs_domain::claim::Claim> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.claim(env.project, item).await.unwrap()
}

/// Every conversation the events say was started after the first `skip` events, with its messages.
async fn threads_after(env: &Env, skip: usize) -> Vec<(ThreadRecord, Vec<MessageRecord>)> {
    let started: Vec<ThreadId> = env
        .kinds_after(skip)
        .await
        .into_iter()
        .filter_map(|kind| match kind {
            EventKind::ThreadStarted(started) => Some(started.thread),
            _ => None,
        })
        .collect();
    let mut tx = env.deps.store.begin().await.unwrap();
    let mut threads = Vec::new();
    for id in started {
        let thread = tx.thread(env.project, id).await.unwrap().unwrap();
        let messages = tx.messages(env.project, id).await.unwrap();
        threads.push((thread, messages));
    }
    threads
}

fn passing(text: &str, from: ItemId, to: ItemId) -> AreaPassing {
    AreaPassing {
        area: area(text),
        from,
        to,
    }
}

fn sweep(env: &Env) -> TickHandovers {
    TickHandovers::new(&env.deps)
}

#[tokio::test]
async fn a_feature_claim_waits_for_the_debt_held_area_and_the_holder_is_asked_urgently() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    let before = env.events().await.len();
    let made = claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    let held = made.outcome.claim();
    assert_eq!(held.kind, ClaimKind::Feature);
    assert!(held.areas.is_empty());
    assert_eq!(held.pending[&area("default:billing")].from, debt);
    assert_eq!(
        held.pending[&area("default:billing")].asked_at,
        Some(env.now()),
        "the answer shows the request as sent"
    );
    assert_eq!(made.pending.len(), 1);
    assert_eq!(made.pending[0].holder.as_str(), "alice");
    assert!(made.pending[0].asked);
    assert!(
        env.kinds_after(before)
            .await
            .contains(&EventKind::YieldRequested(passing(
                "default:billing",
                debt,
                feature
            )))
    );
    let threads = threads_after(&env, before).await;
    assert_eq!(threads.len(), 1, "{threads:?}");
    let (thread, messages) = &threads[0];
    assert_eq!(thread.item, Some(debt));
    assert_eq!(thread.author, Author::Member(env.bob.actor.member));
    assert_eq!(
        thread.recipients,
        Recipients::Members([env.alice.actor.member].into())
    );
    assert!(
        thread
            .subject
            .as_str()
            .starts_with("Yield default:billing to gh:acme/widgets#42, priority 2"),
        "{}",
        thread.subject.as_str()
    );
    let message = &messages[0];
    assert_eq!(message.author, Author::System);
    assert_eq!(message.importance, Importance::Urgent);
    assert!(message.ack_required);
    assert!(
        message
            .body
            .as_str()
            .contains(&format!("dibs claim narrow {debt} --area default:billing")),
        "{}",
        message.body.as_str()
    );
    assert!(
        claim(&env, &env.bob, feature, &["billing"], None)
            .await
            .is_ok(),
        "claiming again asks nothing again"
    );
    assert_eq!(threads_after(&env, before).await.len(), 1);
}

#[tokio::test]
async fn the_claim_is_atomic_and_an_area_waits_for_one_claim() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    let other = env.item("gh:acme/widgets#43").await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    let error = claim(&env, &env.bob, other, &["billing"], None)
        .await
        .unwrap_err();
    assert_eq!(error.code(), "area_held");
    assert_eq!(stored(&env, other).await, None);
    let third = env.item("gh:acme/widgets#46").await;
    let refused = claim(&env, &env.bob, third, &["search", "billing"], None)
        .await
        .unwrap_err();
    assert_eq!(
        refused.code(),
        "area_held",
        "billing waits for another claim, so the whole claim is refused"
    );
    assert_eq!(stored(&env, third).await, None);
    let alices = stored(&env, debt).await.unwrap();
    assert!(alices.holds(&area("default:search")), "nothing was taken");
}

#[tokio::test]
async fn narrowing_hands_the_area_over_in_the_same_transaction_and_tells_the_feature_holder() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    let before = env.events().await.len();
    let narrowed = narrow(&env, &env.alice, debt, &["billing"]).await;
    assert_eq!(narrowed.areas, [area("default:billing")].into());
    assert_eq!(narrowed.claim.claim.areas, [area("default:search")].into());
    assert_eq!(narrowed.handed_over.len(), 1);
    assert_eq!(narrowed.handed_over[0].holder.as_str(), "bob");
    assert_eq!(narrowed.handed_over[0].to.id, feature);
    let kinds = env.kinds_after(before).await;
    assert_eq!(
        kinds[0],
        EventKind::ClaimNarrowed(ClaimNarrowed {
            item: debt,
            member: env.alice.actor.member,
            areas: [area("default:billing")].into(),
        })
    );
    assert!(kinds.contains(&EventKind::AreaHandedOver(passing(
        "default:billing",
        debt,
        feature
    ))));
    let bobs = stored(&env, feature).await.unwrap();
    assert!(bobs.holds(&area("default:billing")));
    assert!(bobs.pending.is_empty());
    let threads = threads_after(&env, before).await;
    let (thread, messages) = &threads[0];
    assert_eq!(
        thread.recipients,
        Recipients::Members([env.bob.actor.member].into())
    );
    assert_eq!(thread.subject.as_str(), "Handed over: default:billing");
    assert_eq!(messages[0].importance, Importance::High);
    let again = env.events().await.len();
    let repeated = narrow(&env, &env.alice, debt, &["billing"]).await;
    assert!(repeated.areas.is_empty() && repeated.handed_over.is_empty());
    assert_eq!(env.events().await.len(), again, "a repeat changes nothing");
}

#[tokio::test]
async fn completing_or_releasing_the_debt_claim_hands_its_waited_for_areas_over() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing", "search"], None)
        .await
        .unwrap();
    Complete::new(&env.deps)
        .execute(&env.alice.actor, debt)
        .await
        .unwrap();
    let bobs = stored(&env, feature).await.unwrap();
    assert_eq!(
        bobs.areas,
        [area("default:billing"), area("default:search")].into()
    );

    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["search"], None)
        .await
        .unwrap();
    Release::new(&env.deps)
        .execute(&env.alice.actor, debt)
        .await
        .unwrap();
    assert!(
        stored(&env, feature)
            .await
            .unwrap()
            .holds(&area("default:search"))
    );
}

#[tokio::test]
async fn a_debt_claim_in_review_is_asked_only_once_it_is_worked_on_again() {
    let pr = PrRef::parse("acme/widgets#9").unwrap();
    let env = Env::with_settings(
        r#"{
            "stale_after": "24h",
            "arbiters": ["alice"],
            "repos": [{"id": "widgets", "remote": "github.com/acme/widgets",
                "areas": [{"name": "billing", "paths": ["packages/billing/**"]},
                          {"name": "search", "paths": ["packages/search/**"]}]}]
        }"#,
    )
    .await;
    let (debt, feature) = alice_holds_debt(&env).await;
    StartReview::new(&env.deps)
        .execute(
            &env.alice.actor,
            StartReviewInput {
                item: debt,
                pr: pr.clone(),
                more: false,
            },
        )
        .await
        .unwrap();
    let before = env.events().await.len();
    let made = claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    assert!(!made.pending[0].asked, "not asked while in review");
    assert!(threads_after(&env, before).await.is_empty());
    env.advance(Duration::hours(2));
    let swept = sweep(&env).execute().await.unwrap();
    assert_eq!((swept.asked, swept.escalated), (0, 0));
    let back = env.events().await.len();
    DropPr::new(&env.deps)
        .execute(&env.alice.actor, DropPrInput { item: debt, pr })
        .await
        .unwrap();
    assert!(
        env.kinds_after(back)
            .await
            .contains(&EventKind::YieldRequested(passing(
                "widgets:billing",
                debt,
                feature
            ))),
        "asked once worked on again"
    );
}

#[tokio::test]
async fn the_sweep_escalates_an_unanswered_request_at_thirty_minutes_and_not_before() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    env.advance(Duration::minutes(29));
    let before = env.events().await.len();
    let early = sweep(&env).execute().await.unwrap();
    assert_eq!((early.projects, early.escalated), (1, 0));
    assert_eq!(
        env.events().await.len(),
        before,
        "not before thirty minutes"
    );
    env.advance(Duration::minutes(1));
    let due = sweep(&env).execute().await.unwrap();
    assert_eq!(due.escalated, 1);
    assert!(due.failed.is_empty(), "{:?}", due.failed);
    let kinds = env.kinds_after(before).await;
    assert!(
        kinds.iter().any(|kind| matches!(
            kind,
            EventKind::Escalated(escalated)
                if escalated.by == env.bob.actor.member
                    && escalated.subject == dibs_domain::escalation::EscalationSubject::Item(debt)
        )),
        "{kinds:?}"
    );
    let threads = threads_after(&env, before).await;
    let (thread, messages) = &threads[0];
    assert_eq!(
        thread.recipients,
        Recipients::Members([env.alice.actor.member].into())
    );
    assert_eq!(messages[0].importance, Importance::Urgent);
    assert!(messages[0].body.as_str().contains("default:billing"));
    env.advance(Duration::hours(1));
    let after = env.events().await.len();
    assert_eq!(sweep(&env).execute().await.unwrap().escalated, 0, "once");
    assert_eq!(env.events().await.len(), after);
}

#[tokio::test]
async fn the_sweep_hands_over_from_a_debt_claim_gone_stale() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    env.advance(Duration::hours(23));
    claim(&env, &env.bob, feature, &[], None).await.unwrap();
    env.advance(Duration::hours(2));
    let swept = sweep(&env).execute().await.unwrap();
    assert_eq!(swept.handed_over, 1);
    assert!(
        stored(&env, feature)
            .await
            .unwrap()
            .holds(&area("default:billing"))
    );
    assert!(
        !stored(&env, debt)
            .await
            .unwrap()
            .areas
            .contains(&area("default:billing")),
        "the stale debt claim drops what it handed over"
    );
    assert_eq!(sweep(&env).execute().await.unwrap().projects, 0);
}

#[tokio::test]
async fn a_takeover_of_the_debt_claim_hands_the_area_to_the_claim_waiting_for_it() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    env.advance(Duration::hours(23));
    claim(&env, &env.bob, feature, &[], None).await.unwrap();
    env.advance(Duration::hours(2));
    let taken = TakeOver::new(&env.deps)
        .execute(
            &env.bob.actor,
            TakeOverInput {
                item: debt,
                session: env.bob.session,
                areas: Vec::new(),
                reason: dibs_domain::message::Body::parse("Finishing the clean-up.").unwrap(),
                kind: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(taken.takeover.claim.kind, ClaimKind::Debt);
    assert!(
        !stored(&env, debt)
            .await
            .unwrap()
            .holds(&area("default:billing"))
    );
    assert!(
        stored(&env, feature)
            .await
            .unwrap()
            .holds(&area("default:billing"))
    );
}

#[tokio::test]
async fn only_the_holder_narrows() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, _) = alice_holds_debt(&env).await;
    let refused = NarrowClaim::new(&env.deps)
        .execute(
            &env.bob.actor,
            NarrowClaimInput {
                item: debt,
                areas: vec!["billing".to_owned()],
            },
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "not_holder");
    let unknown = NarrowClaim::new(&env.deps)
        .execute(
            &env.alice.actor,
            NarrowClaimInput {
                item: debt,
                areas: vec!["nowhere".to_owned()],
            },
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.code(), "unknown_area");
    let importer = NarrowClaim::new(&env.deps)
        .execute(
            &env.importer,
            NarrowClaimInput {
                item: debt,
                areas: Vec::new(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(importer.code(), "forbidden");
}

/// One repository the project lists, so that pull requests of it can be recorded.
const REPO_SETTINGS: &str = r#"{
    "stale_after": "24h",
    "arbiters": ["alice"],
    "repos": [{"id": "widgets", "remote": "github.com/acme/widgets",
        "areas": [{"name": "billing", "paths": ["packages/billing/**"]},
                  {"name": "search", "paths": ["packages/search/**"]}]}]
}"#;

async fn messages(env: &Env, thread: ThreadId) -> Vec<MessageRecord> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.messages(env.project, thread).await.unwrap()
}

async fn wait_of(env: &Env, item: ItemId, text: &str) -> dibs_domain::handover::PendingArea {
    stored(env, item).await.unwrap().pending[&area(text)].clone()
}

fn withdrawn(kinds: &[EventKind]) -> Vec<dibs_domain::handover::WithdrawReason> {
    kinds
        .iter()
        .filter_map(|kind| match kind {
            EventKind::WaitWithdrawn(withdrawn) => Some(withdrawn.reason),
            _ => None,
        })
        .collect()
}

async fn review(env: &Env, item: ItemId, text: &str, more: bool) {
    StartReview::new(&env.deps)
        .execute(
            &env.alice.actor,
            StartReviewInput {
                item,
                pr: PrRef::parse(text).unwrap(),
                more,
            },
        )
        .await
        .unwrap();
}

async fn report(env: &Env, text: &str, end: crate::use_cases::ReportedEnd) {
    crate::use_cases::ReportPr::new(&env.deps)
        .execute(
            &env.importer,
            crate::use_cases::PrReport {
                pr: PrRef::parse(text).unwrap(),
                end,
            },
        )
        .await
        .unwrap();
}

fn merged() -> crate::use_cases::ReportedEnd {
    crate::use_cases::ReportedEnd::Merged {
        base: "main".to_owned(),
        merged_at: crate::use_cases::testing::START,
        files: None,
        head: None,
    }
}

#[tokio::test]
async fn a_member_holding_the_area_under_a_feature_claim_too_is_never_taken_from() {
    let env = Env::with_settings(SETTINGS).await;
    let (_, feature) = alice_holds_debt(&env).await;
    let alices_feature = env.item("gh:acme/widgets#8").await;
    claim(&env, &env.alice, alices_feature, &["billing"], None)
        .await
        .unwrap();
    let refused = claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "area_held");
    assert_eq!(stored(&env, feature).await, None);
}

#[tokio::test]
async fn an_area_two_debt_claims_hold_moves_only_when_the_last_lets_go_asked_afresh_between() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    let second = env.item("gh:acme/widgets#9").await;
    claim(
        &env,
        &env.alice,
        second,
        &["billing"],
        Some(ClaimKind::Debt),
    )
    .await
    .unwrap();
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    assert_eq!(wait_of(&env, feature, "default:billing").await.from, debt);
    env.advance(Duration::minutes(5));
    let before = env.events().await.len();
    let narrowed = narrow(&env, &env.alice, debt, &["billing"]).await;
    assert!(narrowed.handed_over.is_empty(), "#9 still holds it");
    let wait = wait_of(&env, feature, "default:billing").await;
    assert_eq!((wait.from, wait.asked_at), (second, Some(env.now())));
    assert!(
        env.kinds_after(before)
            .await
            .contains(&EventKind::YieldRequested(passing(
                "default:billing",
                second,
                feature
            )))
    );
    narrow(&env, &env.alice, second, &["billing"]).await;
    assert!(
        stored(&env, feature)
            .await
            .unwrap()
            .holds(&area("default:billing"))
    );
}

#[tokio::test]
async fn one_pair_of_claims_is_asked_in_one_conversation_that_later_areas_and_a_reclaim_reuse() {
    let env = Env::with_settings(SETTINGS).await;
    let (_, feature) = alice_holds_debt(&env).await;
    let before = env.events().await.len();
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    let thread = wait_of(&env, feature, "default:billing")
        .await
        .request
        .unwrap();
    claim(&env, &env.bob, feature, &["search"], None)
        .await
        .unwrap();
    assert_eq!(
        wait_of(&env, feature, "default:search").await.request,
        Some(thread)
    );
    assert_eq!(
        threads_after(&env, before).await.len(),
        1,
        "one conversation"
    );
    let asked = messages(&env, thread).await;
    assert_eq!(asked.len(), 2);
    assert!(
        asked
            .iter()
            .all(|m| m.ack_required && m.importance == Importance::Urgent)
    );
    assert!(asked[1].body.as_str().contains("default:search"));

    let gave_up = env.events().await.len();
    narrow(&env, &env.bob, feature, &["search"]).await;
    assert_eq!(
        withdrawn(&env.kinds_after(gave_up).await),
        [dibs_domain::handover::WithdrawReason::Narrowed]
    );
    let told = messages(&env, thread).await;
    let last = told.last().unwrap();
    assert_eq!(last.importance, Importance::Normal);
    assert!(!last.ack_required);
    assert!(
        last.body
            .as_str()
            .starts_with("The request for default:search no longer stands"),
        "{}",
        last.body.as_str()
    );
    claim(&env, &env.bob, feature, &["search"], None)
        .await
        .unwrap();
    assert_eq!(
        wait_of(&env, feature, "default:search").await.request,
        Some(thread),
        "a reclaim while the request is open asks in it again"
    );
    assert_eq!(threads_after(&env, before).await.len(), 1);
}

#[tokio::test]
async fn going_into_review_resets_the_request_at_once_and_coming_back_asks_with_a_fresh_clock() {
    let env = Env::with_settings(REPO_SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    let thread = wait_of(&env, feature, "widgets:billing")
        .await
        .request
        .unwrap();
    env.advance(Duration::minutes(10));
    review(&env, debt, "acme/widgets#9", false).await;
    let paused = wait_of(&env, feature, "widgets:billing").await;
    assert_eq!(
        (paused.asked_at, paused.escalated_at),
        (None, None),
        "reset in the transaction that started the review"
    );
    env.advance(Duration::seconds(10));
    let before = env.events().await.len();
    DropPr::new(&env.deps)
        .execute(
            &env.alice.actor,
            DropPrInput {
                item: debt,
                pr: PrRef::parse("acme/widgets#9").unwrap(),
            },
        )
        .await
        .unwrap();
    let back = wait_of(&env, feature, "widgets:billing").await;
    assert_eq!(back.asked_at, Some(env.now()), "a fresh clock");
    assert_eq!(
        back.request,
        Some(thread),
        "asked again in the same conversation"
    );
    assert!(
        env.kinds_after(before)
            .await
            .contains(&EventKind::YieldRequested(passing(
                "widgets:billing",
                debt,
                feature
            )))
    );
}

#[tokio::test]
async fn a_merge_completing_the_debt_claim_hands_over_and_a_close_or_a_merge_with_more_asks_again()
{
    let env = Env::with_settings(REPO_SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    review(&env, debt, "acme/widgets#9", false).await;
    report(
        &env,
        "acme/widgets#9",
        crate::use_cases::ReportedEnd::Closed,
    )
    .await;
    assert!(
        wait_of(&env, feature, "widgets:billing")
            .await
            .asked_at
            .is_some(),
        "closed unmerged: worked on again, so asked again"
    );
    review(&env, debt, "acme/widgets#10", true).await;
    report(&env, "acme/widgets#10", merged()).await;
    assert!(
        wait_of(&env, feature, "widgets:billing")
            .await
            .asked_at
            .is_some(),
        "merged with more to follow: worked on again, so asked again"
    );
    review(&env, debt, "acme/widgets#11", false).await;
    report(&env, "acme/widgets#11", merged()).await;
    assert_eq!(stored(&env, debt).await, None, "completed by the merge");
    assert!(
        stored(&env, feature)
            .await
            .unwrap()
            .holds(&area("widgets:billing"))
    );
}

#[tokio::test]
async fn an_admins_force_release_or_the_removal_of_the_debt_holder_hands_over() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    crate::use_cases::ForceRelease::new(&env.deps)
        .execute(&env.alice.actor, debt)
        .await
        .unwrap();
    assert!(
        stored(&env, feature)
            .await
            .unwrap()
            .holds(&area("default:billing"))
    );

    let env = Env::with_settings(SETTINGS).await;
    let debt = env.item("gh:acme/widgets#7").await;
    let feature = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, debt, &["billing"], Some(ClaimKind::Debt))
        .await
        .unwrap();
    claim(&env, &env.alice, feature, &["billing"], None)
        .await
        .unwrap();
    crate::use_cases::RemoveMember::new(&env.deps)
        .execute(
            &env.alice.actor,
            &dibs_domain::handle::Handle::parse("bob").unwrap(),
        )
        .await
        .unwrap();
    assert!(
        stored(&env, feature)
            .await
            .unwrap()
            .holds(&area("default:billing"))
    );
}

#[tokio::test]
async fn the_end_of_the_waiting_claim_tells_the_debt_holder_the_request_no_longer_stands() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    let thread = wait_of(&env, feature, "default:billing")
        .await
        .request
        .unwrap();
    let before = env.events().await.len();
    crate::use_cases::RemoveMember::new(&env.deps)
        .execute(
            &env.alice.actor,
            &dibs_domain::handle::Handle::parse("bob").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        withdrawn(&env.kinds_after(before).await),
        [dibs_domain::handover::WithdrawReason::Ended]
    );
    assert!(
        messages(&env, thread)
            .await
            .last()
            .unwrap()
            .body
            .as_str()
            .contains("ended")
    );
    assert!(
        stored(&env, debt)
            .await
            .unwrap()
            .holds(&area("default:billing")),
        "the debt claim keeps its area"
    );
}

#[tokio::test]
async fn the_sweep_ends_the_wait_of_a_feature_claim_gone_stale() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    env.advance(Duration::hours(23));
    claim(&env, &env.alice, debt, &[], None).await.unwrap();
    env.advance(Duration::hours(2));
    let before = env.events().await.len();
    let swept = sweep(&env).execute().await.unwrap();
    assert_eq!((swept.withdrawn, swept.handed_over), (1, 0));
    assert_eq!(
        withdrawn(&env.kinds_after(before).await),
        [dibs_domain::handover::WithdrawReason::Stale]
    );
    assert!(stored(&env, feature).await.unwrap().pending.is_empty());
    assert!(
        stored(&env, debt)
            .await
            .unwrap()
            .holds(&area("default:billing"))
    );
}

#[tokio::test]
async fn a_takeover_of_a_stale_feature_claim_inherits_its_waits_unless_it_is_made_as_debt() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    env.advance(Duration::hours(23));
    claim(&env, &env.alice, debt, &[], None).await.unwrap();
    env.advance(Duration::hours(2));
    let carol = crate::use_cases::CreateMember::new(&env.deps)
        .execute(
            &env.alice.actor,
            crate::use_cases::CreateMemberInput {
                member: crate::use_cases::testing::new_member("carol"),
                role: dibs_domain::member::Role::Member,
            },
        )
        .await
        .unwrap()
        .token;
    let carol = crate::use_cases::Authenticate::new(&env.deps)
        .execute(carol.expose())
        .await
        .unwrap();
    let carol_session =
        crate::use_cases::testing::session(&env.deps, &carol, "tower", "tower/1").await;
    let taken = TakeOver::new(&env.deps)
        .execute(
            &carol,
            TakeOverInput {
                item: feature,
                session: carol_session,
                areas: Vec::new(),
                reason: dibs_domain::message::Body::parse("Picking the feature up.").unwrap(),
                kind: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(taken.takeover.claim.kind, ClaimKind::Feature);
    let wait = wait_of(&env, feature, "default:billing").await;
    assert_eq!(wait.from, debt, "the wait carries on");

    let env = Env::with_settings(SETTINGS).await;
    let (_, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    env.advance(Duration::hours(25));
    let before = env.events().await.len();
    let taken = TakeOver::new(&env.deps)
        .execute(
            &env.alice.actor,
            TakeOverInput {
                item: feature,
                session: env.alice.session,
                areas: Vec::new(),
                reason: dibs_domain::message::Body::parse("Folding it into the clean-up.").unwrap(),
                kind: Some(ClaimKind::Debt),
            },
        )
        .await
        .unwrap();
    assert_eq!(taken.takeover.claim.kind, ClaimKind::Debt, "the named kind");
    assert!(stored(&env, feature).await.unwrap().pending.is_empty());
    assert_eq!(
        withdrawn(&env.kinds_after(before).await),
        [dibs_domain::handover::WithdrawReason::Ended]
    );
}

#[tokio::test]
async fn a_renewal_naming_another_kind_is_refused() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, _) = alice_holds_debt(&env).await;
    let refused = claim(&env, &env.alice, debt, &[], Some(ClaimKind::Feature))
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "kind_fixed");
    assert_eq!(
        refused.to_string(),
        "the claim was made as debt; release it and claim again to change it"
    );
    assert!(
        claim(&env, &env.alice, debt, &[], Some(ClaimKind::Debt))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_request_already_escalated_about_the_item_is_added_to_that_escalation() {
    let env = Env::with_settings(
        r#"{"stale_after": "24h", "arbiters": ["alice"],
        "areas": [{"name": "billing", "paths": ["packages/billing/**"]}]}"#,
    )
    .await;
    let debt = env.item("gh:acme/widgets#7").await;
    let feature = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.alice, debt, &["billing"], Some(ClaimKind::Debt))
        .await
        .unwrap();
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    let standing = crate::use_cases::Escalate::new(&env.deps)
        .execute(
            &env.bob.actor,
            crate::use_cases::EscalateInput {
                subject: dibs_domain::escalation::EscalationSubject::Item(debt),
                reason: dibs_domain::escalation::EscalationReason::parse("Who goes first?")
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    env.advance(Duration::minutes(30));
    let before = messages(&env, standing.thread).await.len();
    let swept = sweep(&env).execute().await.unwrap();
    assert_eq!((swept.escalated, swept.unescalated), (1, 0));
    let after = messages(&env, standing.thread).await;
    assert_eq!(after.len(), before + 1);
    assert!(
        after
            .last()
            .unwrap()
            .body
            .as_str()
            .starts_with("Also escalated here:"),
        "{}",
        after.last().unwrap().body.as_str()
    );
}

#[tokio::test]
async fn a_request_no_arbiter_would_receive_is_recorded_and_counted_not_dropped_silently() {
    let env = Env::with_settings(
        r#"{"stale_after": "24h", "arbiters": ["bob"],
        "yield_escalates_after": "10m",
        "areas": [{"name": "billing", "paths": ["packages/billing/**"]},
                  {"name": "search", "paths": ["packages/search/**"]}]}"#,
    )
    .await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    env.advance(Duration::minutes(9));
    assert_eq!(sweep(&env).execute().await.unwrap().unescalated, 0);
    env.advance(Duration::minutes(1));
    let before = env.events().await.len();
    let swept = sweep(&env).execute().await.unwrap();
    assert_eq!((swept.escalated, swept.unescalated), (0, 1));
    assert!(
        env.kinds_after(before)
            .await
            .contains(&EventKind::YieldUnescalated(
                dibs_domain::event::YieldUnescalated {
                    area: area("default:billing"),
                    from: debt,
                    to: feature,
                    reason: dibs_domain::event::UnescalatedReason::NoArbiters,
                }
            ))
    );
    assert_eq!(sweep(&env).execute().await.unwrap().unescalated, 0, "once");
}

#[tokio::test]
async fn a_claim_the_store_finds_waiting_for_an_area_twice_is_refused_as_the_decision_would() {
    let env = Env::with_settings(SETTINGS).await;
    let (debt, feature) = alice_holds_debt(&env).await;
    claim(&env, &env.bob, feature, &["billing"], None)
        .await
        .unwrap();
    let other = env.item("gh:acme/widgets#43").await;
    let mut twice = stored(&env, feature).await.unwrap();
    twice.item = other;
    let mut work = crate::work::Work::begin_for(&*env.deps.store, &env.bob.actor)
        .await
        .unwrap();
    let refused = save_claimed(&mut work, &twice).await.unwrap_err();
    drop(work);
    assert_eq!(refused.code(), "area_held");
    assert!(refused.to_string().starts_with("your claim #"), "{refused}");
    assert!(stored(&env, debt).await.is_some());
}
