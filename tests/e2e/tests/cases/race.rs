//! Two processes claim the same item at the same moment, over and over: one wins each time.

use std::sync::Barrier;
use std::time::Instant;

use serde_json::Value;

use crate::harness::Team;

const ROUNDS: usize = 100;

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["packages/billing/**"]
"#;

/// What one `dibs claim` answered: its exit code and the JSON it printed.
struct Attempt {
    code: Option<i32>,
    answer: Value,
}

#[test]
fn two_members_claiming_one_item_never_both_win() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, "git@example.com:acme/widgets.git");

    // A reference holds `/` and `#`, which a path segment cannot carry; each round has its own.
    let refs: Vec<String> = (0..ROUNDS)
        .map(|round| format!("gh:acme/widgets/race-{round}#{round}"))
        .collect();
    for (round, reference) in refs.iter().enumerate() {
        team.alice.ok(
            &work,
            &["new", reference, "--title", &format!("Race round {round}")],
        );
    }

    let started = Instant::now();
    let barrier = Barrier::new(2);
    let [alice, bob] = std::thread::scope(|scope| {
        [&team.alice, &team.bob]
            .map(|person| {
                let (refs, barrier, work) = (&refs, &barrier, &work);
                scope.spawn(move || {
                    refs.iter()
                        .map(|reference| {
                            barrier.wait();
                            let (code, answer) = person.json(work, &["claim", reference]);
                            Attempt { code, answer }
                        })
                        .collect::<Vec<_>>()
                })
            })
            .map(|handle| handle.join().unwrap())
    });
    eprintln!("{ROUNDS} rounds of two claims in {:?}", started.elapsed());

    let mut alice_wins = 0;
    for (round, (a, b)) in alice.iter().zip(&bob).enumerate() {
        let (winner, loser, winner_is_alice) = match (a.code, b.code) {
            (Some(0), Some(1)) => (a, b, true),
            (Some(1), Some(0)) => (b, a, false),
            other => panic!(
                "round {round}: not exactly one winner: {other:?}\n{a_answer}\n{b_answer}",
                a_answer = a.answer,
                b_answer = b.answer
            ),
        };
        assert_eq!(
            winner.answer["outcome"], "new",
            "round {round}: {}",
            winner.answer
        );
        assert_eq!(
            loser.answer["error"]["code"], "held",
            "round {round}: {}",
            loser.answer
        );
        let holder = if winner_is_alice { "alice" } else { "bob" };
        let message = loser.answer["error"]["message"].as_str().unwrap();
        assert!(message.contains(holder), "round {round}: {message}");
        alice_wins += usize::from(winner_is_alice);
    }
    eprintln!("alice won {alice_wins} rounds, bob {}", ROUNDS - alice_wins);

    // The server agrees with the processes: every item has the claim of the member who won it.
    let (_, status) = team.alice.json(&work, &["status"]);
    let live = status["live"].as_array().unwrap();
    assert_eq!(live.len(), ROUNDS, "one live claim per item");
    for (round, reference) in refs.iter().enumerate() {
        let entry = live
            .iter()
            .find(|entry| entry["item"]["ref"] == reference.as_str())
            .unwrap_or_else(|| panic!("round {round}: no live claim on {reference}"));
        let holder = if alice[round].code == Some(0) {
            "alice"
        } else {
            "bob"
        };
        assert_eq!(entry["claim"]["holder"], holder, "round {round}");
    }
}
