//! What a member's machine counts and reports: refusals by kind with the id they are shown with,
//! sessions run with the guard switched off, never a path or a command line, sent with a heartbeat
//! of the background sync, a later command of the command line included, and `dibs feedback`.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use dibs_test_server::two_repositories;

use crate::support::{API_REMOTE, Setup, json, status, text};

fn event(cwd: &Path, tool: Option<Value>) -> String {
    let mut value = json!({
        "protocol": 1,
        "session": "sess-1234-abcd",
        "cwd": cwd.to_string_lossy(),
        "thread": "main",
    });
    if let Some(tool) = tool {
        value["tool"] = tool;
    }
    value.to_string()
}

fn hook(setup: &Setup, handle: &str, cwd: &Path, name: &str, stdin: String, off: bool) -> Value {
    let mut command = setup.dibs_as(handle, cwd);
    if off {
        command.env("DIBS_OFF", "1");
    }
    let out = command
        .args(["hook", "neutral", name])
        .write_stdin(stdin)
        .output()
        .unwrap();
    assert_eq!(status(&out), 0, "a hook always exits 0");
    json(&out)
}

/// The bodies of the heartbeats the server received, joined.
fn heartbeats(setup: &Setup) -> String {
    setup
        .server
        .request_log()
        .into_iter()
        .filter(|request| request.starts_with("POST /v1/heartbeat"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Waits up to ten seconds for a heartbeat that says `part`.
fn reported(setup: &Setup, part: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !heartbeats(setup).contains(part) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    heartbeats(setup)
}

#[test]
fn switched_off_hooks_ask_nothing_and_a_later_command_reports_the_session_once() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let before = setup.server.requests();
    for (name, stdin) in [
        ("session-started", event(&api, None)),
        (
            "before-tool",
            event(
                &api,
                Some(json!({"kind": "edit", "paths": ["src/billing/a.rs"]})),
            ),
        ),
        ("after-tool", event(&api, None)),
    ] {
        let answer = hook(&setup, "bob", &api, name, stdin, true);
        assert_eq!(answer, json!({"protocol": 1, "decision": "no_objection"}));
    }
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        setup.server.requests(),
        before,
        "no verdict, heartbeat, inbox or sync"
    );

    let out = setup
        .dibs_as("bob", &api)
        .env("DIBS_OFF", "1")
        .args(["status"])
        .output()
        .unwrap();
    assert_eq!(
        status(&out),
        0,
        "the switch silences the hooks, not the command line"
    );
    let sent = reported(&setup, r#""switched_off":1"#);
    assert!(sent.contains(r#""switched_off":1"#), "{sent}");
    assert!(
        !sent.contains("sess-1234"),
        "the session stays on the machine: {sent}"
    );
}

#[test]
fn the_report_holds_kinds_and_counts_never_a_path_or_a_command_line() {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["slots"] = json!([
        {"name": "deploy:prod", "max_hold": "30m",
         "commands": [{"matches": "^deploy\\.sh prod"}]}
    ]);
    setup.server.settings(&setup.alice, settings).unwrap();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    for args in [
        vec!["slot", "take", "deploy:prod", "--purpose", "release"],
        vec!["new", "gh:acme/api#1", "--title", "Billing work"],
        vec!["claim", "gh:acme/api#1", "--area", "billing"],
    ] {
        let out = setup.dibs_as("alice", &api).args(&args).output().unwrap();
        assert_eq!(status(&out), 0, "{args:?}: {}", text(&out.stderr));
    }

    let shell = hook(
        &setup,
        "bob",
        &api,
        "before-tool",
        event(
            &api,
            Some(json!({"kind": "shell", "command": "PGPASSWORD=hunter2-secret ./deploy.sh prod"})),
        ),
        false,
    );
    assert_eq!(shell["decision"], "deny", "{shell}");
    let reason = shell["reason"].as_str().unwrap().to_owned();
    let id = reason
        .split_whitespace()
        .find(|word| word.starts_with("r-"))
        .map(|word| word.trim_end_matches(';').to_owned())
        .unwrap();
    assert!(
        reason.contains(&format!("dibs feedback false-refusal {id}")),
        "{reason}"
    );
    let edit = hook(
        &setup,
        "bob",
        &api,
        "before-tool",
        event(
            &api,
            Some(json!({"kind": "edit", "paths": ["src/billing/secret-plan.rs"]})),
        ),
        false,
    );
    assert_eq!(edit["decision"], "deny", "{edit}");

    let sync = setup
        .dibs_as("bob", &api)
        .args(["sync", "--report"])
        .output()
        .unwrap();
    assert_eq!(status(&sync), 0, "{}", text(&sync.stderr));
    let sent = reported(&setup, r#""held_by":1"#);
    assert!(sent.contains(r#""held_by":1"#), "{sent}");
    assert!(sent.contains(r#""needs_slot":1"#), "{sent}");
    for secret in [
        "hunter2",
        "deploy.sh",
        "secret-plan",
        "src/billing",
        "acme/api#1",
        &id,
    ] {
        assert!(!sent.contains(secret), "{secret} left the machine: {sent}");
    }

    let feedback = setup
        .dibs_as("bob", &api)
        .args([
            "--json",
            "feedback",
            "false-refusal",
            &id,
            "--reason",
            "the release was over",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&feedback), 0, "{}", text(&feedback.stderr));
    assert_eq!(
        json(&feedback)["feedback"],
        json!({"kind": "false_refusal", "refusal": id, "tally": "needs_slot"})
    );
    let conflict = setup
        .dibs_as("bob", &api)
        .args([
            "feedback",
            "conflict",
            "src/billing/rates.rs",
            "--reason",
            "rebased over alice's rounding",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&conflict), 0, "{}", text(&conflict.stderr));
    assert_eq!(
        text(&conflict.stdout).trim(),
        "Recorded for the project's admins: a conflict resolved by hand in api:billing."
    );
}

/// The file `name` of the person's project state, once it exists.
fn state_file(setup: &Setup, handle: &str, name: &str) -> std::path::PathBuf {
    let mut pending = vec![setup.home_of(handle).join("state")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|found| found == name) {
                return path;
            }
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    panic!("no {name} under {handle}'s state");
}

fn report_file(setup: &Setup, handle: &str) -> std::path::PathBuf {
    state_file(setup, handle, "report.json")
}

/// A switched-off session's start, as `handle`, in its own process.
fn switched_off_start(
    setup: &Setup,
    handle: &str,
    cwd: &Path,
    session: &str,
) -> std::process::Child {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut child = setup
        .process_as(handle, cwd)
        .env("DIBS_OFF", "1")
        .args(["hook", "neutral", "session-started"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let input = json!({"protocol": 1, "session": session, "cwd": cwd, "thread": "main"});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    child
}

#[test]
fn hooks_running_at_once_wait_on_no_lock_and_lose_no_count() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    assert!(
        switched_off_start(&setup, "bob", &api, "sess-first")
            .wait()
            .unwrap()
            .success()
    );
    let spool = state_file(&setup, "bob", "report.spool");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(spool.with_file_name("report.lock"))
        .unwrap();
    lock.lock().unwrap();

    let started = Instant::now();
    let hooks: Vec<_> = (0..8)
        .map(|n| switched_off_start(&setup, "bob", &api, &format!("sess-{n}")))
        .collect();
    for mut hook in hooks {
        assert!(hook.wait().unwrap().success());
    }
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "eight hooks ran beside a held lock in {:?}",
        started.elapsed()
    );
    drop(lock);

    let sync = setup
        .dibs_as("bob", &api)
        .args(["sync", "--report", "--force"])
        .output()
        .unwrap();
    assert_eq!(status(&sync), 0, "{}", text(&sync.stderr));
    let file: Value = serde_json::from_slice(&std::fs::read(report_file(&setup, "bob")).unwrap())
        .expect("one whole file");
    let counted: u64 = file["days"]
        .as_object()
        .unwrap()
        .values()
        .map(|day| day["counts"]["switched_off"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(counted, 9, "{file}");
    let left = std::fs::read_dir(&spool)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .count();
    assert_eq!(left, 0, "the fold deleted what it counted");
    assert!(
        !setup
            .home_of("bob")
            .join("state/hooks/timings.json")
            .exists(),
        "a run switched off is left out of the hook timings"
    );
}

#[test]
fn a_day_the_server_does_not_take_stays_to_be_sent_and_the_session_stays_alive() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    for args in [
        vec!["new", "gh:acme/api#3", "--title", "Search work"],
        vec!["claim", "gh:acme/api#3"],
    ] {
        let out = setup.dibs_as("bob", &api).args(&args).output().unwrap();
        assert_eq!(status(&out), 0, "{args:?}: {}", text(&out.stderr));
    }
    hook(
        &setup,
        "bob",
        &api,
        "session-started",
        event(&api, None),
        true,
    );
    let fold = setup
        .dibs_as("bob", &api)
        .args(["sync", "--report"])
        .output()
        .unwrap();
    assert_eq!(status(&fold), 0, "{}", text(&fold.stderr));
    let path = report_file(&setup, "bob");
    let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    file["days"]["2099-01-01"] = json!({"counts": {"held_by": 1, "kind_of_tomorrow": 2}});
    std::fs::write(&path, file.to_string()).unwrap();
    let sync = setup
        .dibs_as("bob", &api)
        .args(["sync", "--report", "--force"])
        .output()
        .unwrap();
    assert_eq!(
        status(&sync),
        0,
        "the heartbeat carrying it was answered: {}",
        text(&sync.stderr)
    );
    let sent = reported(&setup, "2099-01-01");
    assert!(sent.contains(r#""held_by":1"#), "{sent}");
    assert!(
        !sent.contains("kind_of_tomorrow"),
        "a kind it does not know is kept, not sent"
    );
    let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        after["days"]["2099-01-01"].get("sent").is_none(),
        "a day the server did not take is not marked sent: {after}"
    );
    assert_eq!(after["days"]["2099-01-01"]["counts"]["kind_of_tomorrow"], 2);
    let seen_after = setup
        .dibs_as("bob", &api)
        .args(["--json", "status"])
        .output()
        .unwrap();
    assert_eq!(
        json(&seen_after)["live"].as_array().unwrap().len(),
        1,
        "the claim is live"
    );
}
