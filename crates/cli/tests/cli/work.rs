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
            .ends_with(
                "It is held by alice on item gh:acme/api#1. Leave api:billing out of your claim and ask alice for a grant there: dibs ask-grant <your item> --area api:billing (MCP: dibs_ask_grant)."
            ),
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
    let said = text(&taken.stdout);
    assert!(said.contains("from alice"), "{said}");
    assert!(
        said.contains("they were told why in their claim's conversation")
            && !said.contains("conversation #"),
        "the answer names no conversation the taker cannot read: {said}"
    );

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
fn ask_grant_names_who_was_asked_or_why_nobody_was() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#1", "--title", "Invoices"],
    );
    run(&setup.bob, &["new", "gh:acme/api#2", "--title", "Refunds"]);
    for (who, item, area) in [
        (&setup.alice, "gh:acme/api#1", "billing"),
        (&setup.bob, "gh:acme/api#2", "search"),
    ] {
        assert_eq!(
            status(&run(who, &["claim", item, "--area", area])),
            0,
            "{item}"
        );
    }

    let asked = run(
        &setup.bob,
        &["ask-grant", "gh:acme/api#2", "--area", "billing"],
    );
    assert_eq!(status(&asked), 0, "{}", text(&asked.stderr));
    let said = text(&asked.stdout);
    for part in [
        "Asked alice, whose claim on gh:acme/api#1 holds api:billing",
        "for gh:acme/api#2 (conversation ",
    ] {
        assert!(said.contains(part), "{part}: {said}");
    }
    let inbox = run(&setup.alice, &["inbox"]);
    assert!(
        text(&inbox.stdout).contains("A grant on api:billing for gh:acme/api#2"),
        "{}",
        text(&inbox.stdout)
    );

    let own = run(
        &setup.bob,
        &["ask-grant", "gh:acme/api#2", "--area", "search"],
    );
    assert_eq!(
        text(&own.stdout).trim(),
        "Your own claim holds api:search: no grant is needed."
    );
    let refused = run(
        &setup.bob,
        &["ask-grant", "gh:acme/api#1", "--area", "billing"],
    );
    assert_eq!(status(&refused), 1);
    assert!(
        text(&refused.stderr).contains("no live claim on that work item"),
        "{}",
        text(&refused.stderr)
    );
}

#[test]
fn dibs_grants_a_request_to_a_claim_in_review_and_revoke_grant_undoes_it() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#1", "--title", "Invoices"],
    );
    run(&setup.bob, &["new", "gh:acme/api#2", "--title", "Refunds"]);
    run(
        &setup.alice,
        &["claim", "gh:acme/api#1", "--area", "billing"],
    );
    run(&setup.bob, &["claim", "gh:acme/api#2"]);
    let reviewed = run(
        &setup.alice,
        &["claim", "review", "gh:acme/api#1", "--pr", "acme/api#12"],
    );
    assert_eq!(status(&reviewed), 0, "{}", text(&reviewed.stderr));

    let asked = run(
        &setup.bob,
        &["ask-grant", "gh:acme/api#2", "--area", "billing"],
    );
    assert_eq!(status(&asked), 0, "{}", text(&asked.stderr));
    let said = text(&asked.stdout);
    assert!(
        said.starts_with(
            "dibs granted you edits in api:billing for gh:acme/api#2 on alice's behalf, because the claim holding the area is in review (grant "
        ),
        "{said}"
    );
    assert!(
        said.trim_end().ends_with("You may edit there now."),
        "{said}"
    );

    run(&setup.bob, &["release", "gh:acme/api#2"]);
    run(&setup.bob, &["claim", "gh:acme/api#2"]);
    let again = run(
        &setup.bob,
        &["ask-grant", "gh:acme/api#2", "--area", "billing", "--json"],
    );
    let answer = json(&again);
    assert_eq!(answer["outcome"], "granted", "{answer}");
    assert_eq!(answer["decided"], "in_review");
    let grant = answer["grant_id"]
        .as_u64()
        .expect("dibs gave a grant to bob's new claim")
        .to_string();

    let refused = run(&setup.bob, &["revoke-grant", &grant]);
    assert_eq!(status(&refused), 1);
    assert!(
        text(&refused.stderr)
            .contains("Only the member who granted this, or an admin, may revoke it."),
        "{}",
        text(&refused.stderr)
    );
    let revoked = run(&setup.alice, &["revoke-grant", &grant]);
    assert_eq!(status(&revoked), 0, "{}", text(&revoked.stderr));
    let said = text(&revoked.stdout);
    assert!(
        said.starts_with(&format!(
            "Revoked grant {grant}: bob may no longer edit in api:billing for #"
        )),
        "{said}"
    );
    let gone = run(&setup.alice, &["revoke-grant", &grant]);
    assert_eq!(status(&gone), 1, "revoked already");
    let bad = run(&setup.alice, &["revoke-grant", "zero"]);
    assert_eq!(status(&bad), 2);
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
fn a_wide_claim_says_when_dibs_acknowledged_an_announcement_for_a_holder_in_review() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#1", "--title", "Invoices"],
    );
    run(&setup.bob, &["new", "gh:acme/api#2", "--title", "Upgrade"]);
    run(
        &setup.alice,
        &["claim", "gh:acme/api#1", "--area", "billing"],
    );
    let reviewed = run(
        &setup.alice,
        &["claim", "review", "gh:acme/api#1", "--pr", "acme/api#12"],
    );
    assert_eq!(status(&reviewed), 0, "{}", text(&reviewed.stderr));

    let wide = run(
        &setup.bob,
        &[
            "claim",
            "gh:acme/api#2",
            "--wide",
            "billing",
            "--reason",
            "Upgrade the HTTP client",
        ],
    );

    assert_eq!(status(&wide), 0, "{}", text(&wide.stderr));
    let said = text(&wide.stdout);
    assert!(
        said.contains("dibs acknowledged the announcement to alice (message "),
        "{said}"
    );
    assert!(
        said.contains(
            "on their behalf, because the claim holding the area is in review: you may edit api:billing now."
        ),
        "{said}"
    );
    assert!(!said.contains("Asked alice to acknowledge"), "{said}");
}

