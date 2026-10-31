//! The worktree pool's state machine, case by case.

use time::Duration;

use super::*;
use crate::event::{EventKind, WorktreeReleaseCause, WorktreeReleased, WorktreeTaken};
use crate::ids::{ItemId, SessionId};
use crate::report::MachineId;
use crate::session::HostName;
use crate::slot::Purpose;
use crate::testing::{ALICE, BOB, at};

const MACHINE: &str = "0123456789abcdef0123456789abcdef";
const POOL: &str = "d41d8cd98f00b204";

fn name(text: &str) -> WorktreeName {
    WorktreeName::parse(text).unwrap()
}

fn names(texts: &[&str]) -> Vec<WorktreeName> {
    texts.iter().map(|text| name(text)).collect()
}

fn key_of(pool: &str) -> PoolKey {
    PoolKey {
        member: ALICE,
        machine: MachineId::parse(MACHINE).unwrap(),
        pool: PoolId::parse(pool).unwrap(),
    }
}

fn session(id: u64) -> Holder {
    Holder::Session(SessionId(id))
}

fn agent(id: u64, key: &str) -> Holder {
    Holder::Agent {
        session: SessionId(id),
        agent: AgentKey::parse(key).unwrap(),
    }
}

fn request(holder: &Holder, offered: &[&str]) -> TakeWorktree {
    TakeWorktree {
        names: names(offered),
        holder: holder.clone(),
        host: HostName::parse("laptop").unwrap(),
        purpose: Purpose::parse("fix the rounding").unwrap(),
        item: Some(ItemId(7)),
    }
}

const ALL: [&str; 3] = ["worktree1", "worktree2", "worktree3"];

/// A pool with takes and other changes applied one after another.
struct Run {
    pool: WorktreePool,
}

impl Run {
    fn new() -> Self {
        Self::of(POOL)
    }

    fn of(pool: &str) -> Self {
        Self {
            pool: WorktreePool::empty(key_of(pool)),
        }
    }

    fn take(&mut self, holder: &Holder, minute: i64) -> (TakeAnswer, Vec<EventKind>) {
        self.take_of(holder, &ALL, minute)
    }

    fn take_of(
        &mut self,
        holder: &Holder,
        offered: &[&str],
        minute: i64,
    ) -> (TakeAnswer, Vec<EventKind>) {
        let done = decide_take(&self.pool, &request(holder, offered), at(minute)).unwrap();
        self.pool = done.pool;
        (done.answer, done.events)
    }

    fn holds(&self) -> Vec<(&str, Holder)> {
        self.pool
            .holds()
            .iter()
            .map(|hold| (hold.worktree.as_str(), hold.holder.clone()))
            .collect()
    }
}

fn took(worktree: &str, again: bool) -> TakeAnswer {
    TakeAnswer::Took {
        worktree: name(worktree),
        again,
    }
}

fn released_event(worktree: &str, holder: &Holder, cause: WorktreeReleaseCause) -> EventKind {
    EventKind::WorktreeReleased(WorktreeReleased {
        member: ALICE,
        host: HostName::parse("laptop").unwrap(),
        worktree: name(worktree),
        holder: holder.kind(),
        purpose: Purpose::parse("fix the rounding").unwrap(),
        cause,
        by: None,
    })
}

#[test]
fn a_take_lends_the_first_free_worktree_and_records_it() {
    let mut run = Run::new();
    let holder = agent(1, "a1");
    let (answer, events) = run.take(&holder, 0);
    assert_eq!(answer, took("worktree1", false));
    assert_eq!(
        events,
        [EventKind::WorktreeTaken(WorktreeTaken {
            member: ALICE,
            host: HostName::parse("laptop").unwrap(),
            worktree: name("worktree1"),
            holder: HolderKind::Agent,
            purpose: Purpose::parse("fix the rounding").unwrap(),
        })]
    );
    let hold = run.pool.hold(&name("worktree1")).unwrap();
    assert_eq!(hold.holder, holder);
    assert_eq!(hold.item, Some(ItemId(7)));
    assert_eq!((hold.taken_at, hold.last_seen), (at(0), at(0)));
    assert_eq!(run.pool.hold_of(&holder), Some(hold));
}

