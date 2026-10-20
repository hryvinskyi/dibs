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
    assert_eq!(answer["item"]["ref"], "gh:acme/api#7");
    let renewed = alice(&["claim", "gh:acme/api#7", "--area", "billing"]);
    assert!(
        text(&renewed.stdout).starts_with("Renewed your claim on gh:acme/api#7 (alice on "),
        "{}",
        text(&renewed.stdout)
    );

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
    assert!(
        text(&done.stdout).starts_with("Completed gh:acme/api#7 ("),
        "the item is named by its reference: {}",
        text(&done.stdout)
    );
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
    let refusal = json(&area);
    assert_eq!(refusal["error"]["code"], "area_held");
    assert_eq!(refusal["error"]["details"]["holder"], "alice");
    assert_eq!(refusal["error"]["details"]["area"], "api:billing");
    assert_eq!(refusal["error"]["details"]["item_ref"], "gh:acme/api#1");
    assert!(
        refusal["error"]["message"]
            .as_str()
            .unwrap()
            .ends_with("It is held by alice on item gh:acme/api#1."),
        "{refusal}"
    );

    let missing = run(&setup.bob, &["claim", "gh:acme/api#404"]);
    assert_eq!(status(&missing), 1);
    assert!(text(&missing.stderr).contains("dibs new gh:acme/api#404"));

    let not_mine = run(&setup.bob, &["release", "gh:acme/api#1"]);
    assert_eq!(status(&not_mine), 1);
}

#[test]
fn ready_work_and_dependencies_from_the_command_line() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let alice = |args: &[&str]| setup.dibs(&setup.alice, &api).args(args).output().unwrap();
    alice(&["new", "gh:acme/api#1", "--title", "Waits"]);
    alice(&["new", "gh:acme/api#2", "--title", "Blocks"]);

    let added = alice(&[
        "dep",
        "add",
        "gh:Acme/Api#1",
        "--blocked-by",
        "gh:acme/api#2",
    ]);
    assert_eq!(status(&added), 0, "{}", text(&added.stderr));
    assert_eq!(
        text(&added.stdout).trim(),
        "gh:acme/api#1 is now blocked by gh:acme/api#2."
    );
    let cycle = alice(&[
        "dep",
        "add",
        "gh:acme/api#2",
        "--blocked-by",
        "gh:acme/api#1",
        "--json",
    ]);
    assert_eq!(status(&cycle), 1);
    assert_eq!(json(&cycle)["error"]["code"], "dependency_cycle");

    let ready = alice(&["ready"]);
    assert_eq!(status(&ready), 0);
    let shown = text(&ready.stdout);
    assert!(
        shown.contains("Ready to pick up, soonest first:\n  P2 gh:acme/api#2 Blocks"),
        "{shown}"
    );
    assert!(
        shown.contains("Not ready:\n  P2 gh:acme/api#1 Waits — blocked by gh:acme/api#2"),
        "{shown}"
    );
    let as_json = json(&alice(&["ready", "--json"]));
    assert_eq!(as_json["ready"][0]["item"]["ref"], "gh:acme/api#2");

    let claimed = alice(&["claim", "gh:acme/api#1"]);
    assert_eq!(status(&claimed), 0);
    assert!(
        text(&claimed.stdout).contains("It is blocked by gh:acme/api#2, which is not done yet"),
        "{}",
        text(&claimed.stdout)
    );

    let removed = alice(&[
        "dep",
        "rm",
        "gh:acme/api#1",
        "--blocked-by",
        "gh:acme/api#2",
    ]);
    assert_eq!(status(&removed), 0);
    assert_eq!(
        text(&removed.stdout).trim(),
        "gh:acme/api#1 is no longer blocked by gh:acme/api#2."
    );
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