#[test]
fn a_claim_inside_a_wide_claim_says_which_message_to_read_and_what_acknowledging_it_grants() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#1", "--title", "Invoices"],
    );
    run(&setup.bob, &["new", "gh:acme/api#2", "--title", "Upgrade"]);
    let wide = run(
        &setup.bob,
        &[
            "claim",
            "gh:acme/api#2",
            "--wide",
            "billing",
            "--reason",
            "Upgrade the HTTP client",
        ],
    );
    assert_eq!(status(&wide), 0, "{}", text(&wide.stderr));

    let claimed = run(
        &setup.alice,
        &["claim", "gh:acme/api#1", "--area", "billing"],
    );
    assert_eq!(status(&claimed), 0, "{}", text(&claimed.stderr));
    let inbox = run(&setup.alice, &["inbox", "--unacked", "--json"]);
    let message = json(&inbox)["unacked"][0]["message"]["id"]
        .as_u64()
        .unwrap();
    let said = text(&claimed.stdout);
    assert!(
        said.contains("api:billing is in bob's wide claim on gh:acme/api#2"),
        "{said}"
    );
    assert!(
        said.contains(&format!(
            "read message {message}: acknowledging it (dibs ack {message}) lets bob edit there, so acknowledge it only when your work will change no file there."
        )),
        "{said}"
    );
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

