//! A pilot's figures as its admin reads them: a refusal and a session run switched off, counted on
//! the member's machine, reported by the background sync a later command starts, and feedback on
//! the refusal by the id it was shown with.

use std::io::Write as _;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::harness::{Person, Server, apply_settings, call, checkout};

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["packages/billing/**"]
"#;

const REMOTE: &str = "git@example.com:acme/widgets.git";

/// `dibs hook neutral session-started` with the guard switched off.
fn start_switched_off(person: &Person, cwd: &std::path::Path) -> Value {
    let input = json!({"protocol": 1, "session": "bob-quiet", "cwd": cwd, "thread": "main"});
    let mut hook = person
        .dibs(cwd)
        .env("DIBS_OFF", "1")
        .args(["hook", "neutral", "session-started"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(hook.stdin.take().unwrap(), "{input}").unwrap();
    let out = hook.wait_with_output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn an_admin_reads_the_refusals_and_the_switched_off_sessions_members_machines_reported() {
    let root = tempfile::tempdir().unwrap();
    let (server, alice_token) = Server::start("widgets", "alice");
    let bob_token = server.add_member("bob");
    let alice = Person::sign_in(&server, root.path(), "alice", &alice_token);
    let bob = Person::sign_in(&server, root.path(), "bob", &bob_token);
    apply_settings(&alice, root.path(), SETTINGS);
    let work = checkout(&root.path().join("work"), &server, None, REMOTE);

    alice.ok(&work, &["new", "gh:acme/widgets#42", "--title", "Refunds"]);
    alice.ok(&work, &["claim", "gh:acme/widgets#42", "--area", "billing"]);
    let denied = bob.before_edit(&work, "bob-session", &["packages/billing/round.rs"]);
    assert_eq!(denied["decision"], "deny", "{denied}");
    let reason = denied["reason"].as_str().unwrap();
    let id = reason
        .split_whitespace()
        .find(|word| word.starts_with("r-"))
        .map(|word| word.trim_end_matches(';').to_owned())
        .unwrap();
    assert_eq!(
        start_switched_off(&bob, &work),
        json!({"protocol": 1, "decision": "no_objection"})
    );

    bob.ok(&work, &["status"]);
    let deadline = Instant::now() + Duration::from_secs(15);
    let stats = loop {
        let (code, stats) = call(&server, "GET", "/v1/stats", &alice_token, &Value::Null);
        assert_eq!(code, 200, "{stats}");
        if stats["reported"]["switched_off"] == 1 || Instant::now() > deadline {
            break stats;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(stats["reported"]["held_by"], 1, "{stats}");
    assert_eq!(stats["reported"]["switched_off"], 1, "{stats}");
    assert_eq!(stats["claims"], 1, "{stats}");
    assert_eq!(stats["reported_days"][0]["member"], "bob", "{stats}");
    assert!(
        !stats.to_string().contains("round.rs"),
        "no path reaches the figures: {stats}"
    );

    bob.ok(
        &work,
        &[
            "feedback",
            "false-refusal",
            &id,
            "--reason",
            "alice said go ahead",
        ],
    );
    let (_, stats) = call(&server, "GET", "/v1/stats", &alice_token, &Value::Null);
    assert_eq!(stats["feedback"]["false_refusals"], 1, "{stats}");
    assert_eq!(stats["feedback"]["entries"][0]["refusal"], id.as_str());
    assert_eq!(stats["feedback"]["entries"][0]["tally"], "held_by");
    let (code, _) = call(&server, "GET", "/v1/stats", &bob_token, &Value::Null);
    assert_eq!(code, 403, "the figures are the admins'");
}
