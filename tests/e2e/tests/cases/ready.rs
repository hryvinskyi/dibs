//! An integration worker's import through `PUT /v1/items`, and the ready list it makes as two
//! members see it from the command line.

use serde_json::{Value, json};

use crate::harness::{Person, Team, call};

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["src/billing/**"]
"#;

fn refs(list: &Value, key: &str) -> Vec<String> {
    list[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["item"]["ref"].as_str().unwrap().to_owned())
        .collect()
}

fn ready_of(person: &Person, dir: &std::path::Path) -> Value {
    let (code, list) = person.json(dir, &["ready"]);
    assert_eq!(code, Some(0), "{list}");
    list
}

#[test]
fn an_import_makes_one_item_ready_and_one_blocked_as_each_member_sees_it() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, "git@example.com:acme/widgets.git");
    let board = team.server.add_member_as("board", "importer");
    let batch = json!({
        "source": "board",
        "complete": true,
        "items": [
            {"ref": "gh:Acme/Widgets#1", "title": "Round invoice totals", "priority": 1,
             "areas_hint": ["billing"], "source_status": "Todo"},
            {"ref": "gh:acme/widgets#2", "title": "Send the invoices",
             "blocked_by": ["gh:acme/widgets#1"]},
            {"ref": "gh:acme/widgets#9", "title": "Shipped long ago", "closed": true}
        ]
    });
    let (status, answer) = call(&team.server, "PUT", "/v1/items", &board, &batch);
    assert_eq!(status, 200, "{answer}");
    let outcomes: Vec<&str> = answer["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["created", "created", "skipped_closed"]);
    assert_eq!(answer["edges_added"], 1);

    // The worker's next tick sends the same view: nothing changes.
    let (_, again) = call(&team.server, "PUT", "/v1/items", &board, &batch);
    assert!(
        again["items"]
            .as_array()
            .unwrap()
            .iter()
            .take(2)
            .all(|item| item["outcome"] == "unchanged"),
        "{again}"
    );

    // A person types the reference in another case and finds the imported item.
    let (code, typed) = team.alice.json(
        &work,
        &[
            "new",
            "gh:ACME/widgets#1",
            "--title",
            "Round invoice totals",
        ],
    );
    assert_eq!(code, Some(0), "{typed}");
    assert_eq!(typed["created"], false, "{typed}");

    let alices = ready_of(&team.alice, &work);
    assert_eq!(refs(&alices, "ready"), ["gh:acme/widgets#1"]);
    assert_eq!(
        alices["ready"][0]["link"],
        "https://github.com/acme/widgets/issues/1"
    );
    assert_eq!(refs(&alices, "excluded"), ["gh:acme/widgets#2"]);
    assert_eq!(alices["excluded"][0]["reasons"][0]["reason"], "blocked_by");
    assert_eq!(
        alices["excluded"][0]["reasons"][0]["items"][0]["ref"],
        "gh:acme/widgets#1"
    );

    // Bob takes other billing work; the hinted area is his now.
    team.bob.ok(
        &work,
        &["new", "gh:acme/widgets#40", "--title", "Billing rewrite"],
    );
    team.bob
        .ok(&work, &["claim", "gh:acme/widgets#40", "--area", "billing"]);

    let alices = ready_of(&team.alice, &work);
    assert!(refs(&alices, "ready").is_empty(), "{alices}");
    let held = alices["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["item"]["ref"] == "gh:acme/widgets#1")
        .unwrap();
    assert_eq!(held["reasons"][0]["reason"], "area_held");
    assert_eq!(held["reasons"][0]["holder"], "bob");

    let bobs = ready_of(&team.bob, &work);
    assert_eq!(
        refs(&bobs, "ready"),
        ["gh:acme/widgets#1"],
        "bob holds the area himself"
    );
    let text = String::from_utf8_lossy(&team.alice.run(&work, &["ready"]).stdout).into_owned();
    assert!(
        text.contains("P1 gh:acme/widgets#1 Round invoice totals — default:billing is held by bob for gh:acme/widgets#40"),
        "{text}"
    );

    // The importer is no person: it may not claim.
    let (status, refused) = call(
        &team.server,
        "POST",
        "/v1/items/1/claim",
        &board,
        &json!({}),
    );
    assert_eq!(status, 403, "{refused}");
}
