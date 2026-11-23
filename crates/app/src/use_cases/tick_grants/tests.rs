//! dibs deciding requests for grants on the holder's behalf over the in-memory store: at the ask,
//! when the holder's claim is in review, the area idle on it or untouched by its branch; in the
//! sweep, once the project's `grant_after` has passed; and never again for a request it settled,
//! a request whose claim ended, or after a revoke between the same two claims. Wide claims'
//! announcements are acknowledged on the holder's behalf by the same test, as they are sent and in
//! the sweep.

use time::{Duration, OffsetDateTime};

use dibs_domain::event::{EventKind, GrantEnd, GrantRevoked, Granted, WideGrantedByAck};
use dibs_domain::grant::{AskedGrant, GrantBlock, GrantDecision};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, MessageId, ThreadId};
use dibs_domain::member::Role;
use dibs_domain::message::{Author, Body, Importance, SessionLabel};
use dibs_domain::review::PrRef;
use dibs_domain::wide::{WideAsk, WideReason};

use super::*;
use crate::records::GrantRecord;
use crate::use_cases::testing::{Env, Person, START, in_checkout, new_member, session};
use crate::use_cases::{
    AckMessage, Announced, AskGrant, AskGrantInput, Authenticate, ClaimItem, ClaimItemInput,
    ClaimMade, Complete, CreateMember, CreateMemberInput, FootprintInput, FootprintRecorded, Grant,
    GrantAsked, GrantInput, GrantedByDibs, Heartbeat, Inbox, InboxQuery, NarrowClaim,
    NarrowClaimInput, PostMessage, PostMessageInput, ReadThread, RecordFootprint, Release,
    RevokeGrant, StartReview, StartReviewInput, WideInput,
};

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

/// `who` asks for edits in billing for their claim on `item`.
async fn ask(env: &Env, who: &Person, item: ItemId) -> GrantAsked {
    AskGrant::new(&env.deps)
        .execute(
            &who.actor,
            AskGrantInput {
                item,
                area: "billing".to_owned(),
            },
        )
        .await
        .unwrap()
        .outcome
}

/// The conversation of a request that is left to wait.
fn waiting(asked: GrantAsked) -> ThreadId {
    match asked {
        GrantAsked::Asked { thread, .. } => thread,
        other => panic!("not left to wait: {other:?}"),
    }
}

/// The grant dibs gave at the ask.
fn given_at_once(asked: GrantAsked) -> GrantedByDibs {
    match asked {
        GrantAsked::Granted {
            by_dibs: Some(given),
            ..
        } => given,
        other => panic!("not granted by dibs: {other:?}"),
    }
}

async fn sweep(env: &Env) -> GrantSweep {
    let swept = TickGrants::new(&env.deps).execute().await.unwrap();
    assert!(swept.failed.is_empty(), "{:?}", swept.failed);
    swept
}

async fn grants(env: &Env) -> Vec<GrantRecord> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.grants(env.project).await.unwrap()
}

/// The requests dibs still decides on their holders' behalf: not settled, not left to their
/// holders.
async fn unsettled(env: &Env) -> Vec<(AskedGrant, OffsetDateTime)> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.unsettled_asked_grants(env.project)
        .await
        .unwrap()
        .into_iter()
        .filter(|(record, _)| !record.left_to_holder)
        .map(|(record, asked_at)| (record.asked, asked_at))
        .collect()
}

/// The texts of the conversation's messages, as `who` reads them.
async fn conversation(env: &Env, who: &Person, thread: ThreadId) -> Vec<(Author, String)> {
    ReadThread::new(&env.deps)
        .execute(&who.actor, thread)
        .await
        .unwrap()
        .messages
        .into_iter()
        .map(|message| {
            (
                message.record.author,
                message.record.body.as_str().to_owned(),
            )
        })
        .collect()
}

async fn review(env: &Env, who: &Person, item: ItemId) {
    StartReview::new(&env.deps)
        .execute(
            &who.actor,
            StartReviewInput {
                head: None,
                item,
                pr: PrRef::parse("acme/widgets#7").unwrap(),
                more: false,
            },
        )
        .await
        .unwrap();
}

/// `who`'s claim on `item` working on `branch` in the checkout `digit`, and the footprint of that
/// branch an integration reports, with `paths` as the paths it changes.
async fn footprint_of(env: &Env, who: &Person, item: ItemId, branch: &str, paths: &[&str]) {
    use dibs_domain::paths::RelPath;
    use dibs_domain::radar::{BranchName, CommitSha, FootprintBase};

    Heartbeat::new(&env.deps)
        .execute(
            &who.actor,
            who.session,
            Some(&in_checkout('b', Some(branch))),
        )
        .await
        .unwrap();
    let recorded = RecordFootprint::new(&env.deps)
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
    assert_eq!(recorded, FootprintRecorded::Stored { changed: true });
}

/// A heartbeat of the person's session reporting an edit outside every area: it keeps their claim
/// live and moves no area's clock.
async fn alive(env: &Env, who: &Person) {
    use dibs_domain::edit::ReportedEdits;

    let outside = ReportedEdits::new(Default::default(), true).unwrap();
    Heartbeat::new(&env.deps)
        .execute_reporting(&who.actor, who.session, None, None, Some(&outside))
        .await
        .unwrap();
}

