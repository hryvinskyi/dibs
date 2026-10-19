//! The guard's hook as an agent host calls it: a refusal naming the holder, a grant that lets the
//! edit through, and an item that declares an area someone else holds.

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
fn a_hook_denies_the_holders_area_until_the_holder_grants_it() {
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
        &["new", "gh:acme/widgets#43", "--title", "Invoice layout"],
    );
    bob.ok(&work, &["claim", "gh:acme/widgets#43"]);

    // bob's agent writes into alice's area: refused, naming alice.
    let denied = bob.before_edit(&work, "bob-session", &["packages/billing/round.rs"]);
    assert_eq!(decision(&denied), "deny", "{denied}");
    assert!(
        denied["reason"].as_str().unwrap().contains("alice"),
        "{denied}"
    );

    // alice's own agent, and a path no area governs, raise no objection.
    let own = alice.before_edit(&work, "alice-session", &["packages/billing/round.rs"]);
    assert_eq!(decision(&own), "no_objection", "{own}");
    let free = bob.before_edit(&work, "bob-session", &["docs/notes.md"]);
    assert_eq!(decision(&free), "no_objection", "{free}");

    // bob's item declaring the area alice holds is refused by the server.
    let (code, refusal) = bob.json(&work, &["claim", "gh:acme/widgets#43", "--area", "billing"]);
    assert_eq!(code, Some(1), "{refusal}");
    assert_eq!(refusal["error"]["code"], "area_held", "{refusal}");
    assert!(
        refusal["error"]["message"]
            .as_str()
            .unwrap()
            .contains("alice"),
        "{refusal}"
    );

    // alice lets bob in for his item; the same edit now goes through.
    alice.ok(
        &work,
        &[
            "grant",
            "gh:acme/widgets#43",
            "--area",
            "billing",
            "--to",
            "bob",
        ],
    );
    let granted = bob.before_edit(&work, "bob-session", &["packages/billing/round.rs"]);
    assert_eq!(decision(&granted), "no_objection", "{granted}");

    // The grant covers the area bob was let into and nothing else alice holds.
    alice.ok(&work, &["new", "gh:acme/widgets#44", "--title", "Ranking"]);
    alice.ok(&work, &["claim", "gh:acme/widgets#44", "--area", "search"]);
    let other = bob.before_edit(&work, "bob-session", &["packages/search/rank.rs"]);
    assert_eq!(decision(&other), "deny", "{other}");
}
