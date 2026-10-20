//! Review across the built binaries, with an integration driving the routes as its worker does:
//! bob claims an item, his agent's push to the protected main branch is refused with the pull
//! request as the way on, he records his pull request from the command line, alice's edit in his
//! area waits for its merge, and the integration lists the claims in review, reports the merge —
//! which completes bob's claim — and its repeat, which matches nothing.

use serde_json::{Value, json};

use crate::harness::{Team, call};

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["packages/billing/**"]

[[protected_refs]]
ref = "refs/heads/main"
pushers = ["alice"]
"#;

const REMOTE: &str = "git@example.com:acme/widgets.git";

fn decision(reply: &Value) -> &str {
    reply["decision"].as_str().unwrap_or_default()
}

#[test]
fn a_claim_goes_through_review_and_the_integrations_merge_report_completes_it() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, REMOTE);
    let (alice, bob) = (&team.alice, &team.bob);
    let worker = team.server.add_member_as("worker", "importer");

    bob.ok(
        &work,
        &["new", "gh:acme/widgets#42", "--title", "Refund rounding"],
    );
    bob.ok(&work, &["claim", "gh:acme/widgets#42", "--area", "billing"]);

    let pushed = bob.before_shell(&work, "bob-session", "git push origin main");
    assert_eq!(decision(&pushed), "deny", "{pushed}");
    let reason = pushed["reason"].as_str().unwrap();
    assert!(reason.contains("only alice may push to"), "{reason}");
    assert!(reason.contains("open a pull request"), "{reason}");
    let branch = bob.before_shell(
        &work,
        "bob-session",
        "git push origin bob/42-refund-rounding",
    );
    assert_eq!(decision(&branch), "no_objection", "{branch}");

    let (code, reviewed) = bob.json(
        &work,
        &[
            "claim",
            "review",
            "gh:acme/widgets#42",
            "--pr",
            "acme/widgets#7",
        ],
    );
    assert_eq!(code, Some(0), "{reviewed}");
    assert_eq!(reviewed["claim"]["state"], "in_review", "{reviewed}");
    let item = reviewed["item"]["id"].as_u64().unwrap();

    let held = alice.before_edit(&work, "alice-session", &["packages/billing/refund.rs"]);
    assert_eq!(decision(&held), "deny", "{held}");
    let reason = held["reason"].as_str().unwrap();
    assert!(reason.contains("in review on acme/widgets#7"), "{reason}");
    assert!(reason.contains("free when it merges"), "{reason}");

    let (listed_status, listed) = call(
        &team.server,
        "GET",
        "/v1/review-claims",
        &worker,
        &Value::Null,
    );
    assert_eq!(listed_status, 200, "{listed}");
    assert_eq!(listed["claims"][0]["holder"], "bob", "{listed}");
    assert_eq!(listed["claims"][0]["prs"], json!(["acme/widgets#7"]));

    let merge = json!({"pr": "acme/widgets#7", "base": "main", "merged_at": "2026-03-02T10:00:00Z",
                       "files": ["packages/billing/refund.rs"]});
    let (merged_status, merged) = call(&team.server, "POST", "/v1/prs/merged", &worker, &merge);
    assert_eq!(merged_status, 200, "{merged}");
    assert_eq!(
        merged,
        json!({"matched": true, "item": item, "claim": "completed"})
    );

    let (_, status) = bob.json(&work, &["status"]);
    assert_eq!(status["live"], json!([]), "{status}");
    assert_eq!(status["holds"], json!([]), "{status}");
    let done = bob.ok(&work, &["items", "--status", "done"]);
    assert!(done.contains("gh:acme/widgets#42"), "{done}");
    let (_, inbox) = bob.json(&work, &["inbox"]);
    let subjects: Vec<&str> = inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|thread| thread["thread"]["subject"].as_str())
        .collect();
    assert_eq!(subjects, ["Done by merge: Refund rounding"], "{inbox}");
    let free = alice.before_edit(&work, "alice-session", &["packages/billing/refund.rs"]);
    let reason = free["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("you hold no live claim") && !reason.contains("bob"),
        "the area is free; alice only needs a claim of her own: {free}"
    );

    let (again_status, again) = call(&team.server, "POST", "/v1/prs/merged", &worker, &merge);
    assert_eq!(again_status, 200);
    assert_eq!(again, json!({"matched": false}), "a repeat matches nothing");
}
