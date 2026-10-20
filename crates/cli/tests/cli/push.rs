//! `dibs check --push`, fed git's pre-push lines and run by git itself from a real `pre-push` hook
//! on a push to a bare repository: a push to a ref a slot another member holds freezes is refused
//! naming them, anything else is silent, and a server that cannot be reached lets the push go with
//! one warning.

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
    for part in ["migrate", "alice", "invoices to bigint", "--no-verify"] {
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

/// A bare repository beside the checkout, its remote `shared`, and a `pre-push` hook in the
/// checkout that runs `dibs check --push` as `handle`.
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
    let home = setup.home_of(handle);
    let hook = checkout.join(".git/hooks/pre-push");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nDIBS_CONFIG_DIR='{}' DIBS_STATE_DIR='{}' exec '{}' check --push\n",
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
