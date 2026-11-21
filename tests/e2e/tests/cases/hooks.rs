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

/// The `last_seen` of the first live claim `dibs status` shows, as `who` in `cwd`.
fn claim_last_seen(who: &crate::harness::Person, cwd: &std::path::Path) -> String {
    let (code, status) = who.json(cwd, &["status"]);
    assert_eq!(code, Some(0), "{status}");
    status["live"][0]["claim"]["last_seen"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn edits_the_hook_answers_from_its_cache_keep_the_claim_live_through_the_next_sync() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, REMOTE);
    let alice = &team.alice;
    alice.ok(
        &work,
        &["new", "gh:acme/widgets#42", "--title", "Refund rounding"],
    );
    alice.ok(&work, &["claim", "gh:acme/widgets#42", "--area", "billing"]);

    // The server checks the first edit, and this machine learns alice holds billing.
    let first = alice.before_edit(&work, "alice-session", &["packages/billing/round.rs"]);
    assert_eq!(decision(&first), "no_objection", "{first}");
    let checked = claim_last_seen(alice, &work);
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    for path in ["packages/billing/a.rs", "packages/billing/b.rs"] {
        let cached = alice.before_edit(&work, "alice-session", &[path]);
        assert_eq!(decision(&cached), "no_objection", "{cached}");
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let (code, synced) = alice.json(
            &work,
            &[
                "sync",
                "--force",
                "--host",
                "neutral",
                "--session",
                "alice-session",
            ],
        );
        assert_eq!(code, Some(0), "{synced}");
        if synced["heartbeat"] == true {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no heartbeat: {synced}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let after = claim_last_seen(alice, &work);
    assert!(
        after > checked,
        "the edits the cache answered moved the claim: {checked} then {after}"
    );
}
