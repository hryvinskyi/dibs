//! The slots' state machine and the command verdict, case by case.

use time::{Duration, OffsetDateTime};

use super::*;
use crate::event::{
    EventKind, ForceCause, SlotForceReleased, SlotMember, SlotQueued, SlotReleased,
    SlotStallTakenOver, SlotTaken,
};
use crate::ids::{MemberId, RunId, SessionId, WaiterId};
use crate::repo::RepoId;
use crate::session::HostName;
use crate::settings::ProjectSettings;
use crate::testing::{ALICE, BOB, CAROL, at, settings};

const DAVE: MemberId = MemberId(4);

/// Slots of every shape: `deploy:prod` alone, freezing `main`; `db:staging` and `deploy:staging`
/// in the group `staging`; `migrate` that runs only inside `dibs slot run`.
fn project() -> ProjectSettings {
    settings(
        r#"{"slots":[
            {"name":"deploy:prod","max_hold":"30m","offer_for":"5m","max_wait":"1h",
             "commands":[{"matches":"^deploy\\.sh prod"}],
             "freezes":["refs/heads/main"]},
            {"name":"db:staging","max_hold":"1h","group":"staging",
             "commands":[{"matches":"^psql .*staging"}]},
            {"name":"deploy:staging","max_hold":"20m","group":"staging",
             "commands":[{"matches":"^deploy\\.sh staging"},
                         {"matches":"^make deploy","unless":"--dry-run"}]},
            {"name":"migrate","max_hold":"1h","run_only":true,
             "commands":[{"matches":"^migrate\\b"}]}
        ]}"#,
    )
}

fn name(text: &str) -> SlotName {
    SlotName::parse(text).unwrap()
}

fn request(slot: &str, member: MemberId, liveness: Liveness) -> TakeRequest {
    TakeRequest {
        slot: name(slot),
        member,
        session: SessionId(member.get() * 10),
        host: HostName::parse("laptop").unwrap(),
        purpose: Purpose::parse(&format!("work of {member}")).unwrap(),
        item: None,
        liveness,
        waiter: WaiterId(member.get() * 100),
    }
}

/// A board with one take after another applied, each at its minute.
struct Run {
    catalog: SlotCatalog,
    board: SlotBoard,
}

impl Run {
    fn new() -> Self {
        Self {
            catalog: project().slots().clone(),
            board: SlotBoard::default(),
        }
    }

    fn take(&mut self, slot: &str, member: MemberId, minute: i64) -> (TakeAnswer, Vec<EventKind>) {
        self.take_as(slot, member, Liveness::Session, minute)
    }

    fn take_as(
        &mut self,
        slot: &str,
        member: MemberId,
        liveness: Liveness,
        minute: i64,
    ) -> (TakeAnswer, Vec<EventKind>) {
        let done = decide_take(
            &self.board,
            &self.catalog,
            &request(slot, member, liveness),
            at(minute),
        )
        .unwrap();
        self.board = done.board;
        (done.answer, done.events)
    }

    fn tick(&mut self, now: OffsetDateTime) -> Vec<EventKind> {
        let done = decide_tick(&self.board, &self.catalog, now);
        self.board = done.board;
        done.events
    }

    fn release(&mut self, slot: &str, member: MemberId, minute: i64) -> Vec<EventKind> {
        let done = decide_release(
            &self.board,
            &self.catalog,
            &name(slot),
            member,
            None,
            None,
            at(minute),
        )
        .unwrap();
        self.board = done.board;
        done.events
    }
}

fn member_of(slot: &str, member: MemberId) -> SlotMember {
    SlotMember {
        slot: name(slot),
        member,
    }
}

fn place(answer: &TakeAnswer) -> Option<usize> {
    match answer {
        TakeAnswer::Queued { position, .. } => Some(*position),
        TakeAnswer::Taken { .. } | TakeAnswer::Renewed { .. } => None,
    }
}

#[test]
fn a_free_slot_is_taken_and_a_held_one_queues_in_order() {
    let mut run = Run::new();
    let (taken, events) = run.take("deploy:prod", ALICE, 0);
    let TakeAnswer::Taken { hold } = taken else {
        panic!("not taken: {taken:?}");
    };
    assert_eq!(hold.expires_at, at(30));
    assert_eq!(
        events,
        [EventKind::SlotTaken(SlotTaken {
            slot: name("deploy:prod"),
            member: ALICE,
            purpose: Purpose::parse("work of 1").unwrap(),
        })]
    );
    let (bob, bob_events) = run.take("deploy:prod", BOB, 1);
    let (carol, _) = run.take("deploy:prod", CAROL, 2);
    assert_eq!(place(&bob), Some(1));
    assert_eq!(place(&carol), Some(2));
    assert!(matches!(
        bob_events[0],
        EventKind::SlotQueued(SlotQueued { member: BOB, .. })
    ));
    let TakeAnswer::Queued { hold, offer, .. } = bob else {
        panic!("not queued");
    };
    assert_eq!(hold.map(|hold| hold.member), Some(ALICE));
    assert_eq!(offer, None);
}

