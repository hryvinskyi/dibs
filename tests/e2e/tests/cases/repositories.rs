//! One project over two repositories: a claim spanning both, and a hook in each checkout that
//! answers for its own repository.

use serde_json::Value;

use crate::harness::Team;

const SETTINGS: &str = r#"
[[repos]]
id = "api"
remote = "git@github.com:acme/api.git"

[[repos.areas]]
name = "billing"
paths = ["src/billing/**"]

[[repos]]
id = "web"
remote = "git@github.com:acme/web.git"

[[repos.areas]]
name = "checkout"
paths = ["src/checkout/**"]
"#;

fn decision(reply: &Value) -> &str {
    reply["decision"].as_str().unwrap_or_default()
}

#[test]
fn a_claim_over_two_repositories_is_enforced_by_the_hook_of_each_checkout() {
    let team = Team::start(SETTINGS);
    let api = team.checkout("api", Some("api"), "git@github.com:acme/api.git");
    let web = team.checkout("web", Some("web"), "git@github.com:acme/web.git");
    let (alice, bob) = (&team.alice, &team.bob);

    // An item in one repository whose change needs the other's area too.
    alice.ok(
        &api,
        &["new", "gh:acme/api#7", "--title", "Express checkout"],
    );
    alice.ok(
        &api,
        &[
            "claim",
            "gh:acme/api#7",
            "--area",
            "api:billing",
            "--area",
            "web:checkout",
        ],
    );
    let (_, status) = alice.json(&api, &["status"]);
    let mut held: Vec<&str> = status["holds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hold| hold["area"].as_str().unwrap())
        .collect();
    held.sort_unstable();
    assert_eq!(held, ["api:billing", "web:checkout"]);

    // Each checkout's hook refuses bob in the area of its own repository, naming alice.
    for (checkout, path) in [
        (&api, "src/billing/invoice.rs"),
        (&web, "src/checkout/cart.ts"),
    ] {
        let denied = bob.before_edit(checkout, "bob-session", &[path]);
        assert_eq!(decision(&denied), "deny", "{path}: {denied}");
        assert!(
            denied["reason"].as_str().unwrap().contains("alice"),
            "{denied}"
        );
        let own = alice.before_edit(checkout, "alice-session", &[path]);
        assert_eq!(decision(&own), "no_objection", "{path}: {own}");
    }

    // A path is judged by the repository of the checkout it is edited in: the other repository's
    // area does not govern it.
    for (checkout, path) in [
        (&api, "src/checkout/cart.ts"),
        (&web, "src/billing/invoice.rs"),
    ] {
        let reply = bob.before_edit(checkout, "bob-session", &[path]);
        assert_eq!(decision(&reply), "no_objection", "{path}: {reply}");
    }

    // An item declaring one of the held areas is refused, whichever repository it names.
    bob.ok(&web, &["new", "gh:acme/web#3", "--title", "Cart badge"]);
    let (code, refusal) = bob.json(&web, &["claim", "gh:acme/web#3", "--area", "web:checkout"]);
    assert_eq!(code, Some(1), "{refusal}");
    assert_eq!(refusal["error"]["code"], "area_held", "{refusal}");
    let message = refusal["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("web:checkout") && message.contains("alice"),
        "{message}"
    );
}
