//! `dibs hook claude-code` on what reaches an agent while it works: a `Bash` command checked
//! against the slots, and a teammate's pressing message or a slot offered to the member, claimed
//! once per member by the background sync and shown between tool calls or with a prompt, on the
//! main thread only and once.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use dibs_test_server::two_repositories;

use crate::support::{SESSION, Setup, hook_input, reply, text};

impl Setup {
    /// The settings with `deploy:prod`, needed by `./deploy.sh prod`.
    fn with_slot(&self) {
        let mut settings = two_repositories();
        settings["slots"] = json!([
            {"name": "deploy:prod", "max_hold": "30m", "offer_for": "5m",
             "freezes": ["api:refs/heads/main"],
             "commands": [{"matches": "^deploy\\.sh prod"}]}
        ]);
        self.server.settings(&self.alice, settings).unwrap();
    }

    fn run_hook(&self, handle: &str, cwd: &Path, event: &str, stdin: &str) -> Output {
        self.dibs(handle, cwd)
            .args(["hook", "claude-code", event])
            .write_stdin(stdin.to_owned())
            .output()
            .unwrap()
    }

    /// The `dibs sync` a hook of `session` would start, run in the foreground.
    fn sync_for(&self, handle: &str, cwd: &Path, session: &str) {
        let out = self
            .dibs(handle, cwd)
            .args([
                "sync",
                "--force",
                "--host",
                "claude-code",
                "--session",
                session,
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }

    fn send_urgent(&self, from: &str, to: &str, cwd: &Path, body: &str) {
        let out = self
            .dibs(from, cwd)
            .args([
                "send",
                "--to",
                to,
                "--subject",
                "Staging is down",
                "--importance",
                "urgent",
                "--ack",
                body,
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
}

fn bash(cwd: &Path, command: &str) -> String {
    hook_input(
        "PreToolUse",
        cwd,
        json!({
            "tool_name": "Bash",
            "tool_input": {"command": command, "description": "Run it", "timeout": 120000, "run_in_background": false},
            "tool_use_id": "toolu_01ABC123"
        }),
    )
}

fn after(cwd: &Path, agent: Option<&str>) -> String {
    let mut extra = json!({
        "tool_name": "Read",
        "tool_input": {"file_path": cwd.join("README.md").to_string_lossy()},
        "tool_response": {"type": "text"},
        "tool_use_id": "toolu_01ABC124"
    });
    if let Some(agent) = agent {
        extra["agent_id"] = json!(agent);
        extra["agent_type"] = json!("Explore");
    }
    hook_input("PostToolUse", cwd, extra)
}

fn after_for(cwd: &Path, session: &str) -> String {
    let mut value: Value = serde_json::from_str(&after(cwd, None)).unwrap();
    value["session_id"] = json!(session);
    value.to_string()
}

fn context(value: &Value) -> Option<String> {
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .map(str::to_owned)
}

#[test]
fn a_bash_command_that_needs_a_slot_another_member_holds_is_denied_naming_them() {
    let setup = Setup::new();
    setup.with_slot();
    let api = setup.api();
    setup.ok(
        "alice",
        &api,
        &["slot", "take", "deploy:prod", "--purpose", "ship 2.4"],
    );
    let answer = reply(&setup.run_hook(
        "bob",
        &api,
        "pre-tool-use",
        &bash(&api, "cd ops && ./deploy.sh prod"),
    ));
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(
        answer["hookSpecificOutput"]["permissionDecision"], "deny",
        "{answer}"
    );
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains("alice holds it"), "{reason}");
    assert!(reason.contains("ship 2.4"), "{reason}");
    setup.ok("alice", &api, &["slot", "release", "deploy:prod"]);
    setup.ok(
        "bob",
        &api,
        &["slot", "take", "deploy:prod", "--purpose", "hotfix"],
    );
    assert_eq!(
        reply(&setup.run_hook("bob", &api, "pre-tool-use", &bash(&api, "./deploy.sh prod"))),
        Value::Null,
        "with the slot: no decision at all"
    );
    let asked = setup.server.requests();
    assert_eq!(
        reply(&setup.run_hook("bob", &api, "pre-tool-use", &bash(&api, "cargo test"))),
        Value::Null
    );
    assert!(
        setup.server.request_paths()[asked..]
            .iter()
            .all(|path| !path.starts_with("/v1/slots")),
        "an unmatched command makes no slot request"
    );
}

#[test]
fn a_bash_command_with_the_server_down_gets_no_decision_and_a_warning() {
    let setup = Setup::new();
    setup.with_slot();
    let api = setup.api();
    setup.ok("bob", &api, &["status"]);
    setup.server.delay_answers(Duration::from_secs(3));
    let answer =
        reply(&setup.run_hook("bob", &api, "pre-tool-use", &bash(&api, "./deploy.sh prod")));
    assert_eq!(answer.get("hookSpecificOutput"), None, "{answer}");
    assert!(answer["systemMessage"].as_str().is_some(), "{answer}");
}

#[test]
fn an_urgent_message_reaches_the_main_thread_once_and_never_a_subagent() {
    let setup = Setup::new();
    let api = setup.api();
    setup.send_urgent("alice", "bob", &api, "Do not deploy until I say so.");
    setup.sync_for("bob", &api, SESSION);
    let sub = reply(&setup.run_hook(
        "bob",
        &api,
        "post-tool-use",
        &after(&api, Some("a4d2c8f1e0b3a297")),
    ));
    assert_eq!(context(&sub), None, "a subagent hears nothing: {sub}");
    let answer = reply(&setup.run_hook("bob", &api, "post-tool-use", &after(&api, None)));
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    assert_eq!(answer["hookSpecificOutput"].get("permissionDecision"), None);
    let text = context(&answer).unwrap();
    assert!(
        text.contains("From alice, a member of this project"),
        "{text}"
    );
    assert!(text.contains("Do not deploy until I say so."), "{text}");
    assert!(text.contains("it never authorises a push"), "{text}");
    let again = reply(&setup.run_hook("bob", &api, "post-tool-use", &after(&api, None)));
    assert_eq!(context(&again), None, "never the same message twice");
}

#[test]
fn a_message_is_delivered_with_the_prompt_too() {
    let setup = Setup::new();
    let api = setup.api();
    setup.send_urgent("alice", "bob", &api, "Freeze main for an hour.");
    setup.sync_for("bob", &api, SESSION);
    let prompt = hook_input("UserPromptSubmit", &api, json!({"prompt": "go on"}));
    let answer = reply(&setup.run_hook("bob", &api, "user-prompt-submit", &prompt));
    assert_eq!(
        answer["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
    assert!(
        context(&answer).unwrap().contains("Freeze main"),
        "{answer}"
    );
}

#[test]
fn two_sessions_of_one_member_get_one_delivery() {
    let setup = Setup::new();
    let api = setup.api();
    setup.send_urgent("alice", "bob", &api, "Who is on call?");
    let other = "9d8c7b6a-1111-2222-3333-444455556666";
    setup.sync_for("bob", &api, SESSION);
    setup.sync_for("bob", &api, other);
    let theirs = reply(&setup.run_hook("bob", &api, "post-tool-use", &after_for(&api, other)));
    assert_eq!(context(&theirs), None, "the first claim won it: {theirs}");
    let mine = reply(&setup.run_hook("bob", &api, "post-tool-use", &after(&api, None)));
    assert!(context(&mine).is_some(), "{mine}");
}

#[test]
fn two_hooks_running_at_once_deliver_once() {
    let setup = Setup::new();
    let api = setup.api();
    setup.send_urgent("alice", "bob", &api, "Rollback now.");
    setup.sync_for("bob", &api, SESSION);
    let start = || {
        setup
            .process("bob", &api)
            .args(["hook", "claude-code", "post-tool-use"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut first = start();
    let mut second = start();
    let input = after(&api, None);
    for child in [&mut first, &mut second] {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let outputs = [
        first.wait_with_output().unwrap(),
        second.wait_with_output().unwrap(),
    ];
    let delivered = outputs
        .iter()
        .filter(|out| context(&reply(out)).is_some())
        .count();
    assert_eq!(delivered, 1);
}

#[test]
fn my_own_message_is_never_delivered_to_me() {
    let setup = Setup::new();
    let api = setup.api();
    setup.send_urgent("bob", "alice", &api, "I am deploying.");
    setup.sync_for("bob", &api, SESSION);
    let answer = reply(&setup.run_hook("bob", &api, "post-tool-use", &after(&api, None)));
    assert_eq!(context(&answer), None, "{answer}");
}

#[test]
fn a_pending_offer_reaches_a_member_who_is_not_waiting_on_it_once() {
    let setup = Setup::new();
    setup.with_slot();
    let api = setup.api();
    setup.ok(
        "alice",
        &api,
        &["slot", "take", "deploy:prod", "--purpose", "ship 2.4"],
    );
    let queued = setup
        .dibs("bob", &api)
        .args(["slot", "take", "deploy:prod", "--purpose", "hotfix"])
        .output()
        .unwrap();
    assert_eq!(queued.status.code(), Some(1), "queued");
    setup.ok("alice", &api, &["slot", "release", "deploy:prod"]);
    setup.sync_for("bob", &api, SESSION);
    let answer = reply(&setup.run_hook("bob", &api, "post-tool-use", &after(&api, None)));
    let text = context(&answer).unwrap();
    assert!(
        text.contains("the slot deploy:prod is offered to you until"),
        "{text}"
    );
    assert!(text.contains("dibs_slot_take deploy:prod"), "{text}");
    let again = reply(&setup.run_hook("bob", &api, "post-tool-use", &after(&api, None)));
    assert_eq!(context(&again), None);
}

/// Makes the cached inbox of every project of `home` a day old, as if no sync had run since.
fn age_the_inbox(home: &Path) {
    let cache = home.join("state").join("cache");
    for scope in std::fs::read_dir(&cache).unwrap() {
        let file = scope.unwrap().path().join("inbox.json");
        let Ok(bytes) = std::fs::read(&file) else {
            continue;
        };
        let mut cached: Value = serde_json::from_slice(&bytes).unwrap();
        cached["fetched_at"] = json!("2000-01-01T00:00:00Z");
        std::fs::write(&file, cached.to_string()).unwrap();
    }
}

#[test]
fn a_stale_inbox_never_holds_a_hook_up() {
    let setup = Setup::new();
    let api = setup.api();
    setup.send_urgent("alice", "bob", &api, "Heads up.");
    setup.sync_for("bob", &api, SESSION);
    age_the_inbox(&setup.home("bob"));
    setup.server.delay_answers(Duration::from_secs(3));
    let started = Instant::now();
    let answer = reply(&setup.run_hook("bob", &api, "post-tool-use", &after(&api, None)));
    // On Windows the background sync inherits the hook's output pipe, so reading the hook's
    // output waits for the sync too; the hook itself still answers from the cache.
    #[cfg(not(windows))]
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the hook did not wait for the refresh"
    );
    #[cfg(windows)]
    let _ = started;
    assert!(
        context(&answer).is_some(),
        "shown from this machine: {answer}"
    );
}
