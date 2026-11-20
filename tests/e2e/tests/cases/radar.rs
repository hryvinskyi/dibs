//! The overlap radar across the built binaries: alice and bob each work on a branch of their own
//! checkout, the sync of the session that took each claim tells the server which branch the claim
//! is on, the integration reads the branches and reports what each changes through the footprint
//! route, and the one file both branches change is announced once, to both of them, however often
//! the reports come. A checkout two sessions of one member share records no branch, so a file its
//! claim's work changes is announced to nobody.

use serde_json::{Value, json};

use crate::harness::{Team, call, start_branch};

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["packages/billing/**"]

[[areas]]
name = "search"
paths = ["packages/search/**"]
"#;

const REMOTE: &str = "git@example.com:acme/widgets.git";

const SUBJECT: &str = "Overlap: gh:acme/widgets#1 and gh:acme/widgets#2 change the same files";

/// The subjects of the threads in `inbox` that announce the overlap.
fn announcements(inbox: &Value) -> usize {
    inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|thread| thread["thread"]["subject"] == SUBJECT)
        .count()
}

#[test]
fn one_file_two_branches_change_is_told_to_both_holders_once() {
    let team = Team::start(SETTINGS);
    let (alice, bob) = (&team.alice, &team.bob);
    let worker = team.server.add_member_as("worker", "importer");
    let alices_checkout = team.checkout("alice-work", None, REMOTE);
    let bobs_checkout = team.checkout("bob-work", None, REMOTE);
    start_branch(&alices_checkout, "alice/1-rates");
    start_branch(&bobs_checkout, "bob/2-search");

    let mut items = Vec::new();
    for (who, checkout, reference, area, path) in [
        (
            alice,
            &alices_checkout,
            "gh:acme/widgets#1",
            "billing",
            "packages/billing/rates.rs",
        ),
        (
            bob,
            &bobs_checkout,
            "gh:acme/widgets#2",
            "search",
            "packages/search/index.rs",
        ),
    ] {
        who.ok(checkout, &["new", reference, "--title", "Work"]);
        let (code, claimed) = who.json(checkout, &["claim", reference, "--area", area]);
        assert_eq!(code, Some(0), "{claimed}");
        items.push(claimed["claim"]["item"].as_u64().unwrap());
        who.ok(checkout, &["sync", "--force", "--quiet"]);
        let edited = who.before_edit(checkout, &format!("{}-session", area), &[path]);
        assert_eq!(edited["decision"], "no_objection", "{edited}");
    }

    let (code, listed) = alice.json(&alices_checkout, &["status"]);
    assert_eq!(code, Some(0), "{listed}");
    let branches: Vec<Value> = listed["live"]
        .as_array()
        .unwrap()
        .iter()
        .map(|held| held["claim"]["branches"].clone())
        .collect();
    assert_eq!(
        branches,
        [
            json!({"default": "alice/1-rates"}),
            json!({"default": "bob/2-search"})
        ],
        "each claim records the branch of the checkout its session works in"
    );

    let footprints = [
        (
            items[0],
            "alice/1-rates",
            json!(["packages/billing/rates.rs", "config/app.yaml"]),
        ),
        (
            items[1],
            "bob/2-search",
            json!(["packages/search/index.rs", "config/app.yaml"]),
        ),
    ];
    for _ in 0..2 {
        for (item, branch, paths) in &footprints {
            let body = json!({"repo": "default", "branch": branch, "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "0a1b2c3d",
                              "paths": paths});
            let (code, stored) = call(
                &team.server,
                "PUT",
                &format!("/v1/items/{item}/footprint"),
                &worker,
                &body,
            );
            assert_eq!(code, 200, "{stored}");
            assert_eq!(stored["stored"], true, "{stored}");
        }
    }

    for (who, checkout) in [(alice, &alices_checkout), (bob, &bobs_checkout)] {
        let (_, inbox) = who.json(checkout, &["inbox"]);
        assert_eq!(announcements(&inbox), 1, "{inbox}");
    }
    let (_, status) = bob.json(&bobs_checkout, &["status"]);
    assert_eq!(status["overlaps"][0]["kind"], "same_file", "{status}");
    assert_eq!(
        status["overlaps"][0]["paths"],
        json!(["default:config/app.yaml"])
    );
}

#[test]
fn a_checkout_two_sessions_of_one_member_share_records_no_branch_and_announces_nothing() {
    let team = Team::start(SETTINGS);
    let (alice, bob) = (&team.alice, &team.bob);
    let worker = team.server.add_member_as("worker", "importer");
    let alices_checkout = team.checkout("alice-work", None, REMOTE);
    let bobs_checkout = team.checkout("bob-work", None, REMOTE);
    start_branch(&alices_checkout, "alice/1-rates");
    start_branch(&bobs_checkout, "bob/2-search");

    let mut items = Vec::new();
    for (who, checkout, reference) in [
        (alice, &alices_checkout, "gh:acme/widgets#1"),
        (bob, &bobs_checkout, "gh:acme/widgets#2"),
    ] {
        who.ok(checkout, &["new", reference, "--title", "Work"]);
        let (code, claimed) = who.json(checkout, &["claim", reference]);
        assert_eq!(code, Some(0), "{claimed}");
        items.push(claimed["claim"]["item"].as_u64().unwrap());
    }
    alice.ok(&alices_checkout, &["sync", "--force", "--quiet"]);
    let other_session = [
        "sync",
        "--force",
        "--quiet",
        "--host",
        "neutral",
        "--session",
        "s-other",
    ];
    for _ in 0..2 {
        bob.ok(&bobs_checkout, &other_session);
        bob.ok(&bobs_checkout, &["sync", "--force", "--quiet"]);
    }

    let (code, listed) = alice.json(&alices_checkout, &["status"]);
    assert_eq!(code, Some(0), "{listed}");
    let branches: Vec<Value> = listed["live"]
        .as_array()
        .unwrap()
        .iter()
        .map(|held| held["claim"]["branches"].clone())
        .collect();
    assert_eq!(
        branches,
        [json!({"default": "alice/1-rates"}), json!({})],
        "bob's checkout is shared by two of his sessions, so it says nothing of his claim"
    );

    for (item, branch, stored) in [
        (items[0], "alice/1-rates", true),
        (items[1], "bob/2-search", false),
    ] {
        let body = json!({"repo": "default", "branch": branch, "base": "main",
                          "base_at": "2026-03-01T00:00:00Z", "base_sha": "0a1b2c3d",
                          "paths": ["config/app.yaml"]});
        let (code, answer) = call(
            &team.server,
            "PUT",
            &format!("/v1/items/{item}/footprint"),
            &worker,
            &body,
        );
        assert_eq!(code, 200, "{answer}");
        assert_eq!(answer["stored"], stored, "{answer}");
    }
    for (who, checkout) in [(alice, &alices_checkout), (bob, &bobs_checkout)] {
        let (_, inbox) = who.json(checkout, &["inbox"]);
        assert_eq!(announcements(&inbox), 0, "{inbox}");
    }
}
