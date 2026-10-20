//! The command check of `dibs hook neutral before-tool` on a shell command: a slot another member
//! holds denies naming them, the member's own hold lets it through, a line no slot governs is
//! answered without the server, a server that cannot be reached lets it through with a notice, and
//! the command line itself never reaches the server.

use std::path::Path;
use std::process::Output;

use serde_json::{Value, json};

use dibs_test_server::{two_repositories, unreachable};

use crate::support::{API_REMOTE, Setup, json, make_checkout, status};

/// The setup with `deploy:prod`, freezing `api`'s `main` and needed by `./deploy.sh prod`.
fn setup() -> Setup {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["slots"] = json!([
        {"name": "deploy:prod", "max_hold": "30m", "freezes": ["api:refs/heads/main"],
         "commands": [{"matches": "^deploy\\.sh prod"}]}
    ]);
    setup.server.settings(&setup.alice, settings).unwrap();
    setup
}

fn shell(cwd: &Path, command: &str) -> String {
    json!({
        "protocol": 1,
        "session": "sess-1234-abcd",
        "cwd": cwd.to_string_lossy(),
        "thread": "main",
        "tool": {"kind": "shell", "command": command},
    })
    .to_string()
}

fn before(setup: &Setup, handle: &str, cwd: &Path, command: &str) -> Value {
    let out: Output = setup
        .dibs_as(handle, cwd)
        .args(["hook", "neutral", "before-tool"])
        .write_stdin(shell(cwd, command))
        .output()
        .unwrap();
    assert_eq!(status(&out), 0, "a hook always exits 0");
    json(&out)
}

fn take(setup: &Setup, handle: &str, cwd: &Path) {
    let out = setup
        .dibs_as(handle, cwd)
        .args([
            "slot",
            "take",
            "deploy:prod",
            "--purpose",
            "migrating invoices",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
}

#[test]
fn a_command_another_member_holds_the_slot_of_is_denied_naming_them_and_the_line_stays_here() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    take(&setup, "alice", &api);
    let before_count = setup.server.requests();
    let answer = before(
        &setup,
        "bob",
        &api,
        "cd ops && PGPASSWORD=hunter2-secret ./deploy.sh prod",
    );
    assert_eq!(answer["decision"], "deny", "{answer}");
    let reason = answer["reason"].as_str().unwrap();
    for part in [
        "needs the slot deploy:prod",
        "alice holds it",
        "migrating invoices",
        "dibs_slot_take deploy:prod",
    ] {
        assert!(reason.contains(part), "{part}: {reason}");
    }
    let log = setup.server.request_log();
    assert!(
        log[before_count..]
            .iter()
            .any(|request| request.starts_with("GET /v1/slots/deploy%3Aprod")),
        "{log:?}"
    );
    for request in &log {
        assert!(!request.contains("hunter2"), "{request}");
        assert!(!request.contains("deploy.sh"), "{request}");
    }
}

#[test]
fn my_own_hold_lets_the_command_through_and_a_free_slot_is_still_needed() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let free = before(&setup, "bob", &api, "./deploy.sh prod");
    assert_eq!(free["decision"], "deny", "{free}");
    assert!(
        free["reason"]
            .as_str()
            .unwrap()
            .contains("Nobody holds it now"),
        "{free}"
    );
    take(&setup, "bob", &api);
    assert_eq!(
        before(&setup, "bob", &api, "./deploy.sh prod"),
        json!({"protocol": 1, "decision": "no_objection"})
    );
}

#[test]
fn a_line_no_slot_governs_asks_the_server_nothing() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    take(&setup, "alice", &api);
    before(&setup, "bob", &api, "ls");
    let warm = setup.server.requests();
    for line in [
        "cargo test",
        "git push origin feature",
        "echo './deploy.sh prod'",
    ] {
        assert_eq!(
            before(&setup, "bob", &api, line)["decision"],
            "no_objection",
            "{line}"
        );
    }
    let asked: Vec<String> = setup.server.request_paths()[warm..]
        .iter()
        .filter(|path| path.starts_with("/v1/slots"))
        .cloned()
        .collect();
    assert!(asked.is_empty(), "{asked:?}");
}

#[test]
fn a_push_to_a_frozen_ref_is_denied_by_the_command_check() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    take(&setup, "alice", &api);
    let answer = before(&setup, "bob", &api, "git push origin main");
    assert_eq!(answer["decision"], "deny", "{answer}");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("freezes while alice holds it"),
        "{answer}"
    );
}

#[test]
fn an_unreachable_server_lets_the_command_through_with_a_notice() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    before(&setup, "bob", &api, "ls");
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
        .args(["hook", "neutral", "before-tool"])
        .write_stdin(shell(&away, "./deploy.sh prod"))
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    let answer = json(&out);
    assert_eq!(answer["decision"], "no_objection", "{answer}");
    assert!(answer["notice"].as_str().is_some(), "{answer}");
}

#[test]
fn a_push_to_a_protected_branch_is_denied_without_a_call_and_its_pusher_passes() {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["repos"][0]["protected_refs"] =
        json!([{"ref": "refs/heads/main", "pushers": ["alice"]}]);
    setup.server.settings(&setup.alice, settings).unwrap();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let warm = before(&setup, "bob", &api, "git status");
    assert_eq!(warm["decision"], "no_objection", "{warm}");
    let before_count = setup.server.requests();
    let answer = before(&setup, "bob", &api, "git push origin main");
    assert_eq!(answer["decision"], "deny", "{answer}");
    let reason = answer["reason"].as_str().unwrap();
    assert!(reason.contains("only alice may push to"), "{reason}");
    assert!(reason.contains("dibs_review"), "{reason}");
    let asked: Vec<String> = setup.server.request_paths()[before_count..]
        .iter()
        .filter(|path| path.starts_with("/v1/slots") || path.starts_with("/v1/edit-check"))
        .cloned()
        .collect();
    assert!(
        asked.is_empty(),
        "the protected branch is judged from the cached settings: {asked:?}"
    );
    let branch = before(&setup, "bob", &api, "git push origin bob/7-rounding");
    assert_eq!(branch["decision"], "no_objection", "{branch}");
    for line in ["git push", "git push origin HEAD", "git push --all origin"] {
        let answer = before(&setup, "bob", &api, line);
        assert_eq!(answer["decision"], "deny", "{line}: {answer}");
    }
    crate::support::git(
        &api,
        &["remote", "add", "fork", "https://github.com/bob/api.git"],
    );
    let fork = before(&setup, "bob", &api, "git push fork main");
    assert_eq!(
        fork["decision"], "no_objection",
        "a fork protects nothing: {fork}"
    );
    let pusher = before(&setup, "alice", &api, "git push origin main");
    assert_eq!(pusher["decision"], "no_objection", "{pusher}");
}
