//! `dibs check --push`, fed git's pre-push lines and run by git itself from a real `pre-push` hook
//! on a push to a bare repository: a push to a protected branch by a member who is not among its
//! pushers is refused naming who may push, a push to a ref a slot another member holds freezes is
//! refused naming them, anything else is silent, and a server that cannot be reached lets the push
//! go with one warning.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::json;

use dibs_test_server::{two_repositories, unreachable};

use crate::support::{API_REMOTE, Setup, git, make_checkout, status, text};

fn setup() -> Setup {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["slots"] = json!([
        {"name": "migrate", "max_hold": "1h", "freezes": ["api:refs/heads/main"]}
    ]);
    setup.server.settings(&setup.alice, settings).unwrap();
    setup
}

fn take(setup: &Setup, handle: &str, cwd: &Path) {
    let out = setup
        .dibs_as(handle, cwd)
        .args(["slot", "take", "migrate", "--purpose", "invoices to bigint"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
}

fn check(setup: &Setup, handle: &str, cwd: &Path, stdin: &str) -> Output {
    setup
        .dibs_as(handle, cwd)
        .args(["check", "--push"])
        .write_stdin(stdin.to_owned())
        .output()
        .unwrap()
}

const MAIN: &str = "refs/heads/main 1111111111111111111111111111111111111111 refs/heads/main 2222222222222222222222222222222222222222\n";
const FEATURE: &str = "refs/heads/feature 1111111111111111111111111111111111111111 refs/heads/feature 0000000000000000000000000000000000000000\n";

#[test]
fn a_push_to_a_frozen_main_is_refused_and_main_unfrozen_or_a_branch_is_silent() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let free = check(&setup, "bob", &api, MAIN);
    assert_eq!(status(&free), 0);
    assert_eq!(text(&free.stdout), "");
    assert_eq!(text(&free.stderr), "", "silent with no freezing slot held");
    take(&setup, "alice", &api);
    let refused = check(&setup, "bob", &api, MAIN);
    assert_eq!(status(&refused), 1);
    let said = text(&refused.stderr);
    for part in [
        "migrate",
        "alice",
        "invoices to bigint",
        "--no-verify",
        "(refusal r-",
    ] {
        assert!(said.contains(part), "{part}: {said}");
    }
    let branch = check(&setup, "bob", &api, FEATURE);
    assert_eq!(status(&branch), 0);
    assert_eq!(text(&branch.stderr), "");
    let own = check(&setup, "alice", &api, MAIN);
    assert_eq!(status(&own), 0, "my own hold never freezes me");
}

#[test]
fn an_unreachable_server_lets_the_push_go_with_one_warning() {
    let setup = setup();
    let away = setup.root.path().join("away");
    make_checkout(
        &away,
        &unreachable().unwrap(),
        "acme",
        Some("api"),
        API_REMOTE,
    );
    let out = setup
        .dibs_as("bob", &away)
        .env("DIBS_TOKEN", &setup.bob.token)
        .args(["check", "--push"])
        .write_stdin(MAIN.to_owned())
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    let warning = text(&out.stderr);
    assert_eq!(warning.lines().count(), 1, "{warning}");
    assert!(warning.contains("did not check this push"), "{warning}");
}

/// A bare repository beside the checkout standing for `api`: its remote `shared`, fetched from
/// `api`'s URL and pushed to the bare repository, and a `pre-push` hook in the checkout that runs
/// `dibs check --push` as `handle` with the hook's arguments.
fn shared_remote(setup: &Setup, checkout: &Path, handle: &str) -> PathBuf {
    let bare = setup.root.path().join(format!("{handle}-shared.git"));
    let made = std::process::Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&bare)
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", text(&made.stderr));
    git(
        checkout,
        &["remote", "add", "shared", &bare.to_string_lossy()],
    );
    git(checkout, &["config", "remote.shared.url", API_REMOTE]);
    git(
        checkout,
        &["config", "remote.shared.pushurl", &bare.to_string_lossy()],
    );
    let home = setup.home_of(handle);
    let hook = checkout.join(".git/hooks/pre-push");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nDIBS_CONFIG_DIR='{}' DIBS_STATE_DIR='{}' exec '{}' check --push --remote \"$1\" --url \"$2\"\n",
            home.join("config").display(),
            home.join("state").display(),
            env!("CARGO_BIN_EXE_dibs")
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bare
}