#[test]
fn a_member_is_in_a_queue_once_and_asking_again_keeps_the_place() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    run.take("deploy:prod", BOB, 1);
    run.take("deploy:prod", CAROL, 2);
    let (again, events) = run.take("deploy:prod", BOB, 3);
    assert_eq!(place(&again), Some(1));
    assert!(events.is_empty(), "asking again records nothing");
    assert_eq!(run.board.waiters.len(), 2);
    let bob = run.board.waiters.iter().find(|w| w.member == BOB).unwrap();
    assert_eq!((bob.since, bob.last_seen), (at(1), at(3)));
}

#[test]
fn the_holder_renews_up_to_max_hold_and_no_further_while_somebody_waits() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    let (renewed, events) = run.take("deploy:prod", ALICE, 20);
    let TakeAnswer::Renewed { hold } = renewed else {
        panic!("not renewed");
    };
    assert_eq!(hold.expires_at, at(50), "now + max_hold");
    assert_eq!(
        events,
        [EventKind::SlotRenewed(member_of("deploy:prod", ALICE))]
    );
    run.take("deploy:prod", BOB, 25);
    let (capped, _) = run.take("deploy:prod", ALICE, 45);
    let TakeAnswer::Renewed { hold } = capped else {
        panic!("not renewed");
    };
    assert_eq!(
        hold.expires_at,
        at(60),
        "taken_at + 2 × max_hold while bob waits"
    );
    let (again, _) = run.take("deploy:prod", ALICE, 59);
    let TakeAnswer::Renewed { hold } = again else {
        panic!("not renewed");
    };
    assert_eq!(
        hold.expires_at,
        at(60),
        "never beyond the cap, never shortened"
    );
    assert!(hold.expires_at <= at(59) + Duration::minutes(30));
}

#[test]
fn a_renewal_without_waiters_never_exceeds_max_hold_from_now() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    for minute in [10, 20, 100, 500] {
        let (answer, _) = run.take("deploy:prod", ALICE, minute);
        let TakeAnswer::Renewed { hold } = answer else {
            panic!("not renewed");
        };
        assert_eq!(hold.expires_at, at(minute + 30));
    }
}

#[test]
fn every_way_a_hold_ends_offers_the_slot_to_the_first_waiter() {
    type End = fn(&mut Run) -> Vec<EventKind>;
    let ends: [(&str, End); 4] = [
        ("release", |run| run.release("deploy:prod", ALICE, 5)),
        ("expiry", |run| run.tick(at(31))),
        ("force release", |run| {
            let done =
                decide_force_release(&run.board, &run.catalog, &name("deploy:prod"), DAVE, at(5))
                    .unwrap();
            run.board = done.board;
            done.events
        }),
        ("removal", |run| {
            let empty = settings("{}").slots().clone();
            let done = decide_slots_removed(&run.board, &empty, DAVE, at(5));
            run.board = done.board;
            done.events
        }),
    ];
    for (how, end) in ends {
        let mut run = Run::new();
        run.take("deploy:prod", ALICE, 0);
        run.take("deploy:prod", BOB, 1);
        run.take("deploy:prod", CAROL, 2);
        let events = end(&mut run);
        if how == "removal" {
            assert!(run.board.is_empty(), "{how}: {events:?}");
            assert_eq!(events.len(), 3, "{how}");
            assert!(events.iter().all(|event| matches!(
                event,
                EventKind::SlotForceReleased(SlotForceReleased {
                    cause: ForceCause::SlotRemoved,
                    by: Some(DAVE),
                    ..
                })
            )));
            continue;
        }
        assert_eq!(events.len(), 2, "{how}: {events:?}");
        assert_eq!(
            events[1],
            EventKind::SlotOffered(member_of("deploy:prod", BOB)),
            "{how}"
        );
        assert_eq!(run.board.offers.len(), 1, "{how}");
        assert_eq!(run.board.offers[0].member, BOB, "{how}");
        assert_eq!(run.board.waiters.len(), 1, "{how}: carol still waits");
    }
}