#[test]
fn a_wide_claim_from_the_command_line_waits_for_the_holders_acknowledgement() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#1", "--title", "Invoices"],
    );
    run(&setup.bob, &["new", "gh:acme/api#2", "--title", "Upgrade"]);
    assert_eq!(
        status(&run(
            &setup.alice,
            &["claim", "gh:acme/api#1", "--area", "billing"]
        )),
        0
    );
    let no_reason = run(&setup.bob, &["claim", "gh:acme/api#2", "--wide", "billing"]);
    assert_eq!(status(&no_reason), 2, "a wide claim needs its reason");

    let wide = run(
        &setup.bob,
        &[
            "claim",
            "gh:acme/api#2",
            "--wide",
            "billing,search",
            "--reason",
            "Upgrade the HTTP client",
        ],
    );
    assert_eq!(status(&wide), 0, "{}", text(&wide.stderr));
    let said = text(&wide.stdout);
    assert!(
        said.contains("Wide over api:billing, api:search, holding none of them"),
        "{said}"
    );
    assert!(
        said.contains("Asked alice to acknowledge (message "),
        "{said}"
    );

    let state = run(&setup.alice, &["status"]);
    let shown = text(&state.stdout);
    assert!(shown.contains("Wide claims ("), "{shown}");
    assert!(
        shown.contains(
            "bob: Upgrade the HTTP client; api:billing (waiting for alice), api:search (free)"
        ),
        "{shown}"
    );

    std::fs::create_dir_all(api.join("src")).unwrap();
    let held = run(&setup.bob, &["check", "src/billing/a.rs"]);
    assert_eq!(status(&held), 1);
    assert!(
        text(&held.stdout)
            .contains("your wide claim names it; waiting for alice's acknowledgement"),
        "{}",
        text(&held.stdout)
    );

    let again = run(
        &setup.bob,
        &[
            "claim",
            "gh:acme/api#2",
            "--wide",
            "billing",
            "--reason",
            "Upgrade the HTTP client",
            "--json",
        ],
    );
    assert_eq!(json(&again)["announced"], serde_json::json!([]));
    let inbox = run(&setup.alice, &["inbox", "--unacked", "--json"]);
    let message = json(&inbox)["unacked"][0]["message"]["id"]
        .as_u64()
        .unwrap()
        .to_string();
    let acked = run(&setup.alice, &["ack", &message]);
    assert_eq!(status(&acked), 0);
    assert!(
        text(&acked.stdout).contains("That grants bob edits in api:billing for item"),
        "{}",
        text(&acked.stdout)
    );
    let granted = run(&setup.bob, &["check", "src/billing/a.rs"]);
    assert_eq!(status(&granted), 0, "{}", text(&granted.stdout));
}

#[test]
fn a_claim_goes_to_review_through_the_command_line_and_its_holder_reads_the_pull_requests() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let alice = |args: &[&str]| setup.dibs(&setup.alice, &api).args(args).output().unwrap();
    assert_eq!(
        status(&alice(&["new", "gh:acme/api#7", "--title", "Fix rounding"])),
        0
    );
    assert_eq!(
        status(&alice(&["claim", "gh:acme/api#7", "--area", "billing"])),
        0
    );
    let reviewed = alice(&["claim", "review", "gh:acme/api#7", "--pr", "acme/api#12"]);
    assert_eq!(status(&reviewed), 0, "{}", text(&reviewed.stderr));
    let said = text(&reviewed.stdout);
    assert!(
        said.starts_with("Your claim on gh:acme/api#7 is in review on acme/api#12"),
        "{said}"
    );
    assert!(said.contains("completes it"), "{said}");
    let more = alice(&[
        "claim",
        "review",
        "gh:acme/api#7",
        "--pr",
        "acme/web#3",
        "--more",
        "--json",
    ]);
    assert_eq!(status(&more), 0);
    let answer = json(&more);
    assert_eq!(answer["claim"]["state"], "in_review");
    assert_eq!(
        answer["claim"]["review"]["prs"],
        serde_json::json!(["acme/api#12", "acme/web#3"])
    );
    assert_eq!(answer["claim"]["review"]["more"], true);
    let shown = text(&alice(&["status"]).stdout);
    assert!(
        shown.contains("in review on acme/api#12, acme/web#3"),
        "{shown}"
    );
    let elsewhere = alice(&["claim", "review", "gh:acme/api#7", "--pr", "acme/other#1"]);
    assert_eq!(status(&elsewhere), 1, "{}", text(&elsewhere.stderr));
    assert!(
        text(&elsewhere.stderr).contains("no repository this project lists"),
        "{}",
        text(&elsewhere.stderr)
    );
    let malformed = alice(&["claim", "review", "gh:acme/api#7", "--pr", "acme/api"]);
    assert_eq!(status(&malformed), 2, "a usage error before any call");
    let dropped = alice(&[
        "claim",
        "review",
        "gh:acme/api#7",
        "--drop-pr",
        "acme/web#3",
        "--json",
    ]);
    assert_eq!(status(&dropped), 0, "{}", text(&dropped.stderr));
    assert_eq!(
        json(&dropped)["claim"]["review"]["prs"],
        serde_json::json!(["acme/api#12"])
    );
    let last = alice(&[
        "claim",
        "review",
        "gh:acme/api#7",
        "--drop-pr",
        "acme/api#12",
    ]);
    assert_eq!(status(&last), 0, "{}", text(&last.stderr));
    assert!(
        text(&last.stdout).contains("is no longer in review"),
        "{}",
        text(&last.stdout)
    );
    let unknown = alice(&[
        "claim",
        "review",
        "gh:acme/api#7",
        "--drop-pr",
        "acme/api#12",
    ]);
    assert_eq!(status(&unknown), 1);
}
