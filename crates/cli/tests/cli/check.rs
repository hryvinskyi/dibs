//! `dibs check` on paths and on a range of commits, in nested checkouts and in a clone the
//! configuration does not belong to.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::support::{
    API_REMOTE, Setup, WEB_REMOTE, git, json, make_checkout, status, text, write,
};

/// Alice claims an item holding `api:billing`.
fn alice_holds_billing(setup: &Setup, api: &std::path::Path) {
    let run = |args: &[&str]| setup.dibs(&setup.alice, api).args(args).output().unwrap();
    assert_eq!(
        status(&run(&["new", "gh:acme/api#1", "--title", "Billing"])),
        0
    );
    assert_eq!(
        status(&run(&["claim", "gh:acme/api#1", "--area", "billing"])),
        0
    );
}

fn verdict_paths(answer: &Value) -> BTreeSet<String> {
    answer["verdicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["path"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn paths_are_checked_relative_to_the_checkout_and_a_held_one_is_refused() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    alice_holds_billing(&setup, &api);
    let src = api.join("src");
    std::fs::create_dir_all(&src).unwrap();

    let out = setup
        .dibs(&setup.bob, &src)
        .args(["check", "billing/a.rs", "../README.md", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 1);
    let answer = json(&out);
    let verdicts = answer["verdicts"].as_array().unwrap();
    assert_eq!(verdicts[0]["path"], "src/billing/a.rs");
    assert_eq!(verdicts[0]["verdict"], "held_by");
    assert_eq!(verdicts[0]["holder"], "alice");
    assert_eq!(verdicts[1]["path"], "README.md");
    assert_eq!(verdicts[1]["verdict"], "allowed");

    let free = setup
        .dibs(&setup.bob, &api)
        .args(["check", "README.md", "docs/guide.md"])
        .output()
        .unwrap();
    assert_eq!(status(&free), 0);
    assert!(text(&free.stdout).contains("README.md: allowed"));

    let outside = setup
        .dibs(&setup.bob, &api)
        .args(["check", "../elsewhere.rs"])
        .output()
        .unwrap();
    assert_eq!(status(&outside), 2);
    assert!(text(&outside.stderr).contains("outside the checkout"));
}

#[test]
fn a_range_warns_about_both_sides_of_a_rename_and_skips_merge_commits() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    alice_holds_billing(&setup, &api);
    write(&api, "src/billing/old.rs", "fn old() {}\n");
    write(&api, "docs/notes.md", "notes\n");
    git(&api, &["add", "-A"]);
    git(&api, &["commit", "-q", "-m", "base"]);
    let base = git(&api, &["rev-parse", "HEAD"]).trim().to_owned();

    std::fs::create_dir_all(api.join("src/core")).unwrap();
    git(&api, &["mv", "src/billing/old.rs", "src/core/new.rs"]);
    git(
        &api,
        &["commit", "-q", "-m", "move the helper out of billing"],
    );

    git(&api, &["checkout", "-q", "-b", "side", &base]);
    write(&api, "docs/notes.md", "more notes\n");
    git(
        &api,
        &[
            "-c",
            "user.email=carol@example.com",
            "-c",
            "user.name=Carol",
            "commit",
            "-q",
            "-am",
            "notes",
        ],
    );
    git(&api, &["checkout", "-q", "main"]);
    git(&api, &["merge", "-q", "--no-ff", "--no-commit", "side"]);
    write(&api, "src/billing/merge_only.rs", "fn resolved() {}\n");
    git(&api, &["add", "-A"]);
    git(&api, &["commit", "-q", "-m", "merge side"]);
    let range = format!("{base}..HEAD");

    let out = setup
        .dibs(&setup.bob, &api)
        .args(["check", "--git-range", &range])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    let warnings = text(&out.stderr);
    assert!(
        warnings.contains("warning: src/billing/old.rs: held by alice"),
        "{warnings}"
    );
    assert!(!warnings.contains("merge_only"), "{warnings}");
    assert!(!warnings.contains("src/core/new.rs"), "{warnings}");

    let out = setup
        .dibs(&setup.bob, &api)
        .args(["check", "--git-range", &range, "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    assert_eq!(
        verdict_paths(&json(&out)),
        BTreeSet::from([
            "docs/notes.md".to_owned(),
            "src/billing/old.rs".to_owned(),
            "src/core/new.rs".to_owned()
        ])
    );

    let mine = setup
        .dibs(&setup.bob, &api)
        .args(["check", "--git-range", &range, "--author", "me", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&mine), 0);
    assert_eq!(
        verdict_paths(&json(&mine)),
        BTreeSet::from([
            "src/billing/old.rs".to_owned(),
            "src/core/new.rs".to_owned()
        ])
    );

    let bad = setup
        .dibs(&setup.bob, &api)
        .args(["check", "--git-range", "no-such-ref..HEAD"])
        .output()
        .unwrap();
    assert_eq!(status(&bad), 0);
    assert!(text(&bad.stderr).contains("were not checked"));
}

#[test]
fn a_nested_checkout_answers_for_its_own_repository() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    std::fs::write(api.join(".gitignore"), "web/\n").unwrap();
    let web = api.join("web");
    make_checkout(&web, &setup.server.url(), "acme", Some("web"), WEB_REMOTE);
    let deep = web.join("app/checkout");
    std::fs::create_dir_all(&deep).unwrap();

    let out = setup
        .dibs(&setup.bob, &deep)
        .args(["check", "cart.ts", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 1);
    let verdict = &json(&out)["verdicts"][0];
    assert_eq!(verdict["repo"], "web");
    assert_eq!(verdict["path"], "app/checkout/cart.ts");
    assert_eq!(verdict["verdict"], "needs_claim");

    let outer = setup
        .dibs(&setup.bob, &api)
        .args(["check", "src/search/x.rs", "--json"])
        .output()
        .unwrap();
    assert_eq!(json(&outer)["verdicts"][0]["repo"], "api");
}

#[test]
fn a_nested_clone_without_its_own_file_is_governed_by_none() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    alice_holds_billing(&setup, &api);
    let vendored = api.join("vendor").join("search");
    std::fs::create_dir_all(&vendored).unwrap();
    git(&vendored, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(vendored.join("src/billing")).unwrap();

    let out = setup
        .dibs(&setup.bob, &vendored.join("src"))
        .args(["check", "billing/a.rs"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 2);
    let stderr = text(&out.stderr);
    assert!(stderr.contains("No .dibs.toml"), "{stderr}");
    assert!(!stderr.contains("alice"), "{stderr}");

    let range = setup
        .dibs(&setup.bob, &vendored)
        .args(["check", "--git-range", "HEAD~0..HEAD"])
        .output()
        .unwrap();
    assert_eq!(status(&range), 0);
    assert!(text(&range.stderr).contains("No .dibs.toml"));

    let plain = api.join("src").join("billing");
    std::fs::create_dir_all(&plain).unwrap();
    let held = setup
        .dibs(&setup.bob, &plain)
        .args(["check", "a.rs"])
        .output()
        .unwrap();
    assert_eq!(status(&held), 1);
    assert!(text(&held.stdout).contains("held by alice"));
}

#[test]
fn a_configuration_copied_into_another_clone_is_refused() {
    let setup = Setup::new();
    let copied = setup.checkout("web-clone", Some("api"), WEB_REMOTE);
    let out = setup
        .dibs(&setup.bob, &copied)
        .args(["check", "src/billing/a.rs"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 2);
    let stderr = text(&out.stderr);
    assert!(stderr.contains("github.com/acme/api"), "{stderr}");
    assert!(stderr.contains("github.com/acme/web"), "{stderr}");

    let range = setup
        .dibs(&setup.bob, &copied)
        .args(["check", "--git-range", "HEAD~0..HEAD"])
        .output()
        .unwrap();
    assert_eq!(status(&range), 0);
    assert!(text(&range.stderr).contains("github.com/acme/web"));

    let unknown = setup.checkout("search", Some("search"), API_REMOTE);
    let out = setup
        .dibs(&setup.bob, &unknown)
        .args(["check", "a.rs"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 2);
    assert!(text(&out.stderr).contains("api, web"));

    let no_repo = setup.checkout("bare", None, API_REMOTE);
    let out = setup
        .dibs(&setup.bob, &no_repo)
        .args(["check", "a.rs"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 2);
    assert!(text(&out.stderr).contains("names no repo"));
}