#[test]
fn taking_again_answers_the_same_worktree_without_an_event_and_sees_the_holder_now() {
    let mut run = Run::new();
    let holder = agent(1, "a1");
    run.take(&holder, 0);
    let before = run.pool.clone();
    let (answer, events) = run.take(&holder, 5);
    assert_eq!(answer, took("worktree1", true));
    assert!(events.is_empty());
    let hold = run.pool.hold(&name("worktree1")).unwrap();
    assert_eq!((hold.taken_at, hold.last_seen), (at(0), at(5)));
    assert_eq!(run.pool.holds().len(), before.holds().len());
}

#[test]
fn a_holder_never_holds_two_worktrees_even_when_it_names_others() {
    let mut run = Run::new();
    let holder = session(1);
    run.take_of(&holder, &["worktree2"], 0);
    let (answer, events) = run.take_of(&holder, &["worktree1", "worktree3"], 1);
    assert_eq!(answer, took("worktree2", true));
    assert!(events.is_empty());
    assert_eq!(run.holds(), [("worktree2", holder)]);
}

#[test]
fn the_first_free_worktree_follows_the_order_named_not_the_alphabet() {
    let mut run = Run::new();
    run.take_of(&agent(1, "a1"), &["worktree3"], 0);
    let order = ["worktree3", "worktree10", "worktree2", "worktree1"];
    let (answer, _) = run.take_of(&agent(1, "a2"), &order, 1);
    assert_eq!(answer, took("worktree10", false));
    let (answer, _) = run.take_of(&agent(1, "a3"), &order, 2);
    assert_eq!(answer, took("worktree2", false));
}

#[test]
fn a_session_and_its_agents_are_different_holders() {
    let mut run = Run::new();
    assert_eq!(run.take(&session(1), 0).0, took("worktree1", false));
    assert_eq!(run.take(&agent(1, "a1"), 0).0, took("worktree2", false));
    assert_eq!(run.take(&agent(1, "a2"), 0).0, took("worktree3", false));
    assert_eq!(
        run.holds(),
        [
            ("worktree1", session(1)),
            ("worktree2", agent(1, "a1")),
            ("worktree3", agent(1, "a2")),
        ]
    );
}

#[test]
fn a_take_with_every_named_worktree_held_is_busy_and_names_the_holders_in_order() {
    let mut run = Run::new();
    run.take(&agent(1, "a1"), 0);
    run.take(&agent(1, "a2"), 0);
    let before = run.pool.clone();
    let (answer, events) = run.take_of(&session(2), &["worktree2", "worktree1"], 1);
    assert_eq!(
        answer,
        TakeAnswer::Busy {
            held: vec![
                WorktreeHolder {
                    worktree: name("worktree2"),
                    holder: agent(1, "a2"),
                },
                WorktreeHolder {
                    worktree: name("worktree1"),
                    holder: agent(1, "a1"),
                },
            ],
        }
    );
    assert!(events.is_empty());
    assert_eq!(run.pool, before);
}

#[test]
fn a_take_naming_nothing_or_a_name_twice_is_refused() {
    let pool = WorktreePool::empty(key_of(POOL));
    let empty = request(&session(1), &[]);
    assert_eq!(
        decide_take(&pool, &empty, at(0)),
        Err(WorktreeRefused::NoNames)
    );
    let twice = request(&session(1), &["worktree1", "worktree2", "worktree1"]);
    assert_eq!(
        decide_take(&pool, &twice, at(0)),
        Err(WorktreeRefused::NamedTwice {
            worktree: name("worktree1"),
        })
    );
}

#[test]
fn a_release_by_holder_or_by_name_ends_the_hold() {
    let mut run = Run::new();
    let first = agent(1, "a1");
    let second = agent(1, "a2");
    run.take(&first, 0);
    run.take(&second, 0);

    let done = decide_release(&run.pool, &Release::Holder(first.clone()), ALICE).unwrap();
    assert_eq!(done.answer.worktree, name("worktree1"));
    assert_eq!(
        done.events,
        [released_event(
            "worktree1",
            &first,
            WorktreeReleaseCause::Released
        )]
    );
    assert_eq!(done.pool.hold(&name("worktree1")), None);
    run.pool = done.pool;

    let done = decide_release(&run.pool, &Release::Name(name("worktree2")), ALICE).unwrap();
    assert_eq!(done.answer.holder, second);
    assert!(done.pool.is_empty());
}