fn push(checkout: &Path, args: &[&str]) -> Output {
    std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .arg("push")
        .args(args)
        .env_remove("DIBS_TOKEN")
        .output()
        .unwrap()
}

#[test]
fn git_refuses_a_push_to_main_through_its_pre_push_hook_whatever_the_command_line_said() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    shared_remote(&setup, &api, "bob");
    let first = push(&api, &["-u", "shared", "main"]);
    assert!(first.status.success(), "{}", text(&first.stderr));
    take(&setup, "alice", &api);
    fs::write(api.join("CHANGELOG.md"), "more\n").unwrap();
    git(&api, &["add", "-A"]);
    git(&api, &["commit", "-q", "-m", "more"]);
    let refused = push(&api, &[]);
    assert!(
        !refused.status.success(),
        "a push with no refspec goes to main"
    );
    let said = text(&refused.stderr);
    assert!(said.contains("alice"), "{said}");
    assert!(said.contains("invoices to bigint"), "{said}");
    git(&api, &["checkout", "-q", "-b", "feature"]);
    let branch = push(&api, &["shared", "feature"]);
    assert!(branch.status.success(), "{}", text(&branch.stderr));
    let past = push(&api, &["--no-verify", "shared", "feature:main"]);
    assert!(
        past.status.success(),
        "--no-verify is the conscious way past it"
    );
}

/// The setup with `api`'s `main` protected for alice alone.
fn protected() -> Setup {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["repos"][0]["protected_refs"] =
        json!([{"ref": "refs/heads/main", "pushers": ["alice"]}]);
    setup.server.settings(&setup.alice, settings).unwrap();
    setup
}

#[test]
fn a_push_to_a_protected_main_is_refused_for_a_member_who_is_no_pusher_and_lets_a_pusher_through() {
    let setup = protected();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let refused = check(&setup, "bob", &api, MAIN);
    assert_eq!(status(&refused), 1);
    let said = text(&refused.stderr);
    for part in [
        "api:refs/heads/main",
        "only alice may push to",
        "open a pull request",
        "--no-verify",
        "breaks the team's rule",
        "(refusal r-",
    ] {
        assert!(said.contains(part), "{part}: {said}");
    }
    let branch = check(&setup, "bob", &api, FEATURE);
    assert_eq!(status(&branch), 0);
    assert_eq!(text(&branch.stderr), "");
    let pusher = check(&setup, "alice", &api, MAIN);
    assert_eq!(status(&pusher), 0, "{}", text(&pusher.stderr));
    assert_eq!(text(&pusher.stderr), "");
}

#[test]
fn git_refuses_a_members_push_to_a_protected_main_through_its_pre_push_hook() {
    let setup = protected();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    shared_remote(&setup, &api, "bob");
    let refused = push(&api, &["shared", "main"]);
    assert!(!refused.status.success(), "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("only alice may push to"),
        "{}",
        text(&refused.stderr)
    );
    git(&api, &["checkout", "-q", "-b", "bob/7-rounding"]);
    let branch = push(&api, &["shared", "bob/7-rounding"]);
    assert!(branch.status.success(), "{}", text(&branch.stderr));
}

#[test]
fn switched_off_the_push_check_lets_every_push_go_without_a_word_or_a_call() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    take(&setup, "alice", &api);
    let before = setup.server.requests();
    let out = setup
        .dibs_as("bob", &api)
        .env("DIBS_OFF", "1")
        .args(["check", "--push"])
        .write_stdin(MAIN.to_owned())
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    assert_eq!(text(&out.stdout), "");
    assert_eq!(text(&out.stderr), "");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        setup.server.requests(),
        before,
        "no call, and no sync started"
    );
}