#[test]
fn a_claim_names_its_branch_and_a_pull_requests_head_replaces_it() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let alice = |args: &[&str]| setup.dibs(&setup.alice, &api).args(args).output().unwrap();
    assert_eq!(
        status(&alice(&["new", "gh:acme/api#42", "--title", "Refunds"])),
        0
    );

    let claimed = alice(&["claim", "gh:acme/api#42", "--branch", "bob/42-refunds"]);
    assert_eq!(status(&claimed), 0, "{}", text(&claimed.stderr));
    assert!(
        text(&claimed.stdout).contains("on branch api:bob/42-refunds (named)"),
        "{}",
        text(&claimed.stdout)
    );
    let shown = text(&alice(&["status"]).stdout);
    assert!(
        shown.contains("on branch api:bob/42-refunds (named)"),
        "{shown}"
    );

    let both = alice(&[
        "claim",
        "gh:acme/api#42",
        "--branch",
        "acme/web:bob/42-refunds-ui",
        "--json",
    ]);
    assert_eq!(status(&both), 0, "{}", text(&both.stderr));
    let answer = json(&both);
    assert_eq!(answer["claim"]["branches"]["web"], "bob/42-refunds-ui");
    assert_eq!(answer["claim"]["branch_sources"]["web"], "named");
    assert_eq!(answer["claim"]["branches"]["api"], "bob/42-refunds");

    let twice = alice(&[
        "claim",
        "gh:acme/api#42",
        "--branch",
        "bob/42-refunds",
        "--branch",
        "api:bob/42-other",
    ]);
    assert_eq!(status(&twice), 2, "{}", text(&twice.stderr));
    assert!(
        text(&twice.stderr).contains("two branches"),
        "{}",
        text(&twice.stderr)
    );
    let unlisted = alice(&["claim", "gh:acme/api#42", "--branch", "acme/other:bob/42"]);
    assert_eq!(status(&unlisted), 1, "{}", text(&unlisted.stderr));
    let empty = alice(&["claim", "gh:acme/api#42", "--branch", ":bob/42-refunds"]);
    assert_eq!(status(&empty), 2, "a usage error before any call");

    let headless = alice(&["claim", "review", "gh:acme/api#42", "--head", "bob/42"]);
    assert_eq!(status(&headless), 2, "--head needs --pr");

    let reviewed = alice(&[
        "claim",
        "review",
        "gh:acme/api#42",
        "--pr",
        "acme/api#12",
        "--head",
        "bob/42-refunds-v2",
    ]);
    assert_eq!(status(&reviewed), 0, "{}", text(&reviewed.stderr));
    let shown = text(&alice(&["status"]).stdout);
    assert!(
        shown.contains(concat!(
            "on branches api:bob/42-refunds-v2 (from its pull request), ",
            "web:bob/42-refunds-ui (named)"
        )),
        "{shown}"
    );
}

#[test]
fn a_branch_named_outside_a_checkout_needs_its_repository_when_the_project_has_several() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let created = setup
        .dibs(&setup.alice, &api)
        .args(["new", "gh:acme/api#42", "--title", "Refunds"])
        .output()
        .unwrap();
    assert_eq!(status(&created), 0);
    let elsewhere = setup.elsewhere();
    let alice = |args: &[&str]| {
        setup
            .dibs(&setup.alice, &elsewhere)
            .args(args)
            .output()
            .unwrap()
    };
    let unsaid = alice(&["claim", "gh:acme/api#42", "--branch", "bob/42-refunds"]);
    assert_eq!(status(&unsaid), 2, "{}", text(&unsaid.stderr));
    assert!(
        text(&unsaid.stderr).contains("<repo>:<branch>"),
        "{}",
        text(&unsaid.stderr)
    );
    let said = alice(&[
        "claim",
        "gh:acme/api#42",
        "--branch",
        "api:bob/42-refunds",
        "--json",
    ]);
    assert_eq!(status(&said), 0, "{}", text(&said.stderr));
    assert_eq!(json(&said)["claim"]["branch_sources"]["api"], "named");
}

#[test]
fn the_status_names_each_overlap_with_the_way_on() {
    use dibs_domain::member::Role;

    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let worker = setup
        .server
        .member(&setup.alice, "worker", Role::Importer)
        .unwrap();
    let mut items = Vec::new();
    for (who, reference) in [
        (&setup.alice, "gh:acme/api#1"),
        (&setup.bob, "gh:acme/api#2"),
    ] {
        let out = setup
            .dibs(who, &api)
            .args(["new", reference, "--title", "Rates"])
            .output()
            .unwrap();
        assert_eq!(status(&out), 0, "{}", text(&out.stderr));
        let claimed = setup
            .dibs(who, &api)
            .args(["claim", reference, "--json"])
            .output()
            .unwrap();
        assert_eq!(status(&claimed), 0, "{}", text(&claimed.stderr));
        let item = json(&claimed)["claim"]["item"].as_u64().unwrap();
        let synced = setup
            .dibs(who, &api)
            .args(["sync", "--force", "--quiet"])
            .output()
            .unwrap();
        assert_eq!(status(&synced), 0, "{}", text(&synced.stderr));
        items.push(item);
    }
    for item in &items {
        setup
            .server
            .footprint(&worker, *item, "api", "main", "abcd", &["config/app.yaml"])
            .unwrap();
    }
    let out = setup.dibs(&setup.bob, &api).arg("status").output().unwrap();
    let shown = text(&out.stdout);
    assert!(
        shown.contains("Overlaps (each claim's conversation says which change lands first")
            && shown.contains(
                "same file: gh:acme/api#1 (alice) and gh:acme/api#2 (bob) on api:config/app.yaml"
            ),
        "{shown}"
    );
}