#[test]
fn a_release_by_an_admin_names_them_and_a_release_of_nothing_is_refused() {
    let mut run = Run::new();
    run.take(&session(1), 0);
    let done = decide_release(&run.pool, &Release::Name(name("worktree1")), BOB).unwrap();
    let EventKind::WorktreeReleased(released) = &done.events[0] else {
        panic!("{:?}", done.events);
    };
    assert_eq!((released.member, released.by), (ALICE, Some(BOB)));
    assert_eq!(done.events[0].members(), [ALICE, BOB].into());

    let free = Release::Name(name("worktree2"));
    assert_eq!(
        decide_release(&run.pool, &free, ALICE),
        Err(WorktreeRefused::NotHeld)
    );
    let holds_none = Release::Holder(agent(1, "a1"));
    assert_eq!(
        decide_release(&run.pool, &holds_none, ALICE),
        Err(WorktreeRefused::NotHeld)
    );
}

#[test]
fn a_beat_moves_a_hold_forward_never_back_and_answers_the_pairs_that_hold_nothing() {
    let mut run = Run::new();
    let holder = agent(1, "a1");
    run.take(&holder, 10);
    let beat = |worktree: &str, holder: &Holder, minute: i64| WorktreeBeat {
        worktree: name(worktree),
        holder: holder.clone(),
        seen_at: at(minute),
    };
    let done = decide_beat(
        &run.pool,
        &[
            beat("worktree1", &holder, 15),
            beat("worktree1", &holder, 12),
            beat("worktree1", &agent(1, "a2"), 16),
            beat("worktree2", &holder, 16),
            beat("worktree2", &holder, 17),
        ],
    );
    assert!(done.events.is_empty());
    let hold = done.pool.hold(&name("worktree1")).unwrap();
    assert_eq!(hold.last_seen, at(15));
    assert_eq!(
        done.answer,
        Beaten {
            kept: vec![WorktreeHolder {
                worktree: name("worktree1"),
                holder: holder.clone(),
            }],
            unknown: vec![
                WorktreeHolder {
                    worktree: name("worktree1"),
                    holder: agent(1, "a2"),
                },
                WorktreeHolder {
                    worktree: name("worktree2"),
                    holder: holder.clone(),
                },
            ],
        }
    );
    let earlier = decide_beat(&done.pool, &[beat("worktree1", &holder, 1)]);
    assert_eq!(
        earlier.pool.hold(&name("worktree1")).unwrap().last_seen,
        at(15)
    );
    assert_eq!(earlier.answer.kept.len(), 1);
}

#[test]
fn a_tick_ends_an_agents_hold_after_twenty_silent_minutes_and_no_sooner() {
    let mut run = Run::new();
    let holder = agent(1, "a1");
    run.take(&holder, 0);
    let silence = Silence::DEFAULT;
    let boundary = at(20);
    let kept = decide_tick(&run.pool, boundary, silence);
    assert!(kept.events.is_empty() && kept.answer.is_empty());
    assert_eq!(kept.pool, run.pool);

    let ended = decide_tick(&run.pool, boundary + Duration::seconds(1), silence);
    assert_eq!(
        ended.events,
        [released_event(
            "worktree1",
            &holder,
            WorktreeReleaseCause::Silent
        )]
    );
    assert_eq!(ended.answer.len(), 1);
    assert!(ended.pool.is_empty());
}

#[test]
fn a_tick_keeps_a_sessions_hold_for_twelve_silent_hours() {
    let mut run = Run::new();
    let holder = session(1);
    run.take(&holder, 0);
    let silence = Silence::DEFAULT;
    for minutes in [21, 60, 12 * 60] {
        let kept = decide_tick(&run.pool, at(minutes), silence);
        assert!(kept.answer.is_empty(), "{minutes} minutes");
    }
    let ended = decide_tick(&run.pool, at(12 * 60) + Duration::seconds(1), silence);
    assert_eq!(
        ended.events,
        [released_event(
            "worktree1",
            &holder,
            WorktreeReleaseCause::Silent
        )]
    );
}

