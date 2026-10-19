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
