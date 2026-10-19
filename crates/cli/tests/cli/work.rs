//! Items and claims through the command line.

use time::Duration;

use crate::support::{API_REMOTE, Setup, json, status, text};

#[test]
fn an_item_is_created_claimed_listed_and_completed() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let alice = |args: &[&str]| setup.dibs(&setup.alice, &api).args(args).output().unwrap();

    let created = alice(&["new", "gh:acme/api#7", "--title", "Fix billing rounding"]);
    assert_eq!(status(&created), 0);
    assert!(text(&created.stdout).starts_with("Created #"));
    let again = alice(&[
        "new",
        "gh:acme/api#7",
        "--title",
        "Fix billing rounding",
        "--json",
    ]);
    assert_eq!(status(&again), 0);
    assert_eq!(json(&again)["created"], false);

    let claimed = alice(&["claim", "gh:acme/api#7", "--area", "billing", "--json"]);
    assert_eq!(status(&claimed), 0);
    let answer = json(&claimed);
    assert_eq!(answer["outcome"], "new");
    assert_eq!(answer["claim"]["areas"][0], "api:billing");
    assert_eq!(answer["claim"]["holder"], "alice");

    let mine = alice(&["items", "--mine"]);
    assert_eq!(status(&mine), 0);
    assert!(text(&mine.stdout).contains("gh:acme/api#7 Fix billing rounding [claimed]"));
    let open = alice(&["items", "--status", "open"]);
    assert!(text(&open.stdout).contains("No work items match."));

    let state = alice(&["status"]);
    assert_eq!(status(&state), 0);
    let shown = text(&state.stdout);
    assert!(
        shown.contains("Live claims:") && shown.contains("areas api:billing"),
        "{shown}"
    );

    let done = alice(&["done", "gh:acme/api#7"]);
    assert_eq!(status(&done), 0);
    assert!(text(&done.stdout).starts_with("Completed #"));
}

#[test]
fn a_claim_in_the_way_is_refused_naming_the_holder() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#1", "--title", "Billing"],
    );
    run(
        &setup.bob,
        &["new", "gh:acme/api#2", "--title", "More billing"],
    );
    assert_eq!(
        status(&run(
            &setup.alice,
            &["claim", "gh:acme/api#1", "--area", "billing"]
        )),
        0
    );

    let held = run(&setup.bob, &["claim", "gh:acme/api#1"]);
    assert_eq!(status(&held), 1);
    assert!(
        text(&held.stderr).contains("alice"),
        "{}",
        text(&held.stderr)
    );

    let area = run(
        &setup.bob,
        &["claim", "gh:acme/api#2", "--area", "billing", "--json"],
    );
    assert_eq!(status(&area), 1);
    assert_eq!(json(&area)["error"]["code"], "area_held");

    let missing = run(&setup.bob, &["claim", "gh:acme/api#404"]);
    assert_eq!(status(&missing), 1);
    assert!(text(&missing.stderr).contains("dibs new gh:acme/api#404"));

    let not_mine = run(&setup.bob, &["release", "gh:acme/api#1"]);
    assert_eq!(status(&not_mine), 1);
}

#[test]
fn a_stale_claim_is_taken_over_with_a_reason_and_a_grant_lets_another_edit() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#1", "--title", "Billing"],
    );
    run(
        &setup.alice,
        &["claim", "gh:acme/api#1", "--area", "billing"],
    );

    let early = run(
        &setup.bob,
        &["takeover", "gh:acme/api#1", "--reason", "taking it"],
    );
    assert_eq!(status(&early), 1);
    setup.server.advance(Duration::hours(2));
    let taken = run(
        &setup.bob,
        &["takeover", "gh:acme/api#1", "--reason", "alice is away"],
    );
    assert_eq!(status(&taken), 0);
    assert!(text(&taken.stdout).contains("from alice"));

    run(&setup.alice, &["new", "gh:acme/api#2", "--title", "Search"]);
    assert_eq!(
        status(&run(
            &setup.alice,
            &["claim", "gh:acme/api#2", "--area", "search"]
        )),
        0
    );
    let granted = run(
        &setup.alice,
        &["grant", "gh:acme/api#1", "--area", "search", "--to", "bob"],
    );
    assert_eq!(status(&granted), 0);
    assert!(text(&granted.stdout).contains("bob may edit in api:search"));
    let check = run(&setup.bob, &["check", "src/search/a.rs"]);
    assert_eq!(status(&check), 0);
    assert!(text(&check.stdout).contains("granted by alice"));
}