/// A third member, `carol`, with a session of her own.
async fn carol(env: &Env) -> Person {
    let token = CreateMember::new(&env.deps)
        .execute(
            &env.alice.actor,
            CreateMemberInput {
                member: new_member("carol"),
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
    let session = session(&env.deps, &actor, "tablet", "tablet/0c3e55d1").await;
    Person { actor, session }
}

async fn revoke(env: &Env, who: &Person, grant: &GrantRecord) {
    RevokeGrant::new(&env.deps)
        .execute(&who.actor, grant.id)
        .await
        .unwrap();
}

/// alice holds billing on `#1`; bob claims `#2` with no area.
async fn held() -> (Env, ItemId, ItemId) {
    let env = Env::new().await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim(&env, &env.alice, alices, &["billing"]).await;
    claim(&env, &env.bob, bobs, &[]).await;
    (env, alices, bobs)
}

/// Checks the one grant dibs gave on alice's behalf to bob for `bobs`, that both read why in the
/// request's conversation, and that bob is told in his claim's.
async fn granted_on_alices_behalf(
    env: &Env,
    bobs: ItemId,
    thread: ThreadId,
    because: GrantDecision,
) -> GrantRecord {
    let stored = grants(env).await;
    let [grant] = &stored[..] else {
        panic!("one grant: {stored:?}");
    };
    assert_eq!(grant.grant.item, bobs);
    assert_eq!(
        grant.grant.from, env.alice.actor.member,
        "the holder's grant"
    );
    assert_eq!(grant.grant.to, env.bob.actor.member);
    assert_eq!(grant.decided, because);
    assert!(
        env.events().await.iter().any(|event| event.kind
            == EventKind::Granted(Granted {
                grant: grant.id,
                item: bobs,
                area: grant.grant.area.clone(),
                from: env.alice.actor.member,
                to: env.bob.actor.member,
                decided: because,
            })),
        "granted records who decided"
    );
    let why = because.why().unwrap();
    for who in [&env.alice, &env.bob] {
        let messages = conversation(env, who, thread).await;
        let (author, reply) = messages.last().unwrap();
        assert_eq!(*author, Author::System);
        assert!(
            reply.starts_with(&format!(
                "Granted by dibs: bob may edit default:billing for work item gh:acme/widgets#2 \"Work on gh:acme/widgets#2\", on alice's behalf, because {why}."
            )),
            "{reply}"
        );
        assert!(
            reply.contains(&format!(
                "To undo it, alice or an admin revokes grant {id} (dibs revoke-grant {id}, or DELETE /v1/grants/{id}).",
                id = grant.id
            )),
            "{reply}"
        );
    }
    let mut tx = env.deps.store.begin().await.unwrap();
    let bobs_claim = tx.claim(env.project, bobs).await.unwrap().unwrap();
    let claim_thread = tx
        .claim_thread(env.project, &bobs_claim.instance())
        .await
        .unwrap()
        .expect("bob is told in his claim's conversation")
        .thread;
    drop(tx);
    let notice = conversation(env, &env.bob, claim_thread).await;
    let (_, told) = notice.last().unwrap();
    assert!(
        told.starts_with(&format!(
            "dibs granted you default:billing for your work item gh:acme/widgets#2 (\"Work on gh:acme/widgets#2\") on alice's behalf, because {why}. You may edit there now"
        )),
        "{told}"
    );
    assert!(
        told.contains("unless alice or an admin revokes it. To write to alice: dibs send alice."),
        "{told}"
    );
    assert!(
        unsettled(env).await.is_empty(),
        "dibs decides a request once"
    );
    grant.clone()
}

#[tokio::test]
async fn a_request_to_a_claim_in_review_is_granted_at_once() {
    let (env, alices, bobs) = held().await;
    review(&env, &env.alice, alices).await;

    let given = given_at_once(ask(&env, &env.bob, bobs).await);

    assert_eq!(given.decided, GrantDecision::InReview);
    let grant = granted_on_alices_behalf(&env, bobs, given.thread, GrantDecision::InReview).await;
    assert_eq!(given.grant, grant.id);
}

#[tokio::test]
async fn a_request_for_an_area_idle_on_the_holders_claim_is_granted_at_once() {
    let (env, _, bobs) = held().await;
    for _ in 0..7 {
        env.advance(Duration::days(1));
        alive(&env, &env.alice).await;
        alive(&env, &env.bob).await;
    }
    env.advance(Duration::minutes(1));

    let given = given_at_once(ask(&env, &env.bob, bobs).await);

    assert_eq!(given.decided, GrantDecision::Idle);
    granted_on_alices_behalf(&env, bobs, given.thread, GrantDecision::Idle).await;
}

#[tokio::test]
async fn a_request_for_an_area_the_holders_branch_does_not_touch_is_granted_at_once() {
    let (env, alices, bobs) = held().await;
    footprint_of(
        &env,
        &env.alice,
        alices,
        "alice/1-x",
        &["packages/search/index.rs"],
    )
    .await;

    let given = given_at_once(ask(&env, &env.bob, bobs).await);

    assert_eq!(given.decided, GrantDecision::Untouched);
    granted_on_alices_behalf(&env, bobs, given.thread, GrantDecision::Untouched).await;
}

#[tokio::test]
async fn a_request_for_an_area_the_holders_branch_touches_waits_then_the_sweep_grants_it() {
    let (env, alices, bobs) = held().await;
    footprint_of(
        &env,
        &env.alice,
        alices,
        "alice/1-x",
        &["packages/billing/rates.rs"],
    )
    .await;
    let thread = waiting(ask(&env, &env.bob, bobs).await);
    assert!(grants(&env).await.is_empty());

    env.advance(Duration::minutes(10) - Duration::seconds(1));
    let quiet = env.events().await.len();
    let early = sweep(&env).await;
    assert_eq!((early.granted, early.settled), (0, 0));
    assert_eq!(env.events().await.len(), quiet, "nothing is written");
    assert_eq!(unsettled(&env).await.len(), 1);

    env.advance(Duration::seconds(1));
    let swept = sweep(&env).await;

    assert_eq!((swept.granted, swept.settled), (1, 0));
    granted_on_alices_behalf(&env, bobs, thread, GrantDecision::Waited).await;
    let after = env.events().await.len();
    assert_eq!(sweep(&env).await.granted, 0);
    assert_eq!(env.events().await.len(), after);
}

#[tokio::test]
async fn a_request_with_no_footprint_of_the_holders_claim_waits_for_the_delay() {
    let (env, _, bobs) = held().await;
    let thread = waiting(ask(&env, &env.bob, bobs).await);
    env.advance(Duration::minutes(10));

    assert_eq!(sweep(&env).await.granted, 1);
    granted_on_alices_behalf(&env, bobs, thread, GrantDecision::Waited).await;
}

#[tokio::test]
async fn a_lapsed_request_is_not_granted() {
    let (env, _, bobs) = held().await;
    waiting(ask(&env, &env.bob, bobs).await);
    env.advance(Duration::hours(25));

    let swept = sweep(&env).await;

    assert_eq!((swept.granted, swept.settled), (0, 0));
    assert!(grants(&env).await.is_empty());
    assert_eq!(unsettled(&env).await.len(), 1, "it may wait again later");
}

#[tokio::test]
async fn a_request_the_holder_answered_is_still_granted_once_the_delay_has_passed() {
    let (env, _, bobs) = held().await;
    let thread = waiting(ask(&env, &env.bob, bobs).await);
    PostMessage::new(&env.deps)
        .execute(
            &env.alice.actor,
            PostMessageInput {
                thread,
                label: SessionLabel::parse("laptop/4f2a91c0").unwrap(),
                body: Body::parse("billing is not free yet").unwrap(),
                importance: Importance::Normal,
                ack_required: false,
            },
        )
        .await
        .unwrap();
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.unanswered_asked_grants(env.project, env.alice.actor.member)
            .await
            .unwrap()
            .is_empty(),
        "answered, so out of the holder's brief"
    );
    drop(tx);
    env.advance(Duration::minutes(10));

    assert_eq!(sweep(&env).await.granted, 1);
    granted_on_alices_behalf(&env, bobs, thread, GrantDecision::Waited).await;
}

#[tokio::test]
async fn a_request_a_revoke_stands_against_is_left_to_the_holder_and_never_judged_again() {
    let (env, alices, bobs) = held().await;
    review(&env, &env.alice, alices).await;
    given_at_once(ask(&env, &env.bob, bobs).await);
    let given = grants(&env).await.remove(0);

    revoke(&env, &env.alice, &given).await;

    env.advance(Duration::minutes(10));
    assert_eq!(sweep(&env).await.granted, 0);
    let GrantAsked::Asked {
        thread,
        left_to_holder: true,
        ..
    } = ask(&env, &env.bob, bobs).await
    else {
        panic!("not left to alice");
    };
    assert!(
        unsettled(&env).await.is_empty(),
        "dibs leaves the new request to alice and stops deciding it"
    );
    let inbox = Inbox::new(&env.deps)
        .execute(&env.alice.actor, InboxQuery::default())
        .await
        .unwrap();
    let [pending] = &inbox.grant_asks[..] else {
        panic!("one request in alice's inbox: {:?}", inbox.grant_asks);
    };
    assert!(
        pending.left_to_holder,
        "the pages can say it is left to her"
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    let listed = tx
        .unanswered_asked_grants(env.project, env.alice.actor.member)
        .await
        .unwrap();
    drop(tx);
    let [record] = &listed[..] else {
        panic!("one request for alice: {listed:?}");
    };
    assert!(record.left_to_holder, "alice still decides it");
    let messages = conversation(&env, &env.alice, thread).await;
    let [(author, text)] = &messages[..] else {
        panic!("only the asking message: {messages:?}");
    };
    assert_eq!(*author, Author::Member(env.bob.actor.member));
    assert!(
        text.contains(
            "A revoke of your grant to bob in default:billing stands while both claims last, so dibs leaves this request to you to grant or answer: it does not grant it on your behalf."
        ),
        "{text}"
    );
    assert!(
        !text.contains("have passed since this request"),
        "no grant is promised after the wait: {text}"
    );
    env.advance(Duration::minutes(10));
    let quiet = env.events().await.len();
    let swept = sweep(&env).await;
    assert_eq!(
        (swept.granted, swept.settled, swept.left_to_holders),
        (0, 0, 0)
    );
    assert_eq!(env.events().await.len(), quiet, "the sweep writes nothing");
    assert!(grants(&env).await.is_empty(), "the revoke sticks");
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
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.unanswered_asked_grants(env.project, env.alice.actor.member)
            .await
            .unwrap()
            .is_empty(),
        "alice granted it herself"
    );
    drop(tx);
    let replies = conversation(&env, &env.alice, thread).await;
    assert_eq!(replies.len(), 1, "dibs wrote nothing in it: {replies:?}");
}

#[tokio::test]
async fn the_sweep_leaves_a_request_a_revoke_stands_against_to_the_holder_once() {
    let (env, alices, bobs) = held().await;
    let thread = waiting(ask(&env, &env.bob, bobs).await);
    let mut tx = env.deps.store.begin().await.unwrap();
    let claims = tx.claims(env.project).await.unwrap();
    let claimed_at = |item: ItemId| {
        claims
            .iter()
            .find(|claim| claim.item == item)
            .unwrap()
            .claimed_at
    };
    tx.insert_grant_block(
        env.project,
        &GrantBlock {
            asker: env.bob.actor.member,
            asker_item: bobs,
            asker_claimed_at: claimed_at(bobs),
            holder: env.alice.actor.member,
            holder_item: alices,
            holder_claimed_at: claimed_at(alices),
            area: dibs_domain::area::QualifiedArea::parse("default:billing").unwrap(),
            revoked_at: env.now(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    env.advance(Duration::minutes(10));

    let swept = sweep(&env).await;

    assert_eq!(
        (swept.granted, swept.settled, swept.left_to_holders),
        (0, 0, 1)
    );
    assert!(grants(&env).await.is_empty());
    assert!(
        unsettled(&env).await.is_empty(),
        "left to alice for good, so the sweep never judges it again"
    );
    let quiet = env.events().await.len();
    let again = sweep(&env).await;
    assert_eq!(
        (again.granted, again.settled, again.left_to_holders),
        (0, 0, 0)
    );
    assert_eq!(env.events().await.len(), quiet);
    assert_eq!(
        conversation(&env, &env.alice, thread).await.len(),
        1,
        "dibs wrote nothing in it"
    );
}

#[tokio::test]
async fn a_request_whose_holder_gave_the_area_up_stays_settled_when_they_declare_it_again() {
    let (env, alices, bobs) = held().await;
    waiting(ask(&env, &env.bob, bobs).await);
    NarrowClaim::new(&env.deps)
        .execute(
            &env.alice.actor,
            NarrowClaimInput {
                item: alices,
                areas: vec!["billing".to_owned()],
            },
        )
        .await
        .unwrap();
    assert_eq!(sweep(&env).await.settled, 1);
    for _ in 0..4 {
        env.advance(Duration::hours(12));
        alive(&env, &env.alice).await;
        alive(&env, &env.bob).await;
    }

    alice_claims(&env, alices, &["billing"]).await;
    let quiet = env.events().await.len();
    let swept = sweep(&env).await;

    assert_eq!((swept.granted, swept.settled), (0, 0));
    assert!(grants(&env).await.is_empty(), "nothing is granted");
    assert!(
        unsettled(&env).await.is_empty(),
        "the request stays settled; bob would ask again"
    );
    assert_eq!(env.events().await.len(), quiet);
}

#[tokio::test]
async fn a_grant_settles_only_the_requests_for_its_item() {
    let (env, _, bobs) = held().await;
    let fifth = env.item("gh:acme/widgets#5").await;
    claim(&env, &env.bob, fifth, &[]).await;
    waiting(ask(&env, &env.bob, bobs).await);
    waiting(ask(&env, &env.bob, fifth).await);

    Grant::new(&env.deps)
        .execute(
            &env.alice.actor,
            GrantInput {
                item: fifth,
                area: "billing".to_owned(),
                to: Handle::parse("bob").unwrap(),
            },
        )
        .await
        .unwrap();

    let left: Vec<ItemId> = unsettled(&env)
        .await
        .into_iter()
        .map(|(asked, _)| asked.ask.item)
        .collect();
    assert_eq!(
        left,
        [bobs],
        "the request for #2 is not answered by a grant for #5"
    );
    env.advance(Duration::minutes(10));
    assert_eq!(
        sweep(&env).await.granted,
        0,
        "it lapses while the grant for #5 is in force"
    );
    assert_eq!(unsettled(&env).await.len(), 1);

    Release::new(&env.deps)
        .execute(&env.bob.actor, fifth)
        .await
        .unwrap();
    let swept = sweep(&env).await;

    assert_eq!(swept.granted, 1, "the request for #2 is judged again");
    let stored = grants(&env).await;
    let [grant] = &stored[..] else {
        panic!("one grant: {stored:?}");
    };
    assert_eq!(
        (grant.grant.item, grant.decided),
        (bobs, GrantDecision::Waited)
    );
    assert!(unsettled(&env).await.is_empty());
}

#[tokio::test]
async fn a_holders_grant_revoked_by_the_holder_is_not_given_again_for_any_request_it_answered() {
    let (env, _, bobs) = held().await;
    waiting(ask(&env, &env.bob, bobs).await);
    waiting(ask(&env, &env.bob, bobs).await);
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
    assert!(
        unsettled(&env).await.is_empty(),
        "the holder's grant answers every request of bob's for billing"
    );
    let given = grants(&env).await.remove(0);
    assert_eq!(given.decided, GrantDecision::Holder);

    revoke(&env, &env.alice, &given).await;
    env.advance(Duration::minutes(10));

    let swept = sweep(&env).await;
    assert_eq!((swept.granted, swept.settled), (0, 0));
    assert!(grants(&env).await.is_empty());
}

#[tokio::test]
async fn a_request_whose_claim_ended_is_settled_and_never_granted_to_a_new_claim() {
    let (env, _, bobs) = held().await;
    waiting(ask(&env, &env.bob, bobs).await);
    Release::new(&env.deps)
        .execute(&env.bob.actor, bobs)
        .await
        .unwrap();
    env.advance(Duration::seconds(1));
    claim(&env, &env.bob, bobs, &[]).await;
    env.advance(Duration::minutes(10));

    let swept = sweep(&env).await;

    assert_eq!((swept.granted, swept.settled), (0, 1));
    assert!(grants(&env).await.is_empty());
    assert!(unsettled(&env).await.is_empty());
    env.advance(Duration::minutes(10));
    assert_eq!(sweep(&env).await.settled, 0, "settled once");
}

#[tokio::test]
async fn two_requests_of_one_asker_for_one_area_are_granted_once() {
    let (env, _, bobs) = held().await;
    waiting(ask(&env, &env.bob, bobs).await);
    waiting(ask(&env, &env.bob, bobs).await);
    env.advance(Duration::minutes(10));

    let swept = sweep(&env).await;

    assert_eq!(swept.granted, 1);
    assert_eq!(grants(&env).await.len(), 1, "no second grant record");
    assert!(unsettled(&env).await.is_empty());
    let granted = env
        .events()
        .await
        .iter()
        .filter(|event| matches!(event.kind, EventKind::Granted(_)))
        .count();
    assert_eq!(granted, 1);
}

#[tokio::test]
async fn no_grant_is_written_for_a_holder_who_no_longer_holds_the_area() {
    let (env, alices, bobs) = held().await;
    waiting(ask(&env, &env.bob, bobs).await);
    NarrowClaim::new(&env.deps)
        .execute(
            &env.alice.actor,
            NarrowClaimInput {
                item: alices,
                areas: vec!["billing".to_owned()],
            },
        )
        .await
        .unwrap();
    env.advance(Duration::minutes(10));
    let quiet = env.events().await.len();

    let swept = sweep(&env).await;

    assert_eq!(
        (swept.granted, swept.settled),
        (0, 1),
        "the holder holds the area through no claim, so the request is closed and settled"
    );
    assert!(grants(&env).await.is_empty());
    assert!(unsettled(&env).await.is_empty());
    assert_eq!(env.events().await.len(), quiet);
}

#[tokio::test]
async fn an_admins_revoke_sticks_until_the_holder_claims_again() {
    let env = Env::new().await;
    let carol = carol(&env).await;
    let bobs = env.item("gh:acme/widgets#2").await;
    let carols = env.item("gh:acme/widgets#3").await;
    claim(&env, &env.bob, bobs, &["billing"]).await;
    claim(&env, &carol, carols, &[]).await;
    review(&env, &env.bob, bobs).await;
    given_at_once(ask(&env, &carol, carols).await);
    let given = grants(&env).await.remove(0);
    let before = env.events().await.len();

    revoke(&env, &env.alice, &given).await;

    assert!(
        env.kinds_after(before)
            .await
            .contains(&EventKind::GrantRevoked(GrantRevoked {
                grant: given.id,
                item: carols,
                area: given.grant.area.clone(),
                from: env.bob.actor.member,
                to: carol.actor.member,
                cause: GrantEnd::Revoked,
                by: Some(env.alice.actor.member),
            })),
        "the event names the admin"
    );
    waiting(ask(&env, &carol, carols).await);
    env.advance(Duration::minutes(10));
    assert_eq!(
        sweep(&env).await.granted,
        0,
        "bob's claim is still in review, and the request waits for bob"
    );
    assert!(grants(&env).await.is_empty());

    Release::new(&env.deps)
        .execute(&env.bob.actor, bobs)
        .await
        .unwrap();
    env.advance(Duration::seconds(1));
    claim(&env, &env.bob, bobs, &["billing"]).await;
    waiting(ask(&env, &carol, carols).await);
    env.advance(Duration::minutes(10));

    assert_eq!(
        sweep(&env).await.granted,
        1,
        "bob's new claim is judged afresh"
    );
    let regranted = grants(&env).await;
    assert_eq!(regranted.len(), 1);
    assert_eq!(regranted[0].decided, GrantDecision::Waited);
}

/// bob claims `item` wide over `named`, which announces it to the holders of those areas.
async fn wide_claim(env: &Env, item: ItemId, named: &[&str]) -> ClaimMade {
    ClaimItem::new(&env.deps)
        .execute(
            &env.bob.actor,
            ClaimItemInput {
                branches: Default::default(),
                wide: Some(WideInput {
                    areas: named.iter().map(|area| (*area).to_owned()).collect(),
                    reason: WideReason::parse("Upgrade the HTTP client").unwrap(),
                }),
                item,
                session: env.bob.session,
                areas: Vec::new(),
                kind: None,
            },
        )
        .await
        .unwrap()
}

/// alice's claim on `alices` renewed over `areas`, answered as the claim answers it.
async fn alice_claims(env: &Env, alices: ItemId, areas: &[&str]) -> ClaimMade {
    ClaimItem::new(&env.deps)
        .execute(
            &env.alice.actor,
            ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item: alices,
                session: env.alice.session,
                areas: areas.iter().map(|area| (*area).to_owned()).collect(),
                kind: None,
            },
        )
        .await
        .unwrap()
}

/// alice holds billing and search on `#1`, her branch changing `paths`, and bob claims `#2` wide
/// over both. Answers the items and the one announcement sent to alice.
async fn announced_to_alice(paths: &[&str]) -> (Env, ItemId, ItemId, Announced) {
    let env = Env::new().await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim(&env, &env.alice, alices, &["billing", "search"]).await;
    footprint_of(&env, &env.alice, alices, "alice/1-x", paths).await;
    let made = wide_claim(&env, bobs, &["billing", "search"]).await;
    let [announced] = &made.announced[..] else {
        panic!("one announcement: {:?}", made.announced);
    };
    let announced = announced.clone();
    (env, alices, bobs, announced)
}

/// Why dibs acknowledged the announcement `message` carries on its holder's behalf, if it did.
async fn acked_by_dibs(env: &Env, message: MessageId) -> Option<GrantDecision> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.wide_ask(env.project, message)
        .await
        .unwrap()
        .expect("the message carries an announcement")
        .acked_by_dibs
}

async fn open_announcements(env: &Env) -> Vec<WideAsk> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.open_wide_asks(env.project).await.unwrap()
}

/// Checks that bob holds alice's grants in billing and search for `bobs`, each decided by
/// `because`, each with a `wide_granted_by_ack` naming it, and that both read in the
/// announcement's conversation that dibs acknowledged it, why, and how to undo the grants.
async fn acknowledged_on_alices_behalf(
    env: &Env,
    bobs: ItemId,
    thread: ThreadId,
    because: GrantDecision,
) -> Vec<GrantRecord> {
    let stored = grants(env).await;
    let summary: Vec<(String, ItemId, GrantDecision)> = stored
        .iter()
        .map(|grant| {
            assert_eq!(grant.grant.from, env.alice.actor.member, "alice's grant");
            assert_eq!(grant.grant.to, env.bob.actor.member);
            (
                grant.grant.area.to_string(),
                grant.grant.item,
                grant.decided,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("default:billing".to_owned(), bobs, because),
            ("default:search".to_owned(), bobs, because),
        ]
    );
    let events = env.events().await;
    for grant in &stored {
        assert!(
            events.iter().any(|event| event.kind
                == EventKind::WideGrantedByAck(WideGrantedByAck {
                    grant: grant.id,
                    item: bobs,
                    area: grant.grant.area.clone(),
                    by: env.alice.actor.member,
                    to: env.bob.actor.member,
                    decided: because,
                })),
            "the event records who acknowledged"
        );
    }
    let why = because.why().unwrap();
    for who in [&env.alice, &env.bob] {
        let messages = conversation(env, who, thread).await;
        let (author, reply) = messages.last().unwrap();
        assert_eq!(*author, Author::System);
        assert!(
            reply.starts_with(&format!(
                "Acknowledged by dibs on alice's behalf, because {why}. bob may edit there for work item gh:acme/widgets#2 \"Work on gh:acme/widgets#2\": grant {} in default:billing, grant {} in default:search.",
                stored[0].id, stored[1].id
            )),
            "{reply}"
        );
        assert!(
            reply.contains(&format!(
                "To undo a grant, alice or an admin revokes it (dibs revoke-grant {id}, or DELETE /v1/grants/{id}).",
                id = stored[0].id
            )),
            "{reply}"
        );
    }
    stored
}

#[tokio::test]
async fn an_announcement_is_acknowledged_at_once_when_the_holders_branch_touches_none_of_its_areas()
{
    let (env, alices, bobs, announced) = announced_to_alice(&["README.md"]).await;

    assert_eq!(announced.ask.acked_by_dibs, Some(GrantDecision::Untouched));
    acknowledged_on_alices_behalf(&env, bobs, announced.thread, GrantDecision::Untouched).await;
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.acks(env.project, announced.ask.message)
            .await
            .unwrap()
            .is_empty(),
        "alice did not read it, so no acknowledgement of hers is written"
    );
    drop(tx);
    assert_eq!(
        acked_by_dibs(&env, announced.ask.message).await,
        Some(GrantDecision::Untouched)
    );
    let status = crate::use_cases::Status::new(&env.deps)
        .execute(&env.bob.actor)
        .await
        .unwrap();
    let standing: Vec<(bool, Option<GrantDecision>)> = status.wide[0]
        .areas
        .iter()
        .map(|row| (row.granted, row.decided))
        .collect();
    assert_eq!(
        standing,
        [(true, Some(GrantDecision::Untouched)); 2],
        "the status says dibs decided each area, and why"
    );

    let renewed = alice_claims(&env, alices, &["billing", "search"]).await;
    assert_eq!(
        renewed.inside_wide.len(),
        2,
        "both areas lie inside the wide claim"
    );
    assert!(
        renewed
            .inside_wide
            .iter()
            .all(|note| note.message.is_none()),
        "no message waits for alice: {:?}",
        renewed.inside_wide
    );
    assert!(
        open_announcements(&env).await.is_empty(),
        "nothing is announced again"
    );
    assert_eq!(
        sweep(&env).await.acknowledged,
        0,
        "dibs acknowledges it once"
    );
}

#[tokio::test]
async fn an_announcement_with_a_touched_area_waits_and_the_sweep_acknowledges_it_once_the_delay_passed()
 {
    let (env, _, bobs, announced) = announced_to_alice(&["packages/search/index.rs"]).await;
    assert_eq!(announced.ask.acked_by_dibs, None);
    assert!(grants(&env).await.is_empty());

    env.advance(Duration::minutes(9));
    assert_eq!(sweep(&env).await.acknowledged, 0, "search is touched");
    env.advance(Duration::minutes(1));
    assert_eq!(sweep(&env).await.acknowledged, 1);

    acknowledged_on_alices_behalf(&env, bobs, announced.thread, GrantDecision::Waited).await;
    assert!(open_announcements(&env).await.is_empty());
    env.advance(Duration::minutes(10));
    assert_eq!(
        sweep(&env).await.acknowledged,
        0,
        "dibs acknowledges it once"
    );
    assert_eq!(grants(&env).await.len(), 2);
}

#[tokio::test]
async fn an_announcement_to_a_holder_in_review_is_acknowledged_at_once() {
    let env = Env::new().await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim(&env, &env.alice, alices, &["billing", "search"]).await;
    review(&env, &env.alice, alices).await;

    let made = wide_claim(&env, bobs, &["billing", "search"]).await;

    let [announced] = &made.announced[..] else {
        panic!("a holder in review is announced to: {:?}", made.announced);
    };
    assert_eq!(announced.ask.acked_by_dibs, Some(GrantDecision::InReview));
    acknowledged_on_alices_behalf(&env, bobs, announced.thread, GrantDecision::InReview).await;
}

#[tokio::test]
async fn a_holder_who_acknowledges_before_the_delay_leaves_dibs_nothing_to_do() {
    let (env, _, _, announced) = announced_to_alice(&["packages/search/index.rs"]).await;
    let acked = AckMessage::new(&env.deps)
        .execute(&env.alice.actor, announced.ask.message)
        .await
        .unwrap();
    assert_eq!(acked.granted.len(), 2);

    env.advance(Duration::minutes(10));
    assert_eq!(sweep(&env).await.acknowledged, 0);

    assert_eq!(acked_by_dibs(&env, announced.ask.message).await, None);
    let stored = grants(&env).await;
    assert_eq!(stored.len(), 2);
    assert!(
        stored
            .iter()
            .all(|grant| grant.decided == GrantDecision::Holder),
        "{stored:?}"
    );
}

#[tokio::test]
async fn an_announcement_dibs_acknowledged_is_not_delivered_as_awaiting_an_acknowledgement() {
    let (env, _, _, announced) = announced_to_alice(&["README.md"]).await;

    let inbox = Inbox::new(&env.deps)
        .execute(
            &env.alice.actor,
            InboxQuery {
                unread_only: false,
                unacked: true,
                deliverable: true,
            },
        )
        .await
        .unwrap();

    let message = announced.ask.message;
    assert!(
        !inbox
            .deliverable
            .unwrap()
            .iter()
            .any(|delivered| delivered.message.record.id == message),
        "it is not handed to alice's agent"
    );
    assert!(
        !inbox
            .unacked
            .unwrap()
            .iter()
            .any(|unacked| unacked.message.record.id == message),
        "it does not wait for alice's acknowledgement"
    );
}

#[tokio::test]
async fn revoking_a_grant_of_dibs_acknowledgement_neither_brings_it_back_nor_announces_again() {
    let (env, alices, bobs, announced) = announced_to_alice(&["README.md"]).await;
    let given = grants(&env).await;
    let billing = given
        .iter()
        .find(|grant| grant.grant.area.to_string() == "default:billing")
        .unwrap();

    revoke(&env, &env.alice, billing).await;
    env.advance(Duration::minutes(10));

    assert_eq!(sweep(&env).await.acknowledged, 0);
    let left: Vec<String> = grants(&env)
        .await
        .iter()
        .map(|grant| grant.grant.area.to_string())
        .collect();
    assert_eq!(left, ["default:search"]);
    assert_eq!(
        acked_by_dibs(&env, announced.ask.message).await,
        Some(GrantDecision::Untouched),
        "still acknowledged by dibs"
    );
    let renewed = alice_claims(&env, alices, &["billing", "search"]).await;
    assert!(
        renewed
            .inside_wide
            .iter()
            .all(|note| note.message.is_none())
    );
    let widened = wide_claim(&env, bobs, &["billing", "search"]).await;
    assert!(widened.announced.is_empty(), "{:?}", widened.announced);
    assert!(open_announcements(&env).await.is_empty());
}

#[tokio::test]
async fn revoking_a_grant_the_holders_own_acknowledgement_gave_is_not_announced_again() {
    let (env, alices, bobs, announced) = announced_to_alice(&["packages/search/index.rs"]).await;
    AckMessage::new(&env.deps)
        .execute(&env.alice.actor, announced.ask.message)
        .await
        .unwrap();
    let given = grants(&env).await;

    revoke(&env, &env.alice, &given[0]).await;
    alice_claims(&env, alices, &["billing", "search"]).await;
    let widened = wide_claim(&env, bobs, &["billing", "search"]).await;

    assert!(widened.announced.is_empty(), "{:?}", widened.announced);
    assert!(open_announcements(&env).await.is_empty());
    assert_eq!(grants(&env).await.len(), 1);
}

/// The announcements dibs still judges on their holders' behalf: not settled, not left to their
/// holders.
async fn judged(env: &Env) -> Vec<WideAsk> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.open_wide_asks_with_time(env.project)
        .await
        .unwrap()
        .into_iter()
        .filter(|(record, _)| !record.left_to_holder)
        .map(|(record, _)| record.ask)
        .collect()
}