#[test]
fn a_feature_takes_an_area_from_debt_work_through_the_command_line() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#7", "--title", "Tidy billing"],
    );
    run(
        &setup.bob,
        &["new", "gh:acme/api#42", "--title", "Currency on invoices"],
    );
    let debt = run(
        &setup.alice,
        &["claim", "gh:acme/api#7", "--area", "billing", "--debt"],
    );
    assert_eq!(status(&debt), 0, "{}", text(&debt.stderr));
    assert!(
        text(&debt.stdout).contains("debt work, areas api:billing"),
        "{}",
        text(&debt.stdout)
    );

    let feature = run(
        &setup.bob,
        &["claim", "gh:acme/api#42", "--area", "billing"],
    );
    assert_eq!(status(&feature), 0, "{}", text(&feature.stderr));
    let said = text(&feature.stdout);
    for part in [
        "waiting for api:billing",
        "api:billing waits for alice's debt claim on gh:acme/api#7 to hand it over",
        "dibs asked them to hand it over",
    ] {
        assert!(said.contains(part), "{part}: {said}");
    }

    let narrowed = run(
        &setup.alice,
        &["claim", "narrow", "gh:acme/api#7", "--area", "billing"],
    );
    assert_eq!(status(&narrowed), 0, "{}", text(&narrowed.stderr));
    let said = text(&narrowed.stdout);
    assert!(
        said.starts_with("Gave up api:billing on gh:acme/api#7"),
        "{said}"
    );
    assert!(
        said.contains("api:billing is bob's now, for gh:acme/api#42"),
        "{said}"
    );
    let again = run(
        &setup.alice,
        &[
            "claim",
            "narrow",
            "gh:acme/api#7",
            "--area",
            "billing",
            "--json",
        ],
    );
    assert_eq!(status(&again), 0);
    assert_eq!(json(&again)["areas"], serde_json::json!([]));

    let bobs = run(
        &setup.bob,
        &["claim", "gh:acme/api#42", "--area", "billing", "--json"],
    );
    let answer = json(&bobs);
    assert_eq!(answer["claim"]["areas"], serde_json::json!(["api:billing"]));
    assert_eq!(answer["claim"]["pending"], serde_json::json!({}));
    assert_eq!(answer["claim"]["kind"], "feature");

    let not_theirs = run(
        &setup.bob,
        &["claim", "narrow", "gh:acme/api#7", "--area", "billing"],
    );
    assert_ne!(status(&not_theirs), 0);
}

#[test]
fn a_takeover_needs_no_reason() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(&setup.alice, &["new", "gh:acme/api#4", "--title", "Search"]);
    run(&setup.alice, &["claim", "gh:acme/api#4"]);
    setup.server.advance(Duration::hours(2));
    let taken = run(&setup.bob, &["takeover", "gh:acme/api#4", "--json"]);
    assert_eq!(status(&taken), 0, "{}", text(&taken.stderr));
    assert_eq!(json(&taken)["previous_holder"], "alice");
    assert_eq!(json(&taken)["claim"]["holder"], "bob");
}

#[test]
fn a_takeover_names_the_kind_of_the_new_claim_and_a_renewal_cannot_change_it() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(
        &setup.alice,
        &["new", "gh:acme/api#3", "--title", "Clean-up"],
    );
    run(
        &setup.alice,
        &["claim", "gh:acme/api#3", "--area", "billing", "--debt"],
    );
    let renewed = run(&setup.alice, &["claim", "gh:acme/api#3", "--json"]);
    assert_eq!(status(&renewed), 0, "a renewal without a kind keeps it");
    assert_eq!(json(&renewed)["claim"]["kind"], "debt");
    setup.server.advance(Duration::hours(2));
    let taken = run(
        &setup.bob,
        &[
            "takeover",
            "gh:acme/api#3",
            "--reason",
            "a feature needs it",
            "--feature",
            "--json",
        ],
    );
    assert_eq!(status(&taken), 0, "{}", text(&taken.stderr));
    assert_eq!(json(&taken)["claim"]["kind"], "feature");
    let changed = run(&setup.bob, &["claim", "gh:acme/api#3", "--debt"]);
    assert_eq!(status(&changed), 1);
    assert!(
        text(&changed.stderr)
            .contains("The claim was made as feature; release it and claim again to change it"),
        "{}",
        text(&changed.stderr)
    );
}