#[test]
fn a_refused_push_is_counted_by_its_kind_and_the_next_command_reports_it() {
    let setup = protected();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let refused = check(&setup, "bob", &api, MAIN);
    assert_eq!(status(&refused), 1);
    let sync = setup
        .dibs_as("bob", &api)
        .args(["sync", "--report"])
        .output()
        .unwrap();
    assert_eq!(status(&sync), 0, "{}", text(&sync.stderr));
    let beats = || -> String {
        setup
            .server
            .request_log()
            .into_iter()
            .filter(|request| request.starts_with("POST /v1/heartbeat"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !beats().contains(r#""counts":{"push_needs_permission":1}"#)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let reported = beats();
    assert!(
        reported.contains(r#""counts":{"push_needs_permission":1}"#),
        "{reported}"
    );
    assert!(!reported.contains("refs/heads/main"), "{reported}");
}

/// Another remote of the checkout, `name`, fetched from `url` and pushed to a bare repository of
/// its own.
fn other_remote(setup: &Setup, checkout: &Path, name: &str, url: &str) {
    let bare = setup.root.path().join(format!("{name}.git"));
    let made = std::process::Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&bare)
        .output()
        .unwrap();
    assert!(made.status.success(), "{}", text(&made.stderr));
    git(checkout, &["remote", "add", name, url]);
    git(
        checkout,
        &[
            "config",
            &format!("remote.{name}.pushurl"),
            &bare.to_string_lossy(),
        ],
    );
}

#[test]
fn a_push_is_judged_by_the_repository_of_the_remote_it_goes_to() {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["repos"][0]["protected_refs"] =
        json!([{"ref": "refs/heads/main", "pushers": ["alice"]}]);
    settings["repos"][1]["protected_refs"] =
        json!([{"ref": "refs/heads/main", "pushers": ["bob"]}]);
    setup.server.settings(&setup.alice, settings).unwrap();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    shared_remote(&setup, &api, "bob");
    other_remote(&setup, &api, "fork", "https://github.com/bob/api.git");
    other_remote(&setup, &api, "web", crate::support::WEB_REMOTE);
    let fork = push(&api, &["fork", "main"]);
    assert!(
        fork.status.success(),
        "a fork protects nothing: {}",
        text(&fork.stderr)
    );
    let web = push(&api, &["web", "main"]);
    assert!(
        web.status.success(),
        "bob may push web's main: {}",
        text(&web.stderr)
    );
    let refused = push(&api, &["shared", "main"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused.stderr).contains("api:refs/heads/main, which only alice may push to"),
        "{}",
        text(&refused.stderr)
    );
}

#[test]
fn a_protected_main_is_refused_from_what_the_machine_remembers_while_the_server_is_down() {
    let setup = protected();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let synced = setup
        .dibs_as("bob", &api)
        .args(["sync", "--force", "--quiet"])
        .output()
        .unwrap();
    assert_eq!(status(&synced), 0, "{}", text(&synced.stderr));
    setup.server.stop();
    let refused = check(&setup, "bob", &api, MAIN);
    assert_eq!(status(&refused), 1, "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("only alice may push to"),
        "{}",
        text(&refused.stderr)
    );
    let branch = check(&setup, "bob", &api, FEATURE);
    assert_eq!(status(&branch), 0);
    assert!(
        text(&branch.stderr).contains("did not check this push"),
        "a freeze cannot be judged without the server: {}",
        text(&branch.stderr)
    );
}

/// The setup with `api`'s release tags `v*` protected for alice alone, and no branch protected.
fn protected_tags() -> Setup {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["repos"][0]["protected_refs"] = json!([{"ref": "refs/tags/v*", "pushers": ["alice"]}]);
    setup.server.settings(&setup.alice, settings).unwrap();
    setup
}

const TAG: &str = "refs/tags/v2.4.0 1111111111111111111111111111111111111111 refs/tags/v2.4.0 0000000000000000000000000000000000000000\n";
const TAG_DELETED: &str = "(delete) 0000000000000000000000000000000000000000 refs/tags/v2.3.0 3333333333333333333333333333333333333333\n";
const NIGHTLY: &str = "refs/tags/nightly 1111111111111111111111111111111111111111 refs/tags/nightly 0000000000000000000000000000000000000000\n";

#[test]
fn a_protected_tag_pushed_or_deleted_by_a_member_who_is_no_pusher_is_refused() {
    let setup = protected_tags();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    for (what, line) in [("a pushed tag", TAG), ("a deleted tag", TAG_DELETED)] {
        let refused = check(&setup, "bob", &api, line);
        assert_eq!(status(&refused), 1, "{what}");
        let said = text(&refused.stderr);
        for part in [
            "the tags of api that v* covers",
            "only alice may push to",
            "Do not push or delete tags that v* covers",
            "(refusal r-",
        ] {
            assert!(said.contains(part), "{what}, {part}: {said}");
        }
        assert!(!said.contains("open a pull request"), "{what}: {said}");
        assert!(!said.contains("--no-verify"), "{what}: {said}");
    }
    let other = check(&setup, "bob", &api, NIGHTLY);
    assert_eq!(status(&other), 0, "a tag no pattern covers is free");
    assert_eq!(text(&other.stderr), "");
    let main = check(&setup, "bob", &api, MAIN);
    assert_eq!(status(&main), 0, "protected tags protect no branch");
    assert_eq!(text(&main.stderr), "");
    for line in [TAG, TAG_DELETED] {
        let pusher = check(&setup, "alice", &api, line);
        assert_eq!(status(&pusher), 0, "{}", text(&pusher.stderr));
        assert_eq!(text(&pusher.stderr), "");
    }
}

#[test]
fn git_refuses_a_members_push_of_a_protected_tag_through_its_pre_push_hook() {
    let setup = protected_tags();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    shared_remote(&setup, &api, "bob");
    git(&api, &["config", "tag.gpgsign", "false"]);
    git(&api, &["tag", "v2.4.0"]);
    git(&api, &["tag", "nightly"]);
    let refused = push(&api, &["shared", "v2.4.0"]);
    assert!(!refused.status.success(), "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr).contains("only alice may push to"),
        "{}",
        text(&refused.stderr)
    );
    let refused = push(&api, &["shared", "--tags"]);
    assert!(
        !refused.status.success(),
        "every tag pushed includes the protected one"
    );
    let free = push(&api, &["shared", "nightly"]);
    assert!(free.status.success(), "{}", text(&free.stderr));
    let branch = push(&api, &["shared", "main"]);
    assert!(branch.status.success(), "{}", text(&branch.stderr));
}

/// The setup with `api`'s `main` protected for alice alone and its `release/` branches for alice
/// and carol, and a slot that freezes every release branch.
fn release_train() -> Setup {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["repos"][0]["protected_refs"] = json!([
        {"ref": "refs/heads/main", "pushers": ["alice"]},
        {"ref": "refs/heads/release/*", "pushers": ["alice", "carol"]}
    ]);
    settings["slots"] = json!([
        {"name": "release:cut", "max_hold": "1h", "freezes": ["api:refs/heads/release/*"]}
    ]);
    setup.server.settings(&setup.alice, settings).unwrap();
    setup
}

const RELEASE: &str = "refs/heads/release/2026.10.3 1111111111111111111111111111111111111111 refs/heads/release/2026.10.3 0000000000000000000000000000000000000000\n";
const RELEASE_DELETED: &str = "(delete) 0000000000000000000000000000000000000000 refs/heads/release/2026.10.1 3333333333333333333333333333333333333333\n";
const TASK: &str = "refs/heads/bob/42-rounding 1111111111111111111111111111111111111111 refs/heads/bob/42-rounding 0000000000000000000000000000000000000000\n";

#[test]
fn a_release_branch_a_pattern_protects_is_refused_for_a_member_who_is_no_pusher_naming_the_pattern()
{
    let setup = release_train();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    for (what, line, branch) in [
        ("a pushed release branch", RELEASE, "release/2026.10.3"),
        (
            "a deleted release branch",
            RELEASE_DELETED,
            "release/2026.10.1",
        ),
    ] {
        let refused = check(&setup, "bob", &api, line);
        assert_eq!(status(&refused), 1, "{what}");
        let said = text(&refused.stderr);
        let named = format!(
            "This push goes to the branch {branch} of api, which release/* covers and only alice, carol may push to."
        );
        for part in [
            named.as_str(),
            "open a pull request",
            "--no-verify",
            "(refusal r-",
        ] {
            assert!(said.contains(part), "{what}, {part}: {said}");
        }
    }
    let task = check(&setup, "bob", &api, TASK);
    assert_eq!(status(&task), 0, "a task branch is free");
    assert_eq!(text(&task.stderr), "");
    let main = check(&setup, "bob", &api, MAIN);
    assert_eq!(status(&main), 1, "main stays protected beside the pattern");
    for line in [RELEASE, RELEASE_DELETED, MAIN] {
        let pusher = check(&setup, "alice", &api, line);
        assert_eq!(status(&pusher), 0, "{}", text(&pusher.stderr));
        assert_eq!(text(&pusher.stderr), "");
    }
}

#[test]
fn a_push_to_a_release_branch_is_refused_while_another_member_holds_the_slot_that_freezes_them() {
    let setup = release_train();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let out = setup
        .dibs_as("alice", &api)
        .args(["slot", "take", "release:cut", "--purpose", "cut 2026.10.3"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    let mut settings = two_repositories();
    settings["slots"] = json!([
        {"name": "release:cut", "max_hold": "1h", "freezes": ["api:refs/heads/release/*"]}
    ]);
    setup.server.settings(&setup.alice, settings).unwrap();
    let frozen = check(&setup, "bob", &api, RELEASE);
    assert_eq!(status(&frozen), 1);
    let said = text(&frozen.stderr);
    for part in [
        "api:refs/heads/release/2026.10.3",
        "with every branch release/* covers",
        "release:cut",
        "alice",
        "cut 2026.10.3",
        "--no-verify",
    ] {
        assert!(said.contains(part), "{part}: {said}");
    }
    let task = check(&setup, "bob", &api, TASK);
    assert_eq!(status(&task), 0);
    assert_eq!(text(&task.stderr), "");
    let own = check(&setup, "alice", &api, RELEASE);
    assert_eq!(status(&own), 0, "my own hold never freezes me");
}

#[test]
fn git_refuses_a_members_push_to_a_release_branch_through_its_pre_push_hook_and_lets_a_pusher_through()
 {
    let setup = release_train();
    let bobs = setup.checkout("api", Some("api"), API_REMOTE);
    shared_remote(&setup, &bobs, "bob");
    git(&bobs, &["checkout", "-q", "-b", "release/2026.10.3"]);
    let refused = push(&bobs, &["shared", "release/2026.10.3"]);
    assert!(!refused.status.success(), "{}", text(&refused.stderr));
    assert!(
        text(&refused.stderr)
            .contains("the branch release/2026.10.3 of api, which release/* covers"),
        "{}",
        text(&refused.stderr)
    );
    git(&bobs, &["checkout", "-q", "-b", "bob/42-rounding"]);
    let task = push(&bobs, &["shared", "bob/42-rounding"]);
    assert!(task.status.success(), "{}", text(&task.stderr));
    let bare = push(&bobs, &["shared", "HEAD:refs/heads/release/2026.10.3"]);
    assert!(!bare.status.success(), "whatever the command line names");

    let alices = setup.checkout("api-alice", Some("api"), API_REMOTE);
    shared_remote(&setup, &alices, "alice");
    git(&alices, &["checkout", "-q", "-b", "release/2026.10.3"]);
    let pushed = push(&alices, &["shared", "release/2026.10.3"]);
    assert!(pushed.status.success(), "{}", text(&pushed.stderr));
    let deleted = push(&alices, &["shared", "--delete", "release/2026.10.3"]);
    assert!(deleted.status.success(), "{}", text(&deleted.stderr));
}
