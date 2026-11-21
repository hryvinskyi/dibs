//! The claim sweep over the in-memory store: a working claim whose work stopped showing for the
//! project's `release_after` is released and its holder told in the claim's conversation; a claim
//! seen since, a claim in review and a project with nothing due are left as they are.

use time::Duration;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::Claim;
use dibs_domain::handover::ClaimKind;
use dibs_domain::ids::{ItemId, ThreadId};
use dibs_domain::item::{ExternalRef, Title};
use dibs_domain::message::{Author, Recipients};
use dibs_domain::project::ProjectSlug;
use dibs_domain::review::PrRef;

use super::*;
use crate::records::{MessageRecord, ThreadRecord};
use crate::use_cases::testing::{Env, Person, START, document, new_member, session};
use crate::use_cases::{
    Authenticate, ClaimItem, ClaimItemInput, CreateItem, CreateItemInput, InitProject,
    InitProjectInput, ReadThread, StartReview, StartReviewInput,
};

const CLAIM_42: &str = "Your claim on gh:acme/widgets#42 \"Work on gh:acme/widgets#42\"";

fn area(text: &str) -> QualifiedArea {
    QualifiedArea::parse(text).unwrap()
}

async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str], kind: Option<ClaimKind>) {
    ClaimItem::new(&env.deps)
        .execute(
            &who.actor,
            ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item,
                session: who.session,
                areas: areas.iter().map(|area| (*area).to_owned()).collect(),
                kind,
            },
        )
        .await
        .unwrap();
}

async fn stored(env: &Env, item: ItemId) -> Option<Claim> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.claim(env.project, item).await.unwrap()
}

async fn status(env: &Env, item: ItemId) -> ItemStatus {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.item(env.project, item).await.unwrap().unwrap().status
}

async fn sweep(env: &Env) -> ClaimSweep {
    TickClaims::new(&env.deps).execute().await.unwrap()
}

/// The conversation linked to the claim, as its holder reads it.
async fn claim_conversation(
    env: &Env,
    who: &Person,
    claim: &Claim,
) -> (ThreadRecord, Vec<MessageRecord>) {
    let mut tx = env.deps.store.begin().await.unwrap();
    let thread: ThreadId = tx
        .claim_thread(env.project, &claim.instance())
        .await
        .unwrap()
        .expect("dibs wrote about the claim")
        .thread;
    drop(tx);
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

#[tokio::test]
async fn a_claim_unseen_for_seven_days_is_released_and_its_holder_told_in_its_conversation() {
    let env = Env::new().await;
    let stale = env.item("gh:acme/widgets#42").await;
    let recent = env.item("gh:acme/widgets#43").await;
    let reviewing = env.item("gh:acme/widgets#44").await;
    claim(&env, &env.bob, stale, &["billing"], None).await;
    claim(&env, &env.alice, reviewing, &["search"], None).await;
    StartReview::new(&env.deps)
        .execute(
            &env.alice.actor,
            StartReviewInput {
                head: None,
                item: reviewing,
                pr: PrRef::parse("acme/widgets#7").unwrap(),
                more: false,
            },
        )
        .await
        .unwrap();
    env.advance(Duration::days(1));
    claim(&env, &env.alice, recent, &[], None).await;
    env.advance(Duration::days(6));
    assert_eq!(
        sweep(&env).await.released,
        0,
        "seven days are not more than seven days"
    );
    env.advance(Duration::minutes(1));
    let held = stored(&env, stale).await.unwrap();
    let before = env.events().await.len();

    let swept = sweep(&env).await;

    assert_eq!((swept.projects, swept.released), (1, 1));
    assert!(swept.failed.is_empty());
    assert_eq!(stored(&env, stale).await, None, "the claim ended");
    assert_eq!(status(&env, stale).await, ItemStatus::Open);
    let kinds = env.kinds_after(before).await;
    assert_eq!(
        kinds[0],
        EventKind::ReleasedStale(ReleasedStale {
            item: stale,
            member: env.bob.actor.member,
            last_seen: START,
        })
    );
    assert!(
        matches!(
            &kinds[1..],
            [EventKind::ThreadStarted(started), EventKind::MessagePosted(_)]
                if started.item == Some(stale) && started.author.is_none()
        ),
        "{kinds:?}"
    );
    let (thread, messages) = claim_conversation(&env, &env.bob, &held).await;
    assert_eq!(thread.subject.as_str(), CLAIM_42);
    assert_eq!(
        thread.recipients,
        Recipients::Members([env.bob.actor.member].into())
    );
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.author, Author::System);
    assert_eq!(message.importance, Importance::Normal);
    assert!(!message.ack_required);
    assert!(
        message.body.as_str().starts_with(
            "Released: dibs released your claim on work item gh:acme/widgets#42 \"Work on gh:acme/widgets#42\" because nothing of its work showed for 7d"
        ),
        "{}",
        message.body.as_str()
    );

    assert!(stored(&env, recent).await.is_some(), "seen six days ago");
    assert!(
        stored(&env, reviewing).await.is_some(),
        "a claim in review is never released"
    );
    let other = env.item("gh:acme/widgets#45").await;
    claim(&env, &env.alice, other, &["billing"], None).await;
    assert!(
        stored(&env, other)
            .await
            .unwrap()
            .holds(&area("default:billing")),
        "the released claim's areas are free"
    );
}

#[tokio::test]
async fn a_project_with_nothing_due_is_swept_without_a_write() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#42").await;
    let before = env.events().await.len();
    let empty = sweep(&env).await;
    assert_eq!((empty.projects, empty.released), (1, 0), "no claim at all");
    assert_eq!(env.events().await.len(), before);
    claim(&env, &env.bob, item, &["billing"], None).await;
    env.advance(Duration::days(6));
    let claimed = env.events().await.len();
    let swept = sweep(&env).await;
    assert_eq!((swept.projects, swept.released), (1, 0), "no claim due");
    assert_eq!(env.events().await.len(), claimed);
}