#[test]
fn ending_events_name_what_ended() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    assert_eq!(
        run.release("deploy:prod", ALICE, 1),
        [EventKind::SlotReleased(SlotReleased {
            slot: name("deploy:prod"),
            member: ALICE,
            exit: None,
        })]
    );
    run.take("deploy:prod", ALICE, 2);
    assert_eq!(
        run.tick(at(32)),
        [EventKind::SlotExpired(member_of("deploy:prod", ALICE))]
    );
    assert!(run.board.is_empty(), "a slot nobody waits for is free");
}

#[test]
fn during_an_offer_only_the_offered_member_takes_and_others_queue_behind() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    run.take("deploy:prod", BOB, 1);
    run.release("deploy:prod", ALICE, 2);
    let (carol, _) = run.take("deploy:prod", CAROL, 3);
    let TakeAnswer::Queued {
        position,
        hold,
        offer,
    } = carol
    else {
        panic!("carol is not queued");
    };
    assert_eq!((position, hold), (1, None));
    assert_eq!(offer.map(|o| o.member), Some(BOB));
    let (alice, _) = run.take("deploy:prod", ALICE, 4);
    assert_eq!(place(&alice), Some(2), "the former holder waits too");
    let (bob, _) = run.take("deploy:prod", BOB, 5);
    assert!(matches!(bob, TakeAnswer::Taken { .. }));
    assert!(run.board.offers.is_empty());
}

#[test]
fn an_offer_lapses_to_the_next_waiter_and_the_lapsed_member_is_removed() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    run.take("deploy:prod", BOB, 1);
    run.take("deploy:prod", CAROL, 2);
    run.release("deploy:prod", ALICE, 10);
    assert_eq!(run.board.offers[0].until, at(15));
    assert!(run.tick(at(14)).is_empty(), "the offer still stands");
    assert_eq!(
        run.tick(at(15)),
        [
            EventKind::SlotOfferLapsed(member_of("deploy:prod", BOB)),
            EventKind::SlotOffered(member_of("deploy:prod", CAROL)),
        ]
    );
    assert!(run.board.waiters.is_empty());
    assert!(run.board.offers.iter().all(|offer| offer.member == CAROL));
}

#[test]
fn a_waiter_silent_longer_than_max_wait_loses_the_place() {
    let stalled = || {
        let mut run = Run::new();
        run.take_as("deploy:prod", ALICE, Liveness::Run { run: RunId(1) }, 0);
        run.take("deploy:prod", BOB, 1);
        run.take("deploy:prod", CAROL, 20);
        run.tick(at(5));
        run
    };
    let mut run = stalled();
    assert!(run.tick(at(61)).is_empty(), "bob is within the hour");
    assert_eq!(
        run.tick(at(62)),
        [EventKind::SlotLeft(member_of("deploy:prod", BOB))]
    );
    let mut refreshed = stalled();
    assert!(refreshed.board.refresh_member(BOB, at(50)));
    assert!(
        refreshed.tick(at(62)).is_empty(),
        "a heartbeat keeps the place"
    );
}

#[test]
fn a_waiter_leaves_and_an_offered_member_passes_the_offer_on() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    run.take("deploy:prod", BOB, 1);
    run.take("deploy:prod", CAROL, 2);
    let left = decide_leave(&run.board, &run.catalog, &name("deploy:prod"), CAROL, at(3)).unwrap();
    assert_eq!(
        left.events,
        [EventKind::SlotLeft(member_of("deploy:prod", CAROL))]
    );
    run.board = left.board;
    assert_eq!(
        decide_leave(&run.board, &run.catalog, &name("deploy:prod"), CAROL, at(3)).unwrap_err(),
        SlotRefused::NotWaiting
    );
    run.take("deploy:prod", CAROL, 4);
    run.release("deploy:prod", ALICE, 5);
    let declined =
        decide_leave(&run.board, &run.catalog, &name("deploy:prod"), BOB, at(6)).unwrap();
    assert_eq!(
        declined.events,
        [
            EventKind::SlotLeft(member_of("deploy:prod", BOB)),
            EventKind::SlotOffered(member_of("deploy:prod", CAROL)),
        ]
    );
}