#[test]
fn a_tick_ends_exactly_the_silent_holds_and_follows_the_silence_it_is_given() {
    let mut run = Run::new();
    run.take(&agent(1, "a1"), 0);
    run.take(&session(1), 0);
    run.take(&agent(2, "b1"), 30);
    let ended = decide_tick(&run.pool, at(45), Silence::DEFAULT);
    assert_eq!(ended.answer.len(), 1);
    assert_eq!(ended.answer[0].holder, agent(1, "a1"));
    assert_eq!(
        ended
            .pool
            .holds()
            .iter()
            .map(|hold| hold.holder.clone())
            .collect::<Vec<_>>(),
        [session(1), agent(2, "b1")]
    );

    let short = Silence {
        agent: Duration::minutes(5),
        session: Duration::minutes(5),
    };
    let ended = decide_tick(&run.pool, at(36), short);
    assert_eq!(ended.answer.len(), 3);
}

#[test]
fn a_session_ending_frees_its_own_and_its_agents_holds_only() {
    let mut run = Run::new();
    run.take_of(&session(1), &["worktree1"], 0);
    run.take_of(&agent(1, "a1"), &["worktree2"], 0);
    run.take_of(&agent(2, "a1"), &["worktree3"], 0);
    run.take_of(&session(2), &["worktree4"], 0);
    let done = decide_session_ended(&run.pool, SessionId(1));
    assert_eq!(
        done.events,
        [
            released_event("worktree1", &session(1), WorktreeReleaseCause::SessionEnded),
            released_event(
                "worktree2",
                &agent(1, "a1"),
                WorktreeReleaseCause::SessionEnded
            ),
        ]
    );
    assert_eq!(done.answer.len(), 2);
    run.pool = done.pool;
    assert_eq!(
        run.holds(),
        [("worktree3", agent(2, "a1")), ("worktree4", session(2))]
    );
    let none = decide_session_ended(&run.pool, SessionId(9));
    assert!(none.events.is_empty() && none.answer.is_empty());
    assert_eq!(none.pool, run.pool);
}

#[test]
fn removing_the_member_frees_every_hold_and_names_who_removed_them() {
    let mut run = Run::new();
    run.take(&session(1), 0);
    run.take(&agent(1, "a1"), 0);
    let done = decide_member_removed(&run.pool, Some(BOB));
    assert!(done.pool.is_empty());
    assert_eq!(done.answer.len(), 2);
    for event in &done.events {
        let EventKind::WorktreeReleased(released) = event else {
            panic!("{event:?}");
        };
        assert_eq!(released.cause, WorktreeReleaseCause::MemberRemoved);
        assert_eq!(released.by, Some(BOB));
    }
}

#[test]
fn two_pools_of_one_member_and_machine_never_see_each_others_holds() {
    let mut first = Run::of("aaaa");
    let mut second = Run::of("bbbb");
    assert_ne!(first.pool.key(), second.pool.key());
    let holder = agent(1, "a1");
    assert_eq!(first.take(&holder, 0).0, took("worktree1", false));
    assert_eq!(second.take(&agent(1, "a2"), 0).0, took("worktree1", false));
    assert_eq!(second.take(&holder, 0).0, took("worktree2", false));
    let ended = decide_session_ended(&second.pool, SessionId(1));
    assert_eq!(ended.answer.len(), 2);
    assert_eq!(first.holds(), [("worktree1", holder)]);
}

