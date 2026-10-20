//! A member removed while they hold a claim, a slot and a conversation, across the built binaries:
//! the admin's `dibs admin member remove` releases what they held at once, their sign-in stops
//! working on their own machine, the area and the slot pass to the member who waited, the project
//! is told by dibs, and what they wrote stays.

use crate::harness::Team;

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["src/billing/**"]

[[slots]]
name = "deploy:prod"
max_hold = "30m"
"#;

#[test]
fn removing_a_member_frees_their_claim_and_slot_ends_their_sign_in_and_keeps_their_words() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, "git@example.com:acme/widgets.git");
    team.bob
        .ok(&work, &["new", "gh:acme/widgets#5", "--title", "Rounding"]);
    team.bob
        .ok(&work, &["claim", "gh:acme/widgets#5", "--area", "billing"]);
    team.bob.ok(
        &work,
        &["slot", "take", "deploy:prod", "--purpose", "ship 2.4"],
    );
    let (code, queued) = team.alice.json(
        &work,
        &["slot", "take", "deploy:prod", "--purpose", "ship 2.5"],
    );
    assert_eq!(code, Some(1), "alice waits: {queued}");
    let (code, sent) = team.bob.json(
        &work,
        &[
            "send",
            "--to",
            "alice",
            "--subject",
            "Rounding",
            "Halfway through the invoices.",
        ],
    );
    assert_eq!(code, Some(0), "{sent}");
    let thread = sent["thread"]["id"].as_u64().unwrap().to_string();
    team.alice
        .ok(&work, &["new", "gh:acme/widgets#6", "--title", "Refunds"]);
    let (code, refused) = team
        .alice
        .json(&work, &["claim", "gh:acme/widgets#6", "--area", "billing"]);
    assert_eq!(code, Some(1), "bob holds billing: {refused}");

    let (code, removed) = team
        .alice
        .json(&work, &["admin", "member", "remove", "bob"]);
    assert_eq!(code, Some(0), "{removed}");
    assert_eq!(removed["released"][0]["ref"], "gh:acme/widgets#5");
    assert_eq!(removed["slots"]["held"], serde_json::json!(["deploy:prod"]));
    assert_eq!(removed["tokens_revoked"], 1);

    let gone = team.bob.run(&work, &["whoami"]);
    assert_eq!(gone.status.code(), Some(2), "bob's token signs in nowhere");
    let edit = team
        .bob
        .before_edit(&work, "sess-bob-1", &["src/billing/rate.rs"]);
    assert_eq!(edit["decision"], "no_objection", "{edit}");
    assert!(
        edit["notice"].is_string(),
        "one warning, no decision: {edit}"
    );

    let (code, claimed) = team
        .alice
        .json(&work, &["claim", "gh:acme/widgets#6", "--area", "billing"]);
    assert_eq!(code, Some(0), "billing is free now: {claimed}");
    let (_, slots) = team.alice.json(&work, &["slot", "list"]);
    assert_eq!(slots["slots"][0]["offer"]["member"], "alice", "{slots}");

    let (_, inbox) = team.alice.json(&work, &["inbox"]);
    let told = inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["thread"]["subject"] == "bob left the project")
        .unwrap_or_else(|| panic!("no notice: {inbox}"))
        .clone();
    let (_, notice) = team.alice.json(
        &work,
        &[
            "thread",
            &told["thread"]["id"].as_u64().unwrap().to_string(),
        ],
    );
    assert_eq!(notice["messages"][0]["system"], true, "{notice}");
    let shown = team.alice.ok(
        &work,
        &[
            "thread",
            &told["thread"]["id"].as_u64().unwrap().to_string(),
        ],
    );
    assert!(shown.contains(", dibs at "), "from dibs: {shown}");
    assert!(shown.contains("gh:acme/widgets#5"), "{shown}");

    let (_, kept) = team.alice.json(&work, &["thread", &thread]);
    assert_eq!(kept["messages"][0]["author"], "bob", "{kept}");
    assert_eq!(kept["messages"][0]["body"], "Halfway through the invoices.");
}