#[test]
fn only_the_holder_releases_and_a_run_releases_only_its_own_hold() {
    let mut run = Run::new();
    let slot = name("deploy:prod");
    assert_eq!(
        decide_release(&run.board, &run.catalog, &slot, ALICE, None, None, at(0)).unwrap_err(),
        SlotRefused::NotHeld
    );
    run.take_as("deploy:prod", ALICE, Liveness::Run { run: RunId(7) }, 0);
    assert_eq!(
        decide_release(&run.board, &run.catalog, &slot, BOB, None, None, at(1)).unwrap_err(),
        SlotRefused::NotHolder { member: ALICE }
    );
    assert_eq!(
        decide_release(
            &run.board,
            &run.catalog,
            &slot,
            ALICE,
            Some(RunId(8)),
            None,
            at(1)
        )
        .unwrap_err(),
        SlotRefused::RunEnded
    );
    let done = decide_release(
        &run.board,
        &run.catalog,
        &slot,
        ALICE,
        Some(RunId(7)),
        Some(RunExit::Code(3)),
        at(1),
    )
    .unwrap();
    assert_eq!(
        done.events,
        [EventKind::SlotReleased(SlotReleased {
            slot,
            member: ALICE,
            exit: Some(RunExit::Code(3)),
        })]
    );
}

#[test]
fn slots_of_one_group_exclude_each_other_with_one_queue_and_one_offer() {
    let mut run = Run::new();
    run.take("db:staging", ALICE, 0);
    let (bob, _) = run.take("deploy:staging", BOB, 1);
    let TakeAnswer::Queued { position, hold, .. } = bob else {
        panic!("bob is not queued: {bob:?}");
    };
    assert_eq!(position, 1);
    assert_eq!(hold.map(|hold| hold.slot), Some(name("db:staging")));
    let (carol, _) = run.take("db:staging", CAROL, 2);
    assert_eq!(place(&carol), Some(2), "one queue across the group");
    let events = run.release("db:staging", ALICE, 3);
    assert_eq!(
        events[1],
        EventKind::SlotOffered(member_of("deploy:staging", BOB)),
        "bob is offered the slot he asked for"
    );
    let (carol, _) = run.take("db:staging", CAROL, 4);
    assert_eq!(place(&carol), Some(1), "one offer across the group");
    let (taken, _) = run.take("deploy:staging", BOB, 5);
    assert!(matches!(taken, TakeAnswer::Taken { .. }));
    assert_eq!(
        decide_take(
            &run.board,
            &run.catalog,
            &request("db:staging", BOB, Liveness::Session),
            at(6)
        )
        .unwrap_err(),
        SlotRefused::HoldsOther {
            held: name("deploy:staging")
        }
    );
    let (free, _) = run.take("deploy:prod", BOB, 6);
    assert!(
        matches!(free, TakeAnswer::Taken { .. }),
        "another group is free"
    );
}

#[test]
fn a_session_hold_never_stalls_and_an_until_run_hold_ignores_missing_heartbeats() {
    let mut run = Run::new();
    run.take("db:staging", ALICE, 0);
    let workflow = WorkflowName::parse("deploy.yml").unwrap();
    let sha = CommitSha::parse("abc1234").unwrap();
    run.take_as(
        "deploy:prod",
        BOB,
        Liveness::UntilRun {
            workflow: workflow.clone(),
            sha: sha.clone(),
        },
        0,
    );
    assert!(run.tick(at(29)).is_empty(), "no heartbeat, no stall");
    assert_eq!(
        run.tick(at(30)),
        [EventKind::SlotExpired(member_of("deploy:prod", BOB))],
        "an until-run hold still ends at its expiry"
    );
    assert!(
        run.board
            .hold(&name("db:staging"))
            .is_some_and(|h| !h.is_stalled())
    );
}

#[test]
fn a_run_hold_stalls_after_two_minutes_offers_nothing_and_resumes_on_its_runs_beat() {
    let mut run = Run::new();
    let slot = name("deploy:prod");
    run.take_as("deploy:prod", ALICE, Liveness::Run { run: RunId(7) }, 0);
    run.take("deploy:prod", BOB, 1);
    assert!(
        run.tick(at(0) + Duration::seconds(120)).is_empty(),
        "two minutes is still live"
    );
    let stalled_at = at(2) + Duration::seconds(1);
    assert_eq!(
        run.tick(stalled_at),
        [EventKind::SlotStalled(member_of("deploy:prod", ALICE))]
    );
    assert!(run.tick(at(40)).is_empty(), "stalled: no expiry, no offer");
    assert!(run.board.offers.is_empty());
    assert!(run.board.hold(&slot).unwrap().is_stalled());
    assert_eq!(
        decide_run_beat(&run.board, &slot, ALICE, RunId(8), at(41)).unwrap_err(),
        BeatRefused::Ended,
        "another run's beat resumes nothing"
    );
    let resumed = decide_run_beat(&run.board, &slot, ALICE, RunId(7), at(41)).unwrap();
    assert_eq!(
        resumed.events,
        [EventKind::SlotResumed(member_of("deploy:prod", ALICE))]
    );
    run.board = resumed.board;
    assert_eq!(
        run.tick(at(41)),
        [
            EventKind::SlotExpired(member_of("deploy:prod", ALICE)),
            EventKind::SlotOffered(member_of("deploy:prod", BOB)),
        ],
        "live again, its expiry applies"
    );
}