async fn release(env: &Env, who: &Person, item: ItemId) {
    Release::new(&env.deps)
        .execute(&who.actor, item)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_announcement_whose_holder_gave_back_every_area_is_settled_and_stays_open_for_them() {
    let (env, alices, _, announced) = announced_to_alice(&["packages/search/index.rs"]).await;
    NarrowClaim::new(&env.deps)
        .execute(
            &env.alice.actor,
            NarrowClaimInput {
                item: alices,
                areas: vec!["billing".to_owned(), "search".to_owned()],
            },
        )
        .await
        .unwrap();
    env.advance(Duration::minutes(30));

    assert_eq!(sweep(&env).await.acknowledged, 0);

    assert_eq!(acked_by_dibs(&env, announced.ask.message).await, None);
    assert!(grants(&env).await.is_empty());
    assert!(judged(&env).await.is_empty(), "dibs stops judging it");
    let open = open_announcements(&env).await;
    assert_eq!(
        open.iter().map(|ask| ask.message).collect::<Vec<_>>(),
        [announced.ask.message],
        "alice may still acknowledge it"
    );
}

#[tokio::test]
async fn an_announcement_whose_wide_claim_ended_is_settled_and_the_next_sweep_finds_no_work() {
    let (env, _, bobs, _) = announced_to_alice(&["packages/search/index.rs"]).await;
    release(&env, &env.bob, bobs).await;

    let swept = sweep(&env).await;

    assert_eq!((swept.acknowledged, swept.announcements_closed), (0, 1));
    assert!(judged(&env).await.is_empty(), "settled by this sweep");
    let quiet = env.events().await.len();
    let again = sweep(&env).await;
    assert_eq!(
        (
            again.acknowledged,
            again.announcements_closed,
            again.left_to_holders
        ),
        (0, 0, 0)
    );
    assert_eq!(env.events().await.len(), quiet, "nothing is written");
}

#[tokio::test]
async fn an_announcement_a_revoke_stands_against_is_left_to_the_holder_by_the_sweep() {
    let (env, _, bobs, announced) = announced_to_alice(&["packages/search/index.rs"]).await;
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
    let given = grants(&env).await.remove(0);
    revoke(&env, &env.alice, &given).await;
    env.advance(Duration::minutes(10));

    let swept = sweep(&env).await;

    assert_eq!((swept.acknowledged, swept.left_to_holders), (0, 1));
    assert!(grants(&env).await.is_empty());
    assert!(
        judged(&env).await.is_empty(),
        "left to alice for good, so dibs stops judging it"
    );
    assert_eq!(
        open_announcements(&env)
            .await
            .iter()
            .map(|ask| ask.message)
            .collect::<Vec<_>>(),
        [announced.ask.message],
        "alice may still acknowledge it"
    );
    let left = "Left to alice by dibs: a revoke of alice's grant to bob in default:billing stands while both claims last, so dibs leaves this announcement to alice to acknowledge or answer.";
    for who in [&env.alice, &env.bob] {
        let messages = conversation(&env, who, announced.thread).await;
        let [_, (author, reply)] = &messages[..] else {
            panic!("the announcement and one reply: {messages:?}");
        };
        assert_eq!(*author, Author::System);
        assert!(reply.starts_with(left), "{reply}");
    }
    let quiet = env.events().await.len();
    assert_eq!(sweep(&env).await.acknowledged, 0);
    assert_eq!(env.events().await.len(), quiet);
    assert_eq!(
        conversation(&env, &env.alice, announced.thread).await.len(),
        2,
        "dibs says it once"
    );
}

#[tokio::test]
async fn a_holder_whose_dibs_acknowledged_grants_ended_is_announced_to_again_when_claiming_anew() {
    let (env, alices, _, first) = announced_to_alice(&["README.md"]).await;
    assert_eq!(first.ask.acked_by_dibs, Some(GrantDecision::Untouched));
    release(&env, &env.alice, alices).await;
    assert!(
        grants(&env).await.is_empty(),
        "the grants end with alice's claim"
    );
    env.advance(Duration::seconds(1));

    alice_claims(&env, alices, &["billing", "search"]).await;

    let open = open_announcements(&env).await;
    let [again] = &open[..] else {
        panic!("one new announcement to alice: {open:?}");
    };
    assert_ne!(again.message, first.ask.message);
    assert_eq!(
        again.acked_by_dibs, None,
        "judged afresh: nothing lets dibs yet"
    );
    assert_eq!(
        judged(&env)
            .await
            .iter()
            .map(|ask| ask.message)
            .collect::<Vec<_>>(),
        [again.message]
    );
}

#[tokio::test]
async fn a_holder_whose_announcement_was_settled_is_announced_to_again_when_claiming_anew() {
    let (env, alices, _, first) = announced_to_alice(&["packages/search/index.rs"]).await;
    release(&env, &env.alice, alices).await;
    sweep(&env).await;
    assert!(
        judged(&env).await.is_empty(),
        "settled: alice holds none of its areas"
    );
    env.advance(Duration::seconds(1));

    alice_claims(&env, alices, &["billing", "search"]).await;

    let open: Vec<_> = open_announcements(&env)
        .await
        .into_iter()
        .map(|ask| ask.message)
        .collect();
    assert_eq!(open.len(), 2, "a new announcement goes out: {open:?}");
    assert_eq!(
        open[0], first.ask.message,
        "the settled one stays open for alice"
    );
    assert_eq!(
        judged(&env)
            .await
            .iter()
            .map(|ask| ask.message)
            .collect::<Vec<_>>(),
        [open[1]],
        "dibs judges the new one"
    );
}

#[tokio::test]
async fn a_request_left_to_the_holder_is_settled_once_their_claim_ends_and_not_shown_on_a_new_one()
{
    let (env, alices, bobs) = held().await;
    review(&env, &env.alice, alices).await;
    given_at_once(ask(&env, &env.bob, bobs).await);
    let given = grants(&env).await.remove(0);
    revoke(&env, &env.alice, &given).await;
    let GrantAsked::Asked {
        left_to_holder: true,
        ..
    } = ask(&env, &env.bob, bobs).await
    else {
        panic!("not left to alice");
    };
    let quiet = env.events().await.len();
    let idle = sweep(&env).await;
    assert_eq!(
        (idle.granted, idle.settled, idle.left_to_holders),
        (0, 0, 0)
    );
    assert_eq!(
        env.events().await.len(),
        quiet,
        "nothing closes it yet, so the sweep writes nothing"
    );

    Complete::new(&env.deps)
        .execute(&env.alice.actor, alices)
        .await
        .unwrap();
    let swept = sweep(&env).await;

    assert_eq!(
        (swept.granted, swept.settled, swept.left_to_holders),
        (0, 1, 0),
        "alice holds the area through no claim, so the request closed"
    );
    let alices_next = env.item("gh:acme/widgets#3").await;
    claim(&env, &env.alice, alices_next, &["billing"]).await;
    let inbox = Inbox::new(&env.deps)
        .execute(&env.alice.actor, InboxQuery::default())
        .await
        .unwrap();
    assert!(
        inbox.grant_asks.is_empty(),
        "no revoke of alice's stands against her new claim: {:?}",
        inbox.grant_asks
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.unanswered_asked_grants(env.project, env.alice.actor.member)
            .await
            .unwrap()
            .is_empty()
    );
    drop(tx);
    env.advance(Duration::minutes(30));
    let quiet = env.events().await.len();
    let again = sweep(&env).await;
    assert_eq!((again.granted, again.settled), (0, 0));
    assert!(grants(&env).await.is_empty(), "dibs never grants it");
    assert_eq!(env.events().await.len(), quiet);
}

#[tokio::test]
async fn an_announcement_left_to_the_holder_is_settled_once_they_give_its_areas_up_and_a_new_claim_is_announced_to()
 {
    let (env, alices, bobs, first) = announced_to_alice(&["packages/search/index.rs"]).await;
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
    let given = grants(&env).await.remove(0);
    revoke(&env, &env.alice, &given).await;
    env.advance(Duration::minutes(10));
    assert_eq!(sweep(&env).await.left_to_holders, 1);
    let read = env
        .fakes
        .store
        .footprint_reads_of(env.project, alices)
        .await;
    let listed = env.fakes.store.footprint_listings_of(env.project).await;
    let quiet = env.events().await.len();
    let idle = sweep(&env).await;
    assert_eq!(
        (
            idle.acknowledged,
            idle.announcements_closed,
            idle.left_to_holders
        ),
        (0, 0, 0)
    );
    assert_eq!(
        env.fakes
            .store
            .footprint_reads_of(env.project, alices)
            .await,
        read,
        "a left announcement is never judged, so no footprint is read for it"
    );
    assert_eq!(
        env.fakes.store.footprint_listings_of(env.project).await,
        listed,
        "a sweep whose only rows are left ones lists no footprints"
    );
    assert_eq!(env.events().await.len(), quiet, "the sweep writes nothing");

    release(&env, &env.alice, alices).await;
    let swept = sweep(&env).await;

    assert_eq!(
        (
            swept.acknowledged,
            swept.announcements_closed,
            swept.left_to_holders
        ),
        (0, 1, 0),
        "alice holds none of its areas, so it closed"
    );
    env.advance(Duration::seconds(1));
    alice_claims(&env, alices, &["billing", "search"]).await;
    let open: Vec<_> = open_announcements(&env)
        .await
        .into_iter()
        .map(|ask| ask.message)
        .collect();
    assert_eq!(open.len(), 2, "a new announcement goes out: {open:?}");
    assert_eq!(
        open[0], first.ask.message,
        "the old one stays open for alice"
    );
    assert!(
        judged(&env).await.iter().any(|ask| ask.message == open[1]),
        "dibs judges the new one"
    );
}

/// alice gives bob billing for `bobs` and revokes it, so a revoke between their two claims sticks.
async fn revoked_by_alice(env: &Env, bobs: ItemId) {
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
    let given = grants(env).await.remove(0);
    revoke(env, &env.alice, &given).await;
}

#[tokio::test]
async fn a_request_left_to_the_holder_closes_when_they_narrow_the_area_away_though_the_revoke_stands()
 {
    let (env, alices, bobs) = held().await;
    revoked_by_alice(&env, bobs).await;
    let GrantAsked::Asked {
        left_to_holder: true,
        ..
    } = ask(&env, &env.bob, bobs).await
    else {
        panic!("not left to alice");
    };

    NarrowClaim::new(&env.deps)
        .execute(
            &env.alice.actor,
            NarrowClaimInput {
                item: alices,
                areas: vec!["billing".to_owned()],
            },
        )
        .await
        .unwrap();
    let swept = sweep(&env).await;

    assert_eq!(
        (swept.granted, swept.settled, swept.left_to_holders),
        (0, 1, 0)
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.unsettled_asked_grants(env.project)
            .await
            .unwrap()
            .is_empty(),
        "settled, left to alice or not"
    );
}

#[tokio::test]
async fn an_announcement_left_to_the_holder_closes_when_they_narrow_its_areas_away_though_the_revoke_stands()
 {
    let (env, alices, bobs, _) = announced_to_alice(&["packages/search/index.rs"]).await;
    revoked_by_alice(&env, bobs).await;
    env.advance(Duration::minutes(10));
    assert_eq!(sweep(&env).await.left_to_holders, 1);

    NarrowClaim::new(&env.deps)
        .execute(
            &env.alice.actor,
            NarrowClaimInput {
                item: alices,
                areas: vec!["billing".to_owned(), "search".to_owned()],
            },
        )
        .await
        .unwrap();
    let swept = sweep(&env).await;

    assert_eq!(
        (
            swept.acknowledged,
            swept.announcements_closed,
            swept.left_to_holders
        ),
        (0, 1, 0)
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.open_wide_asks_with_time(env.project)
            .await
            .unwrap()
            .is_empty(),
        "settled, left to alice or not"
    );
}

#[tokio::test]
async fn a_request_left_to_the_holder_is_settled_once_the_revoke_no_longer_applies_to_the_claim_that_holds_the_area()
 {
    let (env, alices, bobs) = held().await;
    revoked_by_alice(&env, bobs).await;
    let GrantAsked::Asked {
        left_to_holder: true,
        ..
    } = ask(&env, &env.bob, bobs).await
    else {
        panic!("not left to alice");
    };
    let alices_next = env.item("gh:acme/widgets#3").await;
    claim(&env, &env.alice, alices_next, &["billing"]).await;
    Complete::new(&env.deps)
        .execute(&env.alice.actor, alices)
        .await
        .unwrap();

    let swept = sweep(&env).await;

    assert_eq!(
        (swept.granted, swept.settled, swept.left_to_holders),
        (0, 1, 0),
        "alice holds billing through a claim the revoke does not reach, so it is settled"
    );
    let inbox = Inbox::new(&env.deps)
        .execute(&env.alice.actor, InboxQuery::default())
        .await
        .unwrap();
    assert!(
        inbox.grant_asks.is_empty(),
        "no revoke of alice's stands against her new claim: {:?}",
        inbox.grant_asks
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.unsettled_asked_grants(env.project)
            .await
            .unwrap()
            .is_empty()
    );
    drop(tx);
    let quiet = env.events().await.len();
    let again = sweep(&env).await;
    assert_eq!((again.granted, again.settled), (0, 0));
    assert_eq!(env.events().await.len(), quiet);
}

#[tokio::test]
async fn an_announcement_left_to_the_holder_is_settled_once_the_revoke_no_longer_applies_and_the_new_claim_is_announced_to()
 {
    let (env, alices, bobs, first) = announced_to_alice(&["packages/search/index.rs"]).await;
    revoked_by_alice(&env, bobs).await;
    env.advance(Duration::minutes(10));
    assert_eq!(sweep(&env).await.left_to_holders, 1);
    let alices_next = env.item("gh:acme/widgets#3").await;
    claim(&env, &env.alice, alices_next, &["billing"]).await;
    Complete::new(&env.deps)
        .execute(&env.alice.actor, alices)
        .await
        .unwrap();
    assert_eq!(
        open_announcements(&env)
            .await
            .iter()
            .map(|ask| ask.message)
            .collect::<Vec<_>>(),
        [first.ask.message],
        "the left announcement still counts as asked"
    );

    let swept = sweep(&env).await;

    assert_eq!(
        (
            swept.acknowledged,
            swept.announcements_closed,
            swept.left_to_holders
        ),
        (0, 1, 0),
        "alice holds billing through a claim the revoke does not reach, so it is settled"
    );
    env.advance(Duration::seconds(1));
    alice_claims(&env, alices_next, &["billing"]).await;
    let open: Vec<_> = open_announcements(&env).await;
    let [old, new] = &open[..] else {
        panic!("a new announcement goes out: {open:?}");
    };
    assert_eq!(
        old.message, first.ask.message,
        "the old one stays open for alice"
    );
    assert_eq!(new.item, bobs);
    assert_eq!(new.to, env.alice.actor.member);
    assert!(
        judged(&env)
            .await
            .iter()
            .any(|ask| ask.message == new.message),
        "dibs judges the new one"
    );
}

#[tokio::test]
async fn a_request_left_to_the_holder_with_no_revoke_recorded_stays_left() {
    let (env, alices, bobs) = held().await;
    waiting(ask(&env, &env.bob, bobs).await);
    let [(asked, _)] = &unsettled(&env).await[..] else {
        panic!("one request");
    };
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.leave_asked_grant_to_holder(env.project, asked.message)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let alices_next = env.item("gh:acme/widgets#3").await;
    claim(&env, &env.alice, alices_next, &["billing"]).await;
    Complete::new(&env.deps)
        .execute(&env.alice.actor, alices)
        .await
        .unwrap();
    env.advance(Duration::minutes(30));

    let swept = sweep(&env).await;

    assert_eq!(
        (swept.granted, swept.settled, swept.left_to_holders),
        (0, 0, 0),
        "with no revoke recorded, nothing tells dibs it outlived one"
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    let listed = tx.unsettled_asked_grants(env.project).await.unwrap();
    let [(record, _)] = &listed[..] else {
        panic!("still unsettled: {listed:?}");
    };
    assert!(record.left_to_holder, "it keeps its mark until it closes");
}
