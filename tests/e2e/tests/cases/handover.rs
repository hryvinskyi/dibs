//! Taking an area from debt work across the built binaries: alice works on debt in billing, bob's
//! feature claim declares billing and waits for it, his agent's edit there is held with the way on,
//! alice is asked, narrows her claim, and bob's claim holds billing and his edit goes ahead.

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

#[test]
fn a_feature_claim_receives_the_area_a_debt_claim_hands_over() {
    let team = Team::start(SETTINGS);
    let (alice, bob) = (&team.alice, &team.bob);
    let alices = team.checkout("alice-work", None, REMOTE);
    let bobs = team.checkout("bob-work", None, REMOTE);
    alice.ok(
        &alices,
        &["new", "gh:acme/widgets#7", "--title", "Tidy billing"],
    );
    bob.ok(&bobs, &["new", "gh:acme/widgets#42", "--title", "Currency"]);
    let (code, debt) = alice.json(
        &alices,
        &[
            "claim",
            "gh:acme/widgets#7",
            "--area",
            "billing",
            "--area",
            "search",
            "--debt",
        ],
    );
    assert_eq!(code, Some(0), "{debt}");
    assert_eq!(debt["claim"]["kind"], "debt");

    let (code, waiting) = bob.json(&bobs, &["claim", "gh:acme/widgets#42", "--area", "billing"]);
    assert_eq!(code, Some(0), "{waiting}");
    assert_eq!(waiting["claim"]["areas"], serde_json::json!([]));
    assert_eq!(waiting["pending"][0]["holder"], "alice");
    assert_eq!(waiting["pending"][0]["asked"], true);

    let held = bob.before_edit(&bobs, "feature-session", &["packages/billing/rates.rs"]);
    assert_eq!(held["decision"], "deny", "{held}");
    assert!(
        held["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("Your claim waits for it to be handed over")),
        "{held}"
    );

    let (_, inbox) = alice.json(&alices, &["inbox"]);
    let asked = inbox["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|thread| {
            thread["thread"]["subject"].as_str().is_some_and(|subject| {
                subject.starts_with("Yield default:billing to gh:acme/widgets#42")
            })
        })
        .count();
    assert_eq!(asked, 1, "{inbox}");

    let (code, narrowed) = alice.json(
        &alices,
        &["claim", "narrow", "gh:acme/widgets#7", "--area", "billing"],
    );
    assert_eq!(code, Some(0), "{narrowed}");
    assert_eq!(narrowed["handed_over"][0]["holder"], "bob");

    let (_, status) = bob.json(&bobs, &["status"]);
    let holds: Vec<(String, String)> = status["holds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hold| {
            (
                hold["area"].as_str().unwrap_or_default().to_owned(),
                hold["holder"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert!(
        holds.contains(&("default:billing".to_owned(), "bob".to_owned())),
        "{status}"
    );
    assert!(
        holds.contains(&("default:search".to_owned(), "alice".to_owned())),
        "{status}"
    );
    let allowed = bob.before_edit(&bobs, "feature-session", &["packages/billing/rates.rs"]);
    assert_eq!(allowed["decision"], "no_objection", "{allowed}");
}
