//! The slot use cases over the in-memory store: each applies what time changed before it decides,
//! records one event per change, and is refused to whoever may not do it.

use time::Duration;

use dibs_domain::event::{EventKind, ForceCause, SlotForceReleased, SlotMember, SlotReleased};
use dibs_domain::ids::{RunId, SessionId};
use dibs_domain::member::Role;
use dibs_domain::message::Body;
use dibs_domain::slot::{CommitSha, EndWord, Purpose, RunExit, SlotName, WorkflowName};

use super::*;
use crate::actor::Actor;
use crate::use_cases::testing::{Env, Person, document, new_member, session};
use crate::use_cases::{
    ApplySettings, Authenticate, CreateMember, CreateMemberInput, Heartbeat, Inbox, InboxQuery,
    ReadThread, Status,
};

const SLOTS: &str = r#"{
    "slots": [
        {"name": "deploy:prod", "max_hold": "30m", "max_wait": "1h",
         "commands": [{"matches": "^deploy\\.sh prod"}], "freezes": ["refs/heads/main"]},
        {"name": "db:staging", "max_hold": "1h", "group": "staging"},
        {"name": "deploy:staging", "max_hold": "20m", "group": "staging"}
    ]
}"#;

fn slot(name: &str) -> SlotName {
    SlotName::parse(name).unwrap()
}

async fn env() -> (Env, Person) {
    let env = Env::with_settings(SLOTS).await;
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
    let carol = Authenticate::new(&env.deps)
        .execute(token.expose())
        .await
        .unwrap();
    let carol_session = session(&env.deps, &carol, "tablet", "tablet/c4r01").await;
    (
        env,
        Person {
            actor: carol,
            session: carol_session,
        },
    )
}

async fn take(env: &Env, who: &Person, name: &str, mode: HoldMode) -> TookSlot {
    take_in(env, &who.actor, who.session, name, mode)
        .await
        .unwrap()
}

async fn take_in(
    env: &Env,
    actor: &Actor,
    session: SessionId,
    name: &str,
    mode: HoldMode,
) -> Result<TookSlot, crate::AppError> {
    TakeSlot::new(&env.deps)
        .execute(
            actor,
            TakeSlotInput {
                slot: slot(name),
                session,
                purpose: Purpose::parse("ship 2.4").unwrap(),
                item: None,
                mode,
            },
        )
        .await
}

async fn release(env: &Env, who: &Person, name: &str) -> Result<EndedHold, crate::AppError> {
    ReleaseSlot::new(&env.deps)
        .execute(
            &who.actor,
            ReleaseSlotInput {
                slot: slot(name),
                run: None,
                exit: None,
            },
        )
        .await
}

