//! Two processes take one slot at the same moment, over and over: one holds it and the other waits
//! each time. A member waiting with `--wait` is handed the slot when its holder releases it.

use std::io::Read;
use std::process::Stdio;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::harness::Team;

const ROUNDS: usize = 100;

const SETTINGS: &str = r#"
[[slots]]
name = "deploy:prod"
max_hold = "30m"
"#;

struct Attempt {
    code: Option<i32>,
    answer: Value,
}

#[test]
fn two_members_taking_one_slot_make_one_holder_and_one_waiter_every_round() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, "git@example.com:acme/widgets.git");
    let take = ["slot", "take", "deploy:prod", "--purpose", "ship it"];
    let started = Instant::now();
    let barrier = Barrier::new(2);
    let rounds: Vec<(Attempt, Attempt)> = (0..ROUNDS)
        .map(|_| {
            let [alice, bob] = std::thread::scope(|scope| {
                [&team.alice, &team.bob]
                    .map(|person| {
                        let (barrier, work) = (&barrier, &work);
                        scope.spawn(move || {
                            barrier.wait();
                            let (code, answer) = person.json(work, &take);
                            Attempt { code, answer }
                        })
                    })
                    .map(|handle| handle.join().unwrap())
            });
            let (holder, waiter) = match (alice.code, bob.code) {
                (Some(0), Some(1)) => (&team.alice, &team.bob),
                (Some(1), Some(0)) => (&team.bob, &team.alice),
                other => panic!(
                    "not one holder and one waiter: {other:?}\n{}\n{}",
                    alice.answer, bob.answer
                ),
            };
            holder.ok(&work, &["slot", "release", "deploy:prod"]);
            waiter.ok(&work, &["slot", "leave", "deploy:prod"]);
            (alice, bob)
        })
        .collect();
    eprintln!("{ROUNDS} rounds of two takes in {:?}", started.elapsed());
    for (round, (a, b)) in rounds.iter().enumerate() {
        let (held, queued) = if a.code == Some(0) { (a, b) } else { (b, a) };
        assert_eq!(
            held.answer["outcome"], "taken",
            "round {round}: {}",
            held.answer
        );
        assert_eq!(
            queued.answer["outcome"], "queued",
            "round {round}: {}",
            queued.answer
        );
        assert_eq!(queued.answer["position"], 1, "round {round}");
    }
    let (_, list) = team.alice.json(&work, &["slot", "list"]);
    let slot = &list["slots"][0];
    assert!(slot["hold"].is_null() && slot["offer"].is_null(), "{slot}");
    assert_eq!(slot["queue"], serde_json::json!([]));
}

#[test]
fn a_member_waiting_is_handed_the_slot_when_its_holder_releases_it() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, "git@example.com:acme/widgets.git");
    team.alice.ok(
        &work,
        &["slot", "take", "deploy:prod", "--purpose", "ship 2.4"],
    );
    let mut waiting = team
        .bob
        .dibs(&work)
        .args([
            "slot",
            "take",
            "deploy:prod",
            "--purpose",
            "hotfix",
            "--wait",
            "60s",
            "--poll-every",
            "1s",
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(2000));
    let released = Instant::now();
    team.alice.ok(&work, &["slot", "release", "deploy:prod"]);
    let status = waiting.wait().unwrap();
    let mut out = String::new();
    waiting
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert_eq!(status.code(), Some(0), "{out}");
    let answer: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(answer["outcome"], "taken");
    assert_eq!(answer["slot"]["hold"]["holder"], "bob");
    assert!(
        released.elapsed() < Duration::from_secs(30),
        "within its deadline"
    );
}