#[test]
fn a_pool_is_built_in_name_order_and_refuses_a_worktree_or_a_holder_twice() {
    let hold = |worktree: &str, holder: Holder| WorktreeHold {
        worktree: name(worktree),
        holder,
        host: HostName::parse("laptop").unwrap(),
        purpose: Purpose::parse("fix the rounding").unwrap(),
        item: None,
        taken_at: at(0),
        last_seen: at(0),
    };
    let pool = WorktreePool::new(
        key_of(POOL),
        vec![hold("worktree2", session(1)), hold("worktree1", session(2))],
    )
    .unwrap();
    let order: Vec<&str> = pool
        .holds()
        .iter()
        .map(|hold| hold.worktree.as_str())
        .collect();
    assert_eq!(order, ["worktree1", "worktree2"]);

    let twice = WorktreePool::new(
        key_of(POOL),
        vec![hold("worktree1", session(1)), hold("worktree1", session(2))],
    );
    assert_eq!(
        twice,
        Err(WorktreePoolError::WorktreeHeldTwice {
            worktree: name("worktree1"),
        })
    );
    let holder_twice = WorktreePool::new(
        key_of(POOL),
        vec![
            hold("worktree1", agent(1, "a1")),
            hold("worktree2", agent(1, "a1")),
        ],
    );
    assert!(matches!(
        holder_twice,
        Err(WorktreePoolError::HolderHoldsTwo { .. })
    ));
    let distinct = WorktreePool::new(
        key_of(POOL),
        vec![
            hold("worktree1", session(1)),
            hold("worktree2", agent(1, "a1")),
            hold("worktree3", agent(2, "a1")),
        ],
    );
    assert!(distinct.is_ok());
}

#[test]
fn names_keys_and_pool_ids_follow_their_limits() {
    let long = |c: char, n: usize| c.to_string().repeat(n);
    for ok in ["worktree1", "w", "0", "a.b_c-d", long('a', 64).as_str()] {
        assert!(WorktreeName::parse(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        ".hidden",
        "-a",
        "_a",
        "Worktree1",
        "a/b",
        "a b",
        "..",
        long('a', 65).as_str(),
    ] {
        assert!(WorktreeName::parse(bad).is_err(), "{bad:?}");
    }
    for ok in ["a", "abc123", "x-y/z:1", long('k', 64).as_str()] {
        assert!(AgentKey::parse(ok).is_ok(), "{ok}");
    }
    for bad in ["", "a b", "a\tb", "a\u{7}", long('k', 65).as_str()] {
        assert!(AgentKey::parse(bad).is_err(), "{bad:?}");
    }
    for ok in ["a", "0f3e", long('0', 64).as_str()] {
        assert!(PoolId::parse(ok).is_ok(), "{ok}");
    }
    for bad in ["", "A", "a-b", "a.b", long('0', 65).as_str()] {
        assert!(PoolId::parse(bad).is_err(), "{bad:?}");
    }
    let name: WorktreeName = serde_json::from_str("\"worktree2\"").unwrap();
    assert_eq!(name.as_str(), "worktree2");
    assert!(serde_json::from_str::<WorktreeName>("\"Bad\"").is_err());
}

#[test]
fn the_default_silence_is_twenty_minutes_for_an_agent_and_twelve_hours_for_a_session() {
    let silence = Silence::DEFAULT;
    assert_eq!(silence.of(HolderKind::Agent), Duration::minutes(20));
    assert_eq!(silence.of(HolderKind::Session), Duration::hours(12));
    assert_eq!(agent(3, "a1").session(), SessionId(3));
    assert_eq!(session(3).agent(), None);
    assert_eq!(HolderKind::Agent.name(), "agent");
}

#[test]
fn no_event_of_a_pool_names_its_machine_its_pool_or_an_agent() {
    let mut run = Run::new();
    let agent_key = "agentkey9";
    let holder = agent(1, agent_key);
    let mut events = run.take(&holder, 0).1;
    events.extend(run.take(&session(1), 0).1);
    events.extend(
        decide_release(&run.pool, &Release::Holder(holder), BOB)
            .unwrap()
            .events,
    );
    events.extend(decide_tick(&run.pool, at(13 * 60), Silence::DEFAULT).events);
    events.extend(decide_session_ended(&run.pool, SessionId(1)).events);
    events.extend(decide_member_removed(&run.pool, None).events);
    assert_eq!(events.len(), 9);
    let allowed = [
        "member", "host", "worktree", "holder", "purpose", "cause", "by",
    ];
    for event in &events {
        let text = serde_json::to_string(event).unwrap();
        for secret in [MACHINE, POOL, agent_key] {
            assert!(!text.contains(secret), "{text} carries {secret}");
        }
        let value = serde_json::to_value(event).unwrap();
        for field in value["payload"].as_object().unwrap().keys() {
            assert!(allowed.contains(&field.as_str()), "{text} carries {field}");
        }
        assert!(event.name().starts_with("worktree_"));
        assert_eq!(
            event.private_to(),
            None,
            "every member's board shows the hold"
        );
    }
}
