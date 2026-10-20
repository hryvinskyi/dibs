//! `dibs admin`.

use crate::support::{API_REMOTE, Setup, json, status, text};

#[test]
fn an_admin_adds_a_member_whose_printed_token_signs_in() {
    let setup = Setup::new();
    let dir = setup.elsewhere();
    let out = setup
        .dibs(&setup.alice, &dir)
        .args([
            "admin", "member", "add", "carol", "--name", "Carol", "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    let token = json(&out)["token"].as_str().unwrap().to_owned();
    assert_eq!(status(&setup.login_as("carol", &token)), 0);

    let rotated = setup
        .dibs(&setup.alice, &dir)
        .args(["admin", "token", "rotate", "carol"])
        .output()
        .unwrap();
    assert_eq!(status(&rotated), 0);
    assert!(text(&rotated.stdout).contains("shown only this once"));
    let stale = setup.dibs_as("carol", &dir).arg("whoami").output().unwrap();
    assert_eq!(status(&stale), 2);
    assert!(text(&stale.stderr).contains("dibs login"));

    let bot = setup
        .dibs(&setup.alice, &dir)
        .args([
            "admin", "member", "add", "board", "--name", "Board", "--role", "importer", "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&bot), 0, "{}", text(&bot.stderr));
    assert_eq!(json(&bot)["member"]["role"], "importer");
    let unknown_role = setup
        .dibs(&setup.alice, &dir)
        .args([
            "admin", "member", "add", "erin", "--name", "Erin", "--role", "owner",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&unknown_role), 2);

    let forbidden = setup
        .dibs(&setup.bob, &dir)
        .args(["admin", "member", "add", "dave", "--name", "Dave"])
        .output()
        .unwrap();
    assert_eq!(status(&forbidden), 1);
}

#[test]
fn settings_are_applied_from_a_file() {
    let setup = Setup::new();
    let dir = setup.elsewhere();
    let file = dir.join("settings.toml");
    std::fs::write(
        &file,
        "stale_after = \"2h\"\nedit_policy = \"advise\"\n\n[[areas]]\nname = \"billing\"\npaths = [\"billing/**\"]\n",
    )
    .unwrap();
    let apply = |path: &std::path::Path| {
        setup
            .dibs(&setup.alice, &dir)
            .args(["admin", "settings", "apply"])
            .arg(path)
            .output()
            .unwrap()
    };
    let first = apply(&file);
    assert_eq!(status(&first), 0);
    assert!(text(&first.stdout).contains("applied"));
    let again = apply(&file);
    assert!(text(&again.stdout).contains("nothing changed"));

    let bad = dir.join("bad.toml");
    std::fs::write(&bad, "stale_after = 3\n").unwrap();
    assert_eq!(status(&apply(&bad)), 2);
    let refused = dir.join("refused.toml");
    std::fs::write(&refused, "stale_after = \"1s\"\n").unwrap();
    let out = apply(&refused);
    assert_eq!(status(&out), 1);
}

#[test]
fn an_admin_force_releases_a_claim() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let run = |who, args: &[&str]| setup.dibs(who, &api).args(args).output().unwrap();
    run(&setup.bob, &["new", "gh:acme/api#3", "--title", "Search"]);
    assert_eq!(status(&run(&setup.bob, &["claim", "gh:acme/api#3"])), 0);
    assert_eq!(
        status(&run(
            &setup.bob,
            &["admin", "force-release", "gh:acme/api#3"]
        )),
        1
    );
    let out = run(&setup.alice, &["admin", "force-release", "gh:acme/api#3"]);
    assert_eq!(status(&out), 0);
    assert!(text(&out.stdout).contains("Force-released"));
}

#[test]
fn an_admin_removes_a_member_and_deletes_a_conversation_from_the_command_line() {
    let setup = Setup::new();
    let dir = setup.elsewhere();
    let added = setup
        .dibs(&setup.alice, &dir)
        .args([
            "admin", "member", "add", "carol", "--name", "Carol", "--github", "carol-gh", "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&added), 0, "{}", text(&added.stderr));
    assert_eq!(json(&added)["member"]["external_login"], "carol-gh");
    let token = json(&added)["token"].as_str().unwrap().to_owned();
    assert_eq!(status(&setup.login_as("carol", &token)), 0);
    let sent = setup
        .dibs_as("carol", &dir)
        .args([
            "send",
            "--to",
            "alice",
            "--subject",
            "Rates",
            "Where is the table?",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&sent), 0, "{}", text(&sent.stderr));
    let thread = json(&sent)["thread"]["id"].as_u64().unwrap().to_string();

    let removed = setup
        .dibs(&setup.alice, &dir)
        .args(["admin", "member", "remove", "carol"])
        .output()
        .unwrap();
    assert_eq!(status(&removed), 0, "{}", text(&removed.stderr));
    assert!(
        text(&removed.stdout).starts_with("Removed carol from the project: 1 token(s) revoked"),
        "{}",
        text(&removed.stdout)
    );
    let gone = setup.dibs_as("carol", &dir).arg("whoami").output().unwrap();
    assert_eq!(status(&gone), 2, "her token no longer signs in");
    let refused = setup
        .dibs(&setup.bob, &dir)
        .args(["admin", "member", "remove", "alice"])
        .output()
        .unwrap();
    assert_eq!(status(&refused), 1);

    let read = setup
        .dibs(&setup.alice, &dir)
        .args(["thread", &thread])
        .output()
        .unwrap();
    assert!(text(&read.stdout).contains("carol ("), "her messages stay");
    let deleted = setup
        .dibs(&setup.alice, &dir)
        .args(["admin", "thread", "delete", &thread])
        .output()
        .unwrap();
    assert_eq!(status(&deleted), 0, "{}", text(&deleted.stderr));
    assert!(text(&deleted.stdout).contains("with its 1 message(s)"));
    let missing = setup
        .dibs(&setup.alice, &dir)
        .args(["thread", &thread])
        .output()
        .unwrap();
    assert_eq!(status(&missing), 1);
}

#[test]
fn a_member_escalates_from_the_command_line_and_the_arbiter_reads_it_from_dibs() {
    let setup = Setup::new();
    let dir = setup.elsewhere();
    let mut settings = dibs_test_server::two_repositories();
    settings["arbiters"] = serde_json::json!(["alice"]);
    setup.server.settings(&setup.alice, settings).unwrap();
    let created = setup
        .dibs(&setup.bob, &dir)
        .args(["new", "gh:acme/api#77", "--title", "Rates", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&created), 0, "{}", text(&created.stderr));

    let none = setup
        .dibs(&setup.bob, &dir)
        .args(["escalate", "--reason", "who goes first?"])
        .output()
        .unwrap();
    assert_eq!(status(&none), 2, "a subject is required");
    let sent = setup
        .dibs(&setup.bob, &dir)
        .args([
            "escalate",
            "gh:acme/api#77",
            "--reason",
            "alice's agent and mine both need the rate table",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&sent), 0, "{}", text(&sent.stderr));
    let said = text(&sent.stdout);
    assert!(said.starts_with("Escalated in conversation #"), "{said}");
    assert!(said.contains("(arbiters: alice)"), "{said}");

    let board = setup
        .dibs(&setup.alice, &dir)
        .arg("status")
        .output()
        .unwrap();
    let shown = text(&board.stdout);
    assert!(
        shown.contains("Escalated by bob: work item gh:acme/api#77 \"Rates\" — waiting for alice"),
        "{shown}"
    );
    let inbox = setup
        .dibs(&setup.alice, &dir)
        .args(["inbox", "--unacked"])
        .output()
        .unwrap();
    assert!(
        text(&inbox.stdout).contains("from dibs in #"),
        "{}",
        text(&inbox.stdout)
    );
    let slot = setup
        .dibs(&setup.bob, &dir)
        .args(["escalate", "--slot", "ghost", "--reason", "?"])
        .output()
        .unwrap();
    assert_eq!(status(&slot), 1);
    assert!(
        text(&slot.stderr).contains("ghost"),
        "{}",
        text(&slot.stderr)
    );
}