#[test]
fn a_stalled_hold_is_taken_over_only_by_another_member_and_only_while_stalled() {
    let mut run = Run::new();
    let slot = name("deploy:prod");
    run.take_as("deploy:prod", ALICE, Liveness::Run { run: RunId(7) }, 0);
    let bob = request("deploy:prod", BOB, Liveness::Session);
    assert_eq!(
        decide_stall_takeover(&run.board, &run.catalog, &bob, at(1)).unwrap_err(),
        SlotRefused::NotStalled {
            member: ALICE,
            last_seen: at(0)
        }
    );
    run.tick(at(5));
    let own = request("deploy:prod", ALICE, Liveness::Session);
    assert_eq!(
        decide_stall_takeover(&run.board, &run.catalog, &own, at(6)).unwrap_err(),
        SlotRefused::OwnHold
    );
    let taken = decide_stall_takeover(&run.board, &run.catalog, &bob, at(6)).unwrap();
    assert_eq!(
        taken.events,
        [EventKind::SlotStallTakenOver(SlotStallTakenOver {
            slot: slot.clone(),
            member: BOB,
            previous: ALICE,
            purpose: Purpose::parse("work of 2").unwrap(),
        })]
    );
    assert_eq!(taken.answer.member, ALICE);
    assert_eq!(
        decide_run_beat(&taken.board, &slot, ALICE, RunId(7), at(7)).unwrap_err(),
        BeatRefused::TakenOver { by: BOB }
    );
}

#[test]
fn an_until_run_hold_ends_on_its_own_workflow_and_commit_only_and_a_repeat_changes_nothing() {
    let mut run = Run::new();
    let slot = name("deploy:prod");
    let workflow = WorkflowName::parse("deploy.yml").unwrap();
    let sha = CommitSha::parse("abc1234").unwrap();
    let success = EndWord::parse("success").unwrap();
    run.take_as(
        "deploy:prod",
        ALICE,
        Liveness::UntilRun {
            workflow: workflow.clone(),
            sha: sha.clone(),
        },
        0,
    );
    let other_sha = CommitSha::parse("def5678").unwrap();
    let other = WorkflowName::parse("release.yml").unwrap();
    for (w, s) in [(&workflow, &other_sha), (&other, &sha)] {
        let none = decide_run_finished(&run.board, &run.catalog, &slot, w, s, &success, at(1));
        assert_eq!(none.answer, None);
        assert!(none.events.is_empty());
    }
    let done = decide_run_finished(
        &run.board,
        &run.catalog,
        &slot,
        &workflow,
        &sha,
        &success,
        at(1),
    );
    assert_eq!(done.answer.map(|hold| hold.member), Some(ALICE));
    assert_eq!(
        done.events,
        [EventKind::SlotReleased(SlotReleased {
            slot: slot.clone(),
            member: ALICE,
            exit: Some(RunExit::Conclusion(success.clone())),
        })]
    );
    let again = decide_run_finished(
        &done.board,
        &run.catalog,
        &slot,
        &workflow,
        &sha,
        &success,
        at(2),
    );
    assert_eq!((again.answer, again.events.len()), (None, 0));
}

#[test]
fn a_plain_take_keeps_a_run_hold_and_a_run_take_replaces_a_plain_one() {
    let mut run = Run::new();
    run.take_as("migrate", ALICE, Liveness::Run { run: RunId(7) }, 0);
    run.take("migrate", ALICE, 1);
    assert!(run.board.hold(&name("migrate")).unwrap().is_run(RunId(7)));
    run.take_as("migrate", ALICE, Liveness::Run { run: RunId(9) }, 2);
    assert!(run.board.hold(&name("migrate")).unwrap().is_run(RunId(9)));
}

#[test]
fn an_unknown_slot_is_refused() {
    let run = Run::new();
    let ghost = request("ghost", ALICE, Liveness::Session);
    assert_eq!(
        decide_take(&run.board, &run.catalog, &ghost, at(0)).unwrap_err(),
        SlotRefused::UnknownSlot {
            slot: name("ghost")
        }
    );
}