#[tokio::test]
async fn a_claim_its_holder_renewed_before_the_sweep_is_kept() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"], None).await;
    env.advance(Duration::days(7) + Duration::minutes(1));
    claim(&env, &env.bob, item, &[], None).await;
    assert_eq!(sweep(&env).await.released, 0);
    assert!(stored(&env, item).await.is_some());
}

#[tokio::test]
async fn a_project_that_releases_after_three_days_releases_after_three_days() {
    let env = Env::with_settings(
        r#"{"stale_after": "24h", "release_after": "3d",
            "areas": [{"name": "billing", "paths": ["packages/billing/**"]}]}"#,
    )
    .await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"], None).await;
    env.advance(Duration::days(3));
    assert_eq!(sweep(&env).await.released, 0);
    env.advance(Duration::minutes(1));
    assert_eq!(sweep(&env).await.released, 1);
    assert_eq!(stored(&env, item).await, None);
    assert_eq!(status(&env, item).await, ItemStatus::Open);
}

#[tokio::test]
async fn a_debt_claim_released_hands_its_areas_to_the_feature_claim_waiting_for_them() {
    let env = Env::with_settings(
        r#"{"stale_after": "2d", "release_after": "2d1h",
            "areas": [{"name": "billing", "paths": ["packages/billing/**"]},
                      {"name": "search", "paths": ["packages/search/**"]}]}"#,
    )
    .await;
    let debt = env.item("gh:acme/widgets#7").await;
    let feature = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.alice, debt, &["billing"], Some(ClaimKind::Debt)).await;
    claim(&env, &env.bob, feature, &["billing"], None).await;
    assert!(
        stored(&env, feature)
            .await
            .unwrap()
            .waits_for(&area("default:billing"))
    );
    env.advance(Duration::days(2) - Duration::hours(1));
    claim(&env, &env.bob, feature, &[], None).await;
    env.advance(Duration::hours(2) + Duration::minutes(1));
    let before = env.events().await.len();

    assert_eq!(sweep(&env).await.released, 1);

    assert_eq!(stored(&env, debt).await, None);
    let waiting = stored(&env, feature).await.unwrap();
    assert!(waiting.holds(&area("default:billing")), "{waiting:?}");
    assert!(!waiting.waits_for(&area("default:billing")));
    let kinds = env.kinds_after(before).await;
    assert!(
        kinds.iter().any(|kind| matches!(
            kind,
            EventKind::AreaHandedOver(passing) if passing.from == debt && passing.to == feature
        )),
        "{kinds:?}"
    );
}

#[tokio::test]
async fn a_project_whose_settings_do_not_parse_fails_alone() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#42").await;
    claim(&env, &env.bob, item, &["billing"], None).await;
    let unknown = r#"{"hot": [{"paths": ["**/*.lock"], "kind": "rebuild-on-merge"}]}"#;
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.put_settings(env.project, &document(unknown))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let created = InitProject::new(&env.deps)
        .execute(InitProjectInput {
            slug: ProjectSlug::parse("gadgets").unwrap(),
            admin: new_member("carol"),
        })
        .await
        .unwrap();
    let carol = Authenticate::new(&env.deps)
        .execute(created.token.expose())
        .await
        .unwrap();
    let carol_session = session(&env.deps, &carol, "laptop", "laptop/0c0ffee0").await;
    let gadget = CreateItem::new(&env.deps)
        .execute(
            &carol,
            CreateItemInput {
                external_ref: Some(ExternalRef::parse("gh:acme/gadgets#1").unwrap()),
                title: Title::parse("Gadget work").unwrap(),
            },
        )
        .await
        .unwrap()
        .item
        .item
        .id;
    ClaimItem::new(&env.deps)
        .execute(
            &carol,
            ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item: gadget,
                session: carol_session,
                areas: Vec::new(),
                kind: None,
            },
        )
        .await
        .unwrap();
    env.advance(Duration::days(8));

    let swept = sweep(&env).await;

    assert_eq!(swept.failed.len(), 1, "{:?}", swept.failed);
    assert_eq!(swept.failed[0].0, env.project);
    assert_eq!(swept.failed[0].1.code(), "internal");
    assert_eq!(
        (swept.projects, swept.released),
        (1, 1),
        "the other project is swept"
    );
    assert!(
        stored(&env, item).await.is_some(),
        "nothing changed where it failed"
    );
}

#[test]
fn the_holder_is_told_why_the_claim_ended_and_how_to_take_it_again() {
    let item = ItemName {
        id: ItemId(7),
        external_ref: Some(ExternalRef::parse("gh:acme/widgets#42").unwrap()),
        title: Title::parse("Refund rounding").unwrap(),
        link: None,
    };
    assert_eq!(
        released_stale(&item, Duration::days(7)),
        "Released: dibs released your claim on work item gh:acme/widgets#42 \"Refund rounding\" because nothing of its work showed for 7d: no edit made under it, no push of its branch, no renewal. The item is open again. If the work goes on, claim it again (dibs claim gh:acme/widgets#42)."
    );
    let local = ItemName {
        external_ref: None,
        ..item
    };
    let text = released_stale(&local, Duration::days(3) + Duration::hours(12));
    assert!(
        text.contains(
            "work item #7 \"Refund rounding\" because nothing of its work showed for 3d12h:"
        ),
        "{text}"
    );
    assert!(text.ends_with("(dibs claim 7)."), "{text}");
}
