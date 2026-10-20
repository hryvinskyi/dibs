//! A wide claim across the built binaries: alice holds `billing`, bob takes a wide claim over
//! `billing` and `search` from the command line, and bob's agent's edit in `billing` is denied by
//! the hook until alice acknowledges the announcement dibs sent her, then let through.

use serde_json::Value;

use crate::harness::Team;

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["packages/billing/**"]

[[areas]]
name = "search"
paths = ["packages/search/**"]
"#;

const REMOTE: &str = "git@example.com:acme/widgets.git";

fn decision(reply: &Value) -> &str {
    reply["decision"].as_str().unwrap_or_default()
}

#[test]
fn a_wide_claimers_edit_waits_for_the_holders_acknowledgement() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, REMOTE);
    let (alice, bob) = (&team.alice, &team.bob);

    alice.ok(
        &work,
        &["new", "gh:acme/widgets#42", "--title", "Refund rounding"],
    );
    alice.ok(&work, &["claim", "gh:acme/widgets#42", "--area", "billing"]);
    bob.ok(
        &work,
        &[
            "new",
            "gh:acme/widgets#50",
            "--title",
            "Upgrade the HTTP client",
        ],
    );
    let (code, wide) = bob.json(
        &work,
        &[
            "claim",
            "gh:acme/widgets#50",
            "--wide",
            "billing,search",
            "--reason",
            "Upgrade the HTTP client in every package",
        ],
    );
    assert_eq!(code, Some(0), "{wide}");
    assert_eq!(wide["claim"]["areas"], serde_json::json!([]), "{wide}");
    assert_eq!(wide["announced"][0]["to"], "alice", "{wide}");
    let message = wide["announced"][0]["message_id"]
        .as_u64()
        .unwrap()
        .to_string();

    let denied = bob.before_edit(&work, "bob-session", &["packages/billing/client.rs"]);
    assert_eq!(decision(&denied), "deny", "{denied}");
    let reason = denied["reason"].as_str().unwrap();
    assert!(reason.contains("Your wide claim names it"), "{reason}");
    assert!(reason.contains("alice"), "{reason}");
    let free = bob.before_edit(&work, "bob-session", &["packages/search/client.rs"]);
    assert_eq!(decision(&free), "no_objection", "{free}");

    let (_, waiting) = alice.json(&work, &["inbox", "--unacked"]);
    assert_eq!(
        waiting["unacked"][0]["message"]["id"]
            .as_u64()
            .map(|id| id.to_string()),
        Some(message.clone()),
        "{waiting}"
    );
    let shown = alice.ok(&work, &["status"]);
    assert!(shown.contains("billing (waiting for alice)"), "{shown}");

    let acked = alice.ok(&work, &["ack", &message]);
    assert!(acked.contains("That grants bob edits in"), "{acked}");
    let allowed = bob.before_edit(&work, "bob-session", &["packages/billing/client.rs"]);
    assert_eq!(decision(&allowed), "no_objection", "{allowed}");
    let shown = bob.ok(&work, &["status"]);
    assert!(shown.contains("billing (acknowledged by alice)"), "{shown}");
}