#[test]
fn a_settings_change_ends_only_the_rows_of_removed_slots() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    run.take("deploy:prod", BOB, 1);
    run.take("migrate", CAROL, 1);
    let kept = settings(
        r#"{"slots":[{"name":"migrate","max_hold":"1h","commands":[{"matches":"^migrate"}]}]}"#,
    );
    let done = decide_slots_removed(&run.board, kept.slots(), DAVE, at(2));
    assert_eq!(done.events.len(), 2, "{:?}", done.events);
    assert_eq!(done.board.holds.len(), 1);
    assert_eq!(done.board.holds[0].member, CAROL);
    assert!(done.board.waiters.is_empty());
}

#[test]
fn removing_a_member_ends_their_holds_places_and_offers_and_offers_the_freed_slots_on() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    run.take("deploy:prod", BOB, 1);
    run.take("db:staging", CAROL, 1);
    run.take("deploy:staging", ALICE, 2);
    let done = decide_member_removed(&run.board, &run.catalog, ALICE, Some(DAVE), at(3));
    assert_eq!(
        done.answer,
        RemovedFromSlots {
            held: vec![name("deploy:prod")],
            waited: vec![name("deploy:staging")],
            offered: Vec::new(),
        }
    );
    let ended = |slot: &str| {
        EventKind::SlotForceReleased(SlotForceReleased {
            slot: name(slot),
            member: ALICE,
            by: Some(DAVE),
            cause: ForceCause::MemberRemoved,
        })
    };
    assert_eq!(
        done.events,
        [
            ended("deploy:prod"),
            ended("deploy:staging"),
            EventKind::SlotOffered(member_of("deploy:prod", BOB)),
        ]
    );
    assert!(done.board.holds.iter().all(|hold| hold.member != ALICE));
    assert!(
        done.board
            .waiters
            .iter()
            .all(|waiter| waiter.member != ALICE)
    );
    run.board = done.board;

    let offered = decide_member_removed(&run.board, &run.catalog, BOB, None, at(4));
    assert_eq!(offered.answer.offered, [name("deploy:prod")]);
    assert!(offered.board.offers.is_empty(), "nobody else waits");
    let nothing = decide_member_removed(&offered.board, &run.catalog, BOB, None, at(5));
    assert!(nothing.answer.is_empty());
    assert!(nothing.events.is_empty());
}

fn verdict(run: &Run, line: &str, me: MemberId) -> CommandVerdict {
    command_verdict(
        &split_commands(line),
        &RepoId::implicit(),
        &run.catalog,
        &run.board,
        me,
        None,
    )
}

#[test]
fn patterns_match_first_slot_first_and_unless_excludes() {
    let run = Run::new();
    let needs = |line: &str| match verdict(&run, line, ALICE) {
        CommandVerdict::NeedsSlot { slot, holder, .. } => {
            assert_eq!(holder, None, "free slots are still needed");
            Some(slot.to_string())
        }
        CommandVerdict::Free => None,
        other => panic!("{line}: {other:?}"),
    };
    assert_eq!(
        needs("cd ops && ./deploy.sh prod").as_deref(),
        Some("deploy:prod")
    );
    assert_eq!(
        needs("FOO=1 ./deploy.sh staging").as_deref(),
        Some("deploy:staging")
    );
    assert_eq!(needs("make deploy").as_deref(), Some("deploy:staging"));
    assert_eq!(needs("make deploy --dry-run"), None);
    assert_eq!(needs("echo './deploy.sh prod'"), None);
    assert_eq!(needs("ls && git status"), None);
    assert_eq!(
        needs("dibs slot run deploy:prod --purpose x -- ./deploy.sh prod"),
        None,
        "dibs slot run takes the slot itself"
    );
}

