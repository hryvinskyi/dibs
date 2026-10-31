//! A member's machine lends the worktrees of a pool to the member's agents: two takes are two
//! worktrees and a third waits until one is released, an edit made inside a worktree is checked
//! against the other members' claims like an edit in the checkout, and a worktree left free is
//! stopped by the project's command while a held one is not.

use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::harness::Team;

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["packages/billing/**"]
"#;

const REMOTE: &str = "git@example.com:acme/widgets.git";

#[test]
fn two_takes_are_two_worktrees_and_a_third_waits_until_one_is_released() {
    let team = Team::start(SETTINGS);
    let pool = team.pool("work", REMOTE, 2, "30m");
    let bob = &team.bob;

    let (code, first) = bob.json(
        &pool.checkout,
        &["worktree", "take", "--purpose", "fix totals"],
    );
    assert_eq!(code, Some(0), "{first}");
    let (code, second) = bob.json(
        &pool.checkout,
        &["worktree", "take", "--purpose", "fix labels"],
    );
    assert_eq!(code, Some(0), "{second}");
    assert_eq!(first["worktree"], "worktree1", "{first}");
    assert_eq!(second["worktree"], "worktree2", "{second}");
    assert_ne!(
        first["agent"], second["agent"],
        "each take is a holder of its own: {first} {second}"
    );
    for (name, taken) in [("worktree1", &first), ("worktree2", &second)] {
        let path = pool.worktree(name);
        assert_eq!(
            std::fs::canonicalize(taken["path"].as_str().unwrap()).unwrap(),
            path
        );
        assert!(path.join(".git").exists(), "{name} is a git worktree");
        assert!(path.join(".dibs.toml").is_file(), "{name} is governed");
    }
    assert_eq!(
        pool.commands_run(),
        ["prepare worktree1", "prepare worktree2"]
    );

    let (_, listed) = bob.json(&pool.checkout, &["worktree", "list"]);
    for worktree in listed["worktrees"].as_array().unwrap() {
        assert!(!worktree["held"].is_null(), "{listed}");
    }

    let (code, busy) = bob.json(&pool.checkout, &["worktree", "take"]);
    assert_eq!(code, Some(1), "{busy}");
    assert_eq!(busy["error"]["code"], "worktree_busy", "{busy}");
    let message = busy["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("worktree1") && message.contains("worktree2"),
        "{message}"
    );

    let waiting = bob
        .dibs(&pool.checkout)
        .args([
            "worktree",
            "take",
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
    bob.ok(&pool.checkout, &["worktree", "release", "worktree1"]);
    let out = waiting.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        crate::harness::text(&out.stderr)
    );
    let third: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(third["worktree"], "worktree1", "{third}");
    assert_eq!(third["again"], false, "{third}");
    assert!(
        released.elapsed() < Duration::from_secs(30),
        "within its deadline"
    );
    assert_eq!(
        pool.commands_run(),
        ["prepare worktree1", "prepare worktree2", "reset worktree1"],
        "a reused worktree is reset, not prepared again"
    );
}

#[test]
fn an_edit_in_a_pool_worktree_is_checked_against_claims_like_one_in_the_checkout() {
    let team = Team::start(SETTINGS);
    let pool = team.pool("work", REMOTE, 1, "30m");
    let (alice, bob) = (&team.alice, &team.bob);

    alice.ok(
        &pool.checkout,
        &["new", "gh:acme/widgets#42", "--title", "Refund rounding"],
    );
    alice.ok(
        &pool.checkout,
        &["claim", "gh:acme/widgets#42", "--area", "billing"],
    );
    bob.ok(
        &pool.checkout,
        &["new", "gh:acme/widgets#43", "--title", "Invoice layout"],
    );
    bob.ok(&pool.checkout, &["claim", "gh:acme/widgets#43"]);
    bob.ok(&pool.checkout, &["worktree", "prepare", "worktree1"]);
    let worktree = pool.worktree("worktree1");

    let denied = bob.before_edit(&worktree, "bob-session", &["packages/billing/round.rs"]);
    assert_eq!(denied["decision"], "deny", "{denied}");
    assert!(
        denied["reason"].as_str().unwrap().contains("alice"),
        "{denied}"
    );
    let free = bob.before_edit(&worktree, "bob-session", &["docs/notes.md"]);
    assert_eq!(free["decision"], "no_objection", "{free}");

    let (_, listed) = bob.json(&pool.checkout, &["worktree", "list"]);
    assert!(
        !listed["worktrees"][0]["held"].is_null(),
        "the session that worked in the free worktree holds it now: {listed}"
    );
    let other = bob.before_edit(&worktree, "bob-other-session", &["docs/notes.md"]);
    assert_eq!(other["decision"], "deny", "{other}");
    assert!(
        other["reason"].as_str().unwrap().contains("holds no hold"),
        "{other}"
    );
}

#[test]
fn a_worktree_left_free_is_stopped_after_its_idle_time_and_a_held_one_is_not() {
    let team = Team::start(SETTINGS);
    let pool = team.pool("work", REMOTE, 2, "1s");
    let bob = &team.bob;
    for _ in 0..2 {
        bob.ok(&pool.checkout, &["worktree", "take"]);
    }
    bob.ok(&pool.checkout, &["worktree", "release", "worktree1"]);

    let (code, first) = bob.json(&pool.checkout, &["worktree", "upkeep"]);
    assert_eq!(code, Some(0), "{first}");
    assert_eq!(first["stopped"], serde_json::json!([]), "{first}");
    std::thread::sleep(Duration::from_millis(2500));
    let (code, second) = bob.json(&pool.checkout, &["worktree", "upkeep"]);
    assert_eq!(code, Some(0), "{second}");
    assert_eq!(
        second["stopped"],
        serde_json::json!(["worktree1"]),
        "{second}"
    );
    assert!(
        pool.commands_run().contains(&"idle worktree1".to_owned())
            && !pool.commands_run().contains(&"idle worktree2".to_owned()),
        "{:?}",
        pool.commands_run()
    );
}