async fn line(env: &Env, name: &str) -> SlotLine {
    Slots::new(&env.deps)
        .one(&env.alice.actor, &slot(name))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_member_takes_a_free_slot_and_the_next_one_queues() {
    let (env, carol) = env().await;
    let before = env.events().await.len();
    let alice = take(&env, &env.alice, "deploy:prod", HoldMode::Session).await;
    assert_eq!(alice.kind, TookKind::Taken);
    assert_eq!(alice.run, None);
    let hold = alice.line.hold.as_ref().unwrap();
    assert_eq!(hold.holder.as_str(), "alice");
    assert_eq!(hold.hold.expires_at, env.now() + Duration::minutes(30));
    let bob = take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    let carol = take(&env, &carol, "deploy:prod", HoldMode::Session).await;
    assert_eq!(bob.kind, TookKind::Queued { position: 1 });
    assert_eq!(carol.kind, TookKind::Queued { position: 2 });
    let queue: Vec<&str> = carol.line.queue.iter().map(|w| w.member.as_str()).collect();
    assert_eq!(queue, ["bob", "carol"]);
    let kinds: Vec<&str> = env
        .kinds_after(before)
        .await
        .iter()
        .map(EventKind::name)
        .collect();
    assert_eq!(kinds, ["slot_taken", "slot_queued", "slot_queued"]);
    let calls = env.fakes.notifier.calls().len();
    assert!(calls >= 3, "every change wakes the followers");
}

#[tokio::test]
async fn a_hold_is_the_members_from_any_of_their_sessions() {
    let (env, _) = env().await;
    take(&env, &env.alice, "deploy:prod", HoldMode::Session).await;
    let other = session(&env.deps, &env.alice.actor, "laptop", "laptop/mcp").await;
    let again = take_in(
        &env,
        &env.alice.actor,
        other,
        "deploy:prod",
        HoldMode::Session,
    )
    .await
    .unwrap();
    assert_eq!(again.kind, TookKind::Renewed);
    assert_eq!(again.line.hold.unwrap().hold.session, other);
    let released = release(&env, &env.alice, "deploy:prod").await.unwrap();
    assert_eq!(released.hold.holder.as_str(), "alice");
}

#[tokio::test]
async fn a_release_offers_the_slot_to_the_first_waiter_who_takes_it() {
    let (env, carol) = env().await;
    take(&env, &env.alice, "deploy:prod", HoldMode::Session).await;
    take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    let before = env.events().await.len();
    let ended = release(&env, &env.alice, "deploy:prod").await.unwrap();
    let offer = ended.line.offer.unwrap();
    assert_eq!(offer.member.as_str(), "bob");
    assert_eq!(offer.offer.until, env.now() + Duration::minutes(5));
    let carol_took = take(&env, &carol, "deploy:prod", HoldMode::Session).await;
    assert_eq!(carol_took.kind, TookKind::Queued { position: 1 });
    let bob_took = take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    assert_eq!(bob_took.kind, TookKind::Taken);
    let kinds: Vec<&str> = env
        .kinds_after(before)
        .await
        .iter()
        .map(EventKind::name)
        .collect();
    assert_eq!(
        kinds,
        ["slot_released", "slot_offered", "slot_queued", "slot_taken"]
    );
}

#[tokio::test]
async fn every_slot_use_case_applies_what_time_changed_first() {
    let (env, carol) = env().await;
    take(&env, &env.alice, "deploy:prod", HoldMode::Session).await;
    take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    env.advance(Duration::minutes(31));
    let before = env.events().await.len();
    let carol_took = take(&env, &carol, "deploy:prod", HoldMode::Session).await;
    assert_eq!(carol_took.kind, TookKind::Queued { position: 1 });
    assert_eq!(
        carol_took.line.offer.map(|o| o.member.to_string()),
        Some("bob".to_owned())
    );
    let kinds = env.kinds_after(before).await;
    assert_eq!(
        kinds[..2],
        [
            EventKind::SlotExpired(SlotMember {
                slot: slot("deploy:prod"),
                member: env.alice.actor.member,
            }),
            EventKind::SlotOffered(SlotMember {
                slot: slot("deploy:prod"),
                member: env.bob.actor.member,
            }),
        ]
    );
    assert_eq!(
        release(&env, &env.alice, "deploy:prod")
            .await
            .unwrap_err()
            .code(),
        "slot_not_held",
        "the expired hold is gone for the release too"
    );
}

#[tokio::test]
async fn refusals_carry_their_codes_and_name_the_holder() {
    let (env, _) = env().await;
    assert_eq!(
        take_in(
            &env,
            &env.alice.actor,
            env.alice.session,
            "ghost",
            HoldMode::Session
        )
        .await
        .unwrap_err()
        .code(),
        "unknown_slot"
    );
    take(&env, &env.alice, "db:staging", HoldMode::Session).await;
    assert_eq!(
        take_in(
            &env,
            &env.alice.actor,
            env.alice.session,
            "deploy:staging",
            HoldMode::Session
        )
        .await
        .unwrap_err()
        .code(),
        "holds_other_slot"
    );
    let not_holder = release(&env, &env.bob, "db:staging").await.unwrap_err();
    assert_eq!(not_holder.code(), "not_holder");
    assert!(matches!(
        &not_holder,
        crate::AppError::Slot(crate::named::Refusal { member: Some(handle), .. })
            if handle.as_str() == "alice"
    ));
    assert_eq!(
        LeaveSlotQueue::new(&env.deps)
            .execute(&env.bob.actor, &slot("db:staging"))
            .await
            .unwrap_err()
            .code(),
        "not_waiting"
    );
    assert_eq!(
        take_in(
            &env,
            &env.bob.actor,
            env.alice.session,
            "deploy:prod",
            HoldMode::Session
        )
        .await
        .unwrap_err()
        .code(),
        "not_found",
        "a session of another member"
    );
}

#[tokio::test]
async fn an_integration_is_refused_everything_but_reading_and_reporting_runs() {
    let (env, _) = env().await;
    let bot = &env.importer;
    let refused = [
        take_in(
            &env,
            bot,
            env.alice.session,
            "deploy:prod",
            HoldMode::Session,
        )
        .await
        .map(|_| ()),
        ReleaseSlot::new(&env.deps)
            .execute(
                bot,
                ReleaseSlotInput {
                    slot: slot("deploy:prod"),
                    run: None,
                    exit: None,
                },
            )
            .await
            .map(|_| ()),
        LeaveSlotQueue::new(&env.deps)
            .execute(bot, &slot("deploy:prod"))
            .await
            .map(|_| ()),
        SlotRunBeat::new(&env.deps)
            .execute(bot, &slot("deploy:prod"), RunId(1))
            .await
            .map(|_| ()),
        ForceReleaseSlot::new(&env.deps)
            .execute(
                bot,
                ForceReleaseSlotInput {
                    slot: slot("deploy:prod"),
                    session: env.alice.session,
                    reason: Body::parse("x").unwrap(),
                },
            )
            .await
            .map(|_| ()),
    ];
    for (n, result) in refused.into_iter().enumerate() {
        assert_eq!(result.unwrap_err().code(), "forbidden", "case {n}");
    }
    assert_eq!(Slots::new(&env.deps).execute(bot).await.unwrap().len(), 3);
    let report = RunFinishedInput {
        slot: slot("deploy:prod"),
        workflow: WorkflowName::parse("deploy.yml").unwrap(),
        sha: CommitSha::parse("abc1234").unwrap(),
        conclusion: EndWord::parse("success").unwrap(),
    };
    assert!(
        !SlotRunFinished::new(&env.deps)
            .execute(bot, report.clone())
            .await
            .unwrap()
    );
    assert_eq!(
        SlotRunFinished::new(&env.deps)
            .execute(&env.bob.actor, report)
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
}

#[tokio::test]
async fn an_until_run_hold_ends_on_the_workers_report_once() {
    let (env, _) = env().await;
    let workflow = WorkflowName::parse("deploy.yml").unwrap();
    let sha = CommitSha::parse("abc1234").unwrap();
    take(
        &env,
        &env.alice,
        "deploy:prod",
        HoldMode::UntilRun {
            workflow: workflow.clone(),
            sha: sha.clone(),
        },
    )
    .await;
    let report = RunFinishedInput {
        slot: slot("deploy:prod"),
        workflow,
        sha,
        conclusion: EndWord::parse("success").unwrap(),
    };
    let finished = SlotRunFinished::new(&env.deps);
    let before = env.events().await.len();
    assert!(
        finished
            .execute(&env.importer, report.clone())
            .await
            .unwrap()
    );
    assert!(!finished.execute(&env.importer, report).await.unwrap());
    assert_eq!(
        env.kinds_after(before).await,
        [EventKind::SlotReleased(SlotReleased {
            slot: slot("deploy:prod"),
            member: env.alice.actor.member,
            exit: Some(RunExit::Conclusion(EndWord::parse("success").unwrap())),
        })]
    );
}

#[tokio::test]
async fn a_run_beats_releases_with_its_exit_and_learns_of_a_takeover() {
    let (env, _) = env().await;
    let took = take(&env, &env.alice, "deploy:prod", HoldMode::Run).await;
    let run = took.run.unwrap();
    let beat = SlotRunBeat::new(&env.deps);
    env.advance(Duration::minutes(3));
    let stalled = line(&env, "deploy:prod").await;
    assert!(stalled.hold.unwrap().hold.is_stalled());
    let resumed = beat
        .execute(&env.alice.actor, &slot("deploy:prod"), run)
        .await
        .unwrap();
    assert!(!resumed.hold.unwrap().hold.is_stalled());
    env.advance(Duration::minutes(3));
    line(&env, "deploy:prod").await;
    let taken = TakeOverSlot::new(&env.deps)
        .execute(
            &env.bob.actor,
            TakeOverSlotInput {
                slot: slot("deploy:prod"),
                session: env.bob.session,
                purpose: Purpose::parse("finish the deploy").unwrap(),
                reason: Body::parse("checked the environment: the deploy had stopped").unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(taken.previous.holder.as_str(), "alice");
    assert_eq!(taken.holder.as_str(), "bob");
    let refused = beat
        .execute(&env.alice.actor, &slot("deploy:prod"), run)
        .await
        .unwrap_err();
    assert_eq!(refused.code(), "taken_over");
    assert!(matches!(
        &refused,
        crate::AppError::Beat(crate::named::Refusal { member: Some(h), .. }) if h.as_str() == "bob"
    ));
    let thread = ReadThread::new(&env.deps)
        .execute(&env.alice.actor, taken.thread)
        .await
        .unwrap();
    assert_eq!(
        thread.messages[0].record.body.as_str(),
        "checked the environment: the deploy had stopped"
    );
    let ended = ReleaseSlot::new(&env.deps)
        .execute(
            &env.alice.actor,
            ReleaseSlotInput {
                slot: slot("deploy:prod"),
                run: Some(run),
                exit: Some(RunExit::Code(0)),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        ended.code(),
        "not_holder",
        "the run releases nothing of bob's"
    );
}

#[tokio::test]
async fn a_run_releases_its_own_hold_with_its_exit() {
    let (env, _) = env().await;
    let run = take(&env, &env.alice, "deploy:prod", HoldMode::Run)
        .await
        .run
        .unwrap();
    let before = env.events().await.len();
    ReleaseSlot::new(&env.deps)
        .execute(
            &env.alice.actor,
            ReleaseSlotInput {
                slot: slot("deploy:prod"),
                run: Some(run),
                exit: Some(RunExit::Code(3)),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        env.kinds_after(before).await,
        [EventKind::SlotReleased(SlotReleased {
            slot: slot("deploy:prod"),
            member: env.alice.actor.member,
            exit: Some(RunExit::Code(3)),
        })]
    );
}

#[tokio::test]
async fn an_admin_forces_a_release_and_the_holder_is_told_why() {
    let (env, _) = env().await;
    take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    let input = || ForceReleaseSlotInput {
        slot: slot("deploy:prod"),
        session: env.alice.session,
        reason: Body::parse("the release is cancelled").unwrap(),
    };
    let force = ForceReleaseSlot::new(&env.deps);
    assert_eq!(
        force
            .execute(&env.bob.actor, input())
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
    let before = env.events().await.len();
    let forced = force.execute(&env.alice.actor, input()).await.unwrap();
    assert_eq!(forced.ended.hold.holder.as_str(), "bob");
    let kinds = env.kinds_after(before).await;
    assert_eq!(
        kinds[0],
        EventKind::SlotForceReleased(SlotForceReleased {
            slot: slot("deploy:prod"),
            member: env.bob.actor.member,
            by: env.alice.actor.member,
            cause: ForceCause::Admin,
        })
    );
    let thread = ReadThread::new(&env.deps)
        .execute(&env.bob.actor, forced.thread.unwrap())
        .await
        .unwrap();
    assert_eq!(
        thread.messages[0].record.body.as_str(),
        "the release is cancelled"
    );
    assert_eq!(
        force
            .execute(&env.alice.actor, input())
            .await
            .unwrap_err()
            .code(),
        "slot_not_held"
    );
}

#[tokio::test]
async fn the_sweep_offers_a_freed_slot_with_nobody_touching_it() {
    let (env, _) = env().await;
    take(&env, &env.alice, "deploy:prod", HoldMode::Session).await;
    take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    let sweep = TickSlots::new(&env.deps);
    let quiet = sweep.execute().await.unwrap();
    assert_eq!((quiet.projects, quiet.events), (1, 0));
    env.advance(Duration::minutes(31));
    let before = env.events().await.len();
    let calls = env.fakes.notifier.calls().len();
    let swept = sweep.execute().await.unwrap();
    assert_eq!((swept.projects, swept.events), (1, 2));
    assert!(swept.failed.is_empty());
    let kinds: Vec<&str> = env
        .kinds_after(before)
        .await
        .iter()
        .map(EventKind::name)
        .collect();
    assert_eq!(kinds, ["slot_expired", "slot_offered"]);
    assert_eq!(env.fakes.notifier.calls().len(), calls + 1);
    env.advance(Duration::minutes(6));
    let lapsed = sweep.execute().await.unwrap();
    assert_eq!(lapsed.events, 1, "bob's offer lapses");
    assert_eq!(sweep.execute().await.unwrap().projects, 0, "nothing left");
}

#[tokio::test]
async fn a_heartbeat_keeps_a_waiters_place() {
    let (env, _) = env().await;
    take(&env, &env.alice, "deploy:prod", HoldMode::Run).await;
    take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    for _ in 0..3 {
        env.advance(Duration::minutes(25));
        Heartbeat::new(&env.deps)
            .execute(&env.bob.actor, env.bob.session)
            .await
            .unwrap();
    }
    let now = line(&env, "deploy:prod").await;
    assert_eq!(
        now.position_of(env.bob.actor.member),
        Some(1),
        "75 minutes in"
    );
    env.advance(Duration::minutes(61));
    assert!(line(&env, "deploy:prod").await.queue.is_empty());
}

#[tokio::test]
async fn the_inbox_lists_a_pending_offer_and_drops_it_once_taken_or_lapsed() {
    let (env, carol) = env().await;
    take(&env, &env.alice, "deploy:prod", HoldMode::Session).await;
    take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    take(&env, &carol, "deploy:prod", HoldMode::Session).await;
    let inbox = Inbox::new(&env.deps);
    let offers = |who: &Person| {
        let inbox = &inbox;
        let actor = who.actor.clone();
        async move {
            inbox
                .execute(&actor, InboxQuery::default())
                .await
                .unwrap()
                .offers
        }
    };
    assert!(offers(&env.bob).await.is_empty());
    release(&env, &env.alice, "deploy:prod").await.unwrap();
    let pending = offers(&env.bob).await;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].slot, slot("deploy:prod"));
    assert_eq!(pending[0].until, env.now() + Duration::minutes(5));
    assert!(offers(&carol).await.is_empty());
    env.advance(Duration::minutes(5));
    assert!(offers(&env.bob).await.is_empty(), "lapsed");
    let carols = offers(&carol).await;
    assert_eq!(carols.len(), 1, "passed on");
    take(&env, &carol, "deploy:prod", HoldMode::Session).await;
    assert!(offers(&carol).await.is_empty(), "taken");
}

#[tokio::test]
async fn the_status_shows_every_slot() {
    let (env, _) = env().await;
    take(&env, &env.alice, "db:staging", HoldMode::Session).await;
    let view = Status::new(&env.deps)
        .execute(&env.bob.actor)
        .await
        .unwrap();
    let names: Vec<(&str, Option<&str>)> = view
        .slots
        .iter()
        .map(|line| {
            (
                line.spec.name.as_str(),
                line.hold.as_ref().map(|h| h.hold.slot.as_str()),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("deploy:prod", None),
            ("db:staging", Some("db:staging")),
            ("deploy:staging", Some("db:staging")),
        ],
        "a group's hold shows on each of its slots"
    );
}

#[tokio::test]
async fn removing_a_slot_from_the_settings_ends_its_hold_and_queue_in_the_same_change() {
    let (env, _) = env().await;
    take(&env, &env.bob, "deploy:prod", HoldMode::Session).await;
    take(&env, &env.alice, "deploy:prod", HoldMode::Session).await;
    let before = env.events().await.len();
    ApplySettings::new(&env.deps)
        .execute(
            &env.alice.actor,
            document(r#"{"slots":[{"name":"db:staging","max_hold":"1h"}]}"#),
        )
        .await
        .unwrap();
    let kinds: Vec<&str> = env
        .kinds_after(before)
        .await
        .iter()
        .map(EventKind::name)
        .collect();
    assert_eq!(
        kinds,
        [
            "settings_changed",
            "slot_force_released",
            "slot_force_released"
        ]
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(tx.slot_board(env.project).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_commit_takes_nothing_and_wakes_nobody() {
    let (env, _) = env().await;
    let calls = env.fakes.notifier.calls().len();
    let before = env.events().await.len();
    env.fakes.store.fail_next_commit();
    assert_eq!(
        take_in(
            &env,
            &env.alice.actor,
            env.alice.session,
            "deploy:prod",
            HoldMode::Session
        )
        .await
        .unwrap_err()
        .code(),
        "internal"
    );
    assert_eq!(env.events().await.len(), before);
    assert_eq!(env.fakes.notifier.calls().len(), calls);
    assert!(line(&env, "deploy:prod").await.hold.is_none());
}