#[test]
fn holding_the_slot_lets_the_command_through_except_a_run_only_slot_held_by_a_plain_take() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    assert_eq!(
        verdict(&run, "./deploy.sh prod", ALICE),
        CommandVerdict::Holding {
            slot: name("deploy:prod")
        }
    );
    let CommandVerdict::NeedsSlot {
        holder, waiting, ..
    } = verdict(&run, "./deploy.sh prod", BOB)
    else {
        panic!("bob needs the slot");
    };
    assert_eq!((holder.map(|h| h.member), waiting), (Some(ALICE), 0));
    run.take("migrate", ALICE, 0);
    assert!(matches!(
        verdict(&run, "migrate up", ALICE),
        CommandVerdict::NeedsSlot { run_only: true, .. }
    ));
    let mut until = Run::new();
    until.take_as(
        "migrate",
        ALICE,
        Liveness::UntilRun {
            workflow: WorkflowName::parse("migrate.yml").unwrap(),
            sha: CommitSha::parse("abc1234").unwrap(),
        },
        0,
    );
    assert_eq!(
        verdict(&until, "migrate up", ALICE),
        CommandVerdict::Holding {
            slot: name("migrate")
        }
    );
    let mut ran = Run::new();
    ran.take_as("migrate", ALICE, Liveness::Run { run: RunId(1) }, 0);
    assert!(matches!(
        verdict(&ran, "migrate up", ALICE),
        CommandVerdict::Holding { .. }
    ));
}

#[test]
fn a_waiting_member_sees_the_queue_and_an_offered_member_is_told() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    run.take("deploy:prod", BOB, 1);
    assert!(matches!(
        verdict(&run, "./deploy.sh prod", CAROL),
        CommandVerdict::NeedsSlot {
            waiting: 1,
            offered_to_me: false,
            ..
        }
    ));
    run.release("deploy:prod", ALICE, 2);
    assert!(matches!(
        verdict(&run, "./deploy.sh prod", BOB),
        CommandVerdict::NeedsSlot {
            offered_to_me: true,
            holder: None,
            ..
        }
    ));
}

#[test]
fn a_push_to_a_ref_another_members_slot_freezes_is_frozen_first() {
    let mut run = Run::new();
    assert_eq!(
        verdict(&run, "git push origin main", BOB),
        CommandVerdict::Free
    );
    run.take("deploy:prod", ALICE, 0);
    let frozen = |line: &str| {
        matches!(
            verdict(&run, line, BOB),
            CommandVerdict::Frozen { holder: ALICE, .. }
        )
    };
    assert!(frozen("git push origin main"));
    assert!(frozen("git push -u origin feature:main"));
    assert!(frozen("git push --force origin +main"));
    assert!(frozen("git push origin refs/heads/main"));
    assert!(frozen(
        "git add . && git commit -m 'x' && git push origin main"
    ));
    assert!(!frozen("git push origin feature"));
    assert!(
        !frozen("git push"),
        "no refspec: the pre-push check covers it"
    );
    assert!(!frozen("git push origin HEAD"));
    assert!(!frozen("git -C ../other push origin main"));
    assert!(!frozen("echo git push origin main"));
    assert!(
        matches!(
            verdict(&run, "git push origin main", ALICE),
            CommandVerdict::Free
        ),
        "my own hold never freezes me"
    );
}

#[test]
fn a_frozen_push_wins_over_a_slot_the_same_line_needs() {
    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    assert!(matches!(
        verdict(&run, "./deploy.sh staging && git push origin main", BOB),
        CommandVerdict::Frozen { .. }
    ));
}

#[test]
fn push_targets_read_refspecs() {
    let repo = RepoId::parse("api").unwrap();
    let targets = |line: &str| {
        split_commands(line)
            .first()
            .and_then(|command| push_targets(command, &repo))
            .map(|refs| refs.into_iter().map(|r| r.to_string()).collect::<Vec<_>>())
    };
    assert_eq!(
        targets("git push origin main dev"),
        Some(vec![
            "api:refs/heads/main".to_owned(),
            "api:refs/heads/dev".to_owned()
        ])
    );
    assert_eq!(
        targets("git push --repo x -o ci.skip origin :old"),
        Some(vec!["api:refs/heads/old".to_owned()])
    );
    assert_eq!(targets("git push --all origin"), None);
    assert_eq!(targets("git status"), None);
    assert_eq!(targets("git push origin 'feat*'"), None);
}

#[test]
fn a_push_is_judged_by_its_targets_alone_for_the_pre_push_check() {
    let mut run = Run::new();
    let main = RepoRef::of(RepoId::implicit(), "refs/heads/main").unwrap();
    let feature = RepoRef::of(RepoId::implicit(), "refs/heads/feature").unwrap();
    assert_eq!(
        push_verdict(std::slice::from_ref(&main), &run.catalog, &run.board, BOB),
        CommandVerdict::Free
    );
    run.take("deploy:prod", ALICE, 0);
    assert!(matches!(
        push_verdict(&[feature.clone(), main.clone()], &run.catalog, &run.board, BOB),
        CommandVerdict::Frozen { holder: ALICE, ref target, .. } if *target == main
    ));
    assert_eq!(
        push_verdict(&[feature], &run.catalog, &run.board, BOB),
        CommandVerdict::Free
    );
    assert_eq!(
        push_verdict(&[main], &run.catalog, &run.board, ALICE),
        CommandVerdict::Free,
        "my own hold never freezes me"
    );
}

#[test]
fn the_slots_in_question_are_the_ones_a_verdict_reads() {
    let mut run = Run::new();
    let asked = |line: &str| {
        slots_in_question(&split_commands(line), &RepoId::implicit(), &run.catalog)
            .into_iter()
            .map(|slot| slot.to_string())
            .collect::<Vec<_>>()
    };
    assert!(asked("cargo test && git push origin feature").is_empty());
    assert!(asked("dibs slot run deploy:prod -- ./deploy.sh prod").is_empty());
    assert_eq!(asked("git push origin main"), ["deploy:prod"]);
    assert_eq!(asked("./deploy.sh prod"), ["deploy:prod"]);
    assert_eq!(
        asked("git push origin main && ./deploy.sh prod"),
        ["deploy:prod"]
    );
    run.take("deploy:prod", ALICE, 0);
    run.take("deploy:prod", CAROL, 1);
    for line in [
        "git push origin main",
        "./deploy.sh prod",
        "migrate up",
        "echo hi",
        "git push origin feature",
    ] {
        let names = slots_in_question(&split_commands(line), &RepoId::implicit(), &run.catalog);
        let only = SlotBoard {
            holds: run
                .board
                .holds
                .iter()
                .filter(|hold| {
                    names.iter().any(|name| {
                        run.catalog
                            .in_group(&hold.slot, &run.catalog.get(name).unwrap().group_key())
                    })
                })
                .cloned()
                .collect(),
            waiters: run
                .board
                .waiters
                .iter()
                .filter(|waiter| {
                    names.iter().any(|name| {
                        run.catalog
                            .in_group(&waiter.slot, &run.catalog.get(name).unwrap().group_key())
                    })
                })
                .cloned()
                .collect(),
            offers: Vec::new(),
        };
        let partial = command_verdict(
            &split_commands(line),
            &RepoId::implicit(),
            &run.catalog,
            &only,
            BOB,
            None,
        );
        assert_eq!(partial, verdict(&run, line, BOB), "{line}");
    }
}

#[test]
fn a_protected_branch_is_judged_before_any_freeze_and_lets_its_pushers_through() {
    use crate::handle::Handle;
    use crate::push::{GitFacts, PushProtection};

    let mut run = Run::new();
    run.take("deploy:prod", ALICE, 0);
    let mut document = project().to_document();
    document.protected_refs = vec![crate::settings::ProtectedRefDocument {
        target: "refs/heads/main".to_owned(),
        pushers: vec!["alice".to_owned()],
    }];
    let protected =
        ProjectSettings::parse(&document, &crate::hot_rule::HotRuleRegistry::standard()).unwrap();
    let main = RepoRef::of(RepoId::implicit(), "refs/heads/main").unwrap();
    let facts = GitFacts {
        branch: Some("feature".to_owned()),
        config: Vec::new(),
    };
    let here = |_: &str| true;
    let judge = |line: &str, me: MemberId, handle: &str| {
        let handle = Handle::parse(handle).unwrap();
        command_verdict(
            &split_commands(line),
            &RepoId::implicit(),
            &run.catalog,
            &run.board,
            me,
            Some(&PushProtection {
                pusher: &handle,
                settings: &protected,
                facts: Some(&facts),
                here: &here,
            }),
        )
    };
    assert_eq!(
        judge("git push origin main", BOB, "bob"),
        CommandVerdict::NeedsPermission {
            target: main.clone(),
            pushers: [Handle::parse("alice").unwrap()].into(),
            certain: true,
        },
        "refused for permission before the freeze is even looked at"
    );
    assert_eq!(
        judge("git push origin bob/42-rounding", BOB, "bob"),
        CommandVerdict::Free,
        "a branch of one's own is pushed freely"
    );
    assert_eq!(
        judge("git push origin main", ALICE, "alice"),
        CommandVerdict::Free,
        "a pusher passes, and their own hold does not freeze them"
    );
    assert_eq!(
        verdict(&run, "git push origin main", BOB),
        CommandVerdict::Frozen {
            slot: SlotName::parse("deploy:prod").unwrap(),
            holder: ALICE,
            purpose: run.board.holds[0].purpose.clone(),
            until: run.board.holds[0].expires_at,
            target: main,
        },
        "without protection no branch counts as protected"
    );
}
