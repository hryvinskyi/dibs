//! `dibs hook claude-code <event>`: the guard's verdicts in Claude Code's hook output, the cache
//! and the circuit that keep a hook off the network, and every failure answered without a
//! decision.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Output, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::{SESSION, Setup, hook_input, reply, text, write_of};

impl Setup {
    /// `dibs hook claude-code <event>` as `handle` in `cwd`, with `stdin`.
    fn hook(&self, handle: &str, cwd: &Path, event: &str, stdin: &str) -> Output {
        self.dibs(handle, cwd)
            .args(["hook", "claude-code", event])
            .write_stdin(stdin.to_owned())
            .output()
            .unwrap()
    }

    /// The `PreToolUse` reply to a `Write` of `path` by `handle`.
    fn write(&self, handle: &str, cwd: &Path, path: &str) -> Value {
        reply(&self.hook(handle, cwd, "pre-tool-use", &write_of(cwd, path)))
    }

    fn claim(&self, handle: &str, cwd: &Path, reference: &str, areas: &[&str]) {
        self.ok(handle, cwd, &["new", reference, "--title", "Billing work"]);
        let mut args = vec!["claim", reference];
        for area in areas {
            args.extend(["--area", area]);
        }
        self.ok(handle, cwd, &args);
    }
}

fn decision(value: &Value) -> Option<&str> {
    value["hookSpecificOutput"]["permissionDecision"].as_str()
}

#[test]
fn a_path_another_member_holds_is_denied_naming_the_holder() {
    let setup = Setup::new();
    let api = setup.api();
    setup.claim("alice", &api, "gh:acme/api#1", &["billing"]);
    let answer = setup.write("bob", &api, "src/billing/a.rs");
    assert_eq!(decision(&answer), Some("deny"), "{answer}");
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains("alice holds for work item"), "{reason}");
    assert!(reason.contains("dibs_send"), "{reason}");
}

#[test]
fn a_free_path_gets_no_decision_and_its_area_is_reserved() {
    let setup = Setup::new();
    let api = setup.api();
    setup.claim("bob", &api, "gh:acme/api#2", &[]);
    assert_eq!(setup.write("bob", &api, "src/billing/a.rs"), Value::Null);
    let state = setup.ok("bob", &api, &["status", "--json"]);
    let holds: Value = serde_json::from_slice(&state.stdout).unwrap();
    assert!(
        holds["holds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hold| hold["area"] == "api:billing" && hold["holder"] == "bob"),
        "{holds}"
    );
    let theirs = setup.write("alice", &api, "src/billing/b.rs");
    assert_eq!(
        decision(&theirs),
        Some("deny"),
        "the reservation holds against others: {theirs}"
    );
}

#[test]
fn without_a_claim_a_governed_path_is_denied_with_the_way_to_claim() {
    let setup = Setup::new();
    let api = setup.api();
    let answer = setup.write("bob", &api, "src/search/q.rs");
    assert_eq!(decision(&answer), Some("deny"), "{answer}");
    let reason = answer["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains("dibs_claim"), "{reason}");
    assert!(reason.contains("--area api:search"), "{reason}");
}

#[test]
fn a_hot_file_rule_reaches_the_agent_as_context_without_a_decision() {
    let setup = Setup::new();
    let api = setup.api();
    setup.claim("alice", &api, "gh:acme/api#3", &[]);
    assert_eq!(setup.write("alice", &api, "Cargo.lock"), Value::Null);
    setup.claim("bob", &api, "gh:acme/api#9", &[]);
    let answer = setup.write("bob", &api, "Cargo.lock");
    assert_eq!(decision(&answer), None, "{answer}");
    assert_eq!(answer["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let context = answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("merge with care"), "{context}");
    assert!(context.contains("also edited by alice"), "{context}");
}

#[test]
fn a_notebook_and_an_edit_are_checked_like_a_write() {
    let setup = Setup::new();
    let api = setup.api();
    setup.claim("alice", &api, "gh:acme/api#4", &["billing"]);
    let notebook = hook_input(
        "PreToolUse",
        &api,
        json!({
            "tool_name": "NotebookEdit",
            "tool_input": {"notebook_path": api.join("src/billing/report.ipynb").to_string_lossy(), "new_source": "x"}
        }),
    );
    let edit = hook_input(
        "PreToolUse",
        &api,
        json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": api.join("src/billing/a.rs").to_string_lossy(), "old_string": "a", "new_string": "b"},
            "agent_id": "agent-abc123",
            "agent_type": "Explore"
        }),
    );
    for stdin in [notebook, edit] {
        let answer = reply(&setup.hook("bob", &api, "pre-tool-use", &stdin));
        assert_eq!(decision(&answer), Some("deny"), "{answer}");
    }
}

/// A running `dibs mcp`, through its handshake.
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn start(setup: &Setup, handle: &str, cwd: &Path) -> Self {
        let mut child = setup
            .process(handle, cwd)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut mcp = Self {
            child,
            stdin,
            lines,
            next_id: 0,
        };
        mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "0.0.1"}
            }),
        );
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("no answer to {method}"));
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    /// Calls a tool and answers its structured result, expecting it to have worked.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &response["result"];
        assert_ne!(result["isError"], json!(true), "{name}: {response}");
        result["structuredContent"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_claim_taken_through_the_mcp_session_lets_the_hook_session_edit() {
    let setup = Setup::new();
    let api = setup.api();
    let mut mcp = Mcp::start(&setup, "bob", &api);
    mcp.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#5", "title": "Fix billing rounding"}),
    );
    let claimed = mcp.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#5", "areas": ["billing"]}),
    );
    assert_eq!(claimed["claim"]["holder"], "bob");
    assert_eq!(setup.write("bob", &api, "src/billing/a.rs"), Value::Null);
    let theirs = setup.write("alice", &api, "src/billing/a.rs");
    assert_eq!(decision(&theirs), Some("deny"), "{theirs}");
}

#[test]
fn a_session_starts_with_the_brief_as_context() {
    let setup = Setup::new();
    let api = setup.api();
    setup.claim("alice", &api, "gh:acme/api#6", &["search"]);
    let out = setup.hook(
        "bob",
        &api,
        "session-start",
        &hook_input("SessionStart", &api, json!({"source": "startup"})),
    );
    let answer = reply(&out);
    assert_eq!(
        answer["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    let brief = answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(brief.contains("you are bob"), "{brief}");
    assert!(brief.contains("alice, areas api:search"), "{brief}");
}

#[test]
fn an_unreachable_server_is_one_warning_and_then_a_minute_without_requests() {
    let setup = Setup::new();
    let api = setup.api();
    setup.server.delay_answers(Duration::from_secs(3));
    let first = setup.write("bob", &api, "src/billing/a.rs");
    assert_eq!(decision(&first), None, "{first}");
    assert_eq!(first.get("hookSpecificOutput"), None, "{first}");
    let warning = first["systemMessage"].as_str().unwrap();
    assert!(warning.contains("for the next minute"), "{warning}");
    let asked = setup.server.requests();
    let started = Instant::now();
    let second = setup.write("bob", &api, "src/billing/b.rs");
    assert_eq!(second, Value::Null, "warned once a minute");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        setup.server.requests(),
        asked,
        "a second call within the minute asks nothing: {:?}",
        setup.server.request_paths()
    );
}

#[test]
fn a_server_that_serves_only_newer_clients_is_one_warning_and_never_a_decision() {
    let setup = Setup::new();
    let gated = dibs_test_server::TestServer::with_client_versions(Some("9.0.0"), None).unwrap();
    let alice = gated.project("acme", "alice").unwrap();
    gated
        .settings(&alice, dibs_test_server::two_repositories())
        .unwrap();
    let dir = setup.checkout_on("gated", &gated.url(), Some("api"));
    let run = |event: &str, input: &str| {
        reply(
            &setup
                .dibs("alice", &dir)
                .env("DIBS_TOKEN", &alice.token)
                .args(["hook", "claude-code", event])
                .write_stdin(input.to_owned())
                .output()
                .unwrap(),
        )
    };
    let first = run("pre-tool-use", &write_of(&dir, "src/billing/a.rs"));
    assert_eq!(decision(&first), None, "{first}");
    assert_eq!(first.get("hookSpecificOutput"), None, "{first}");
    let warning = first["systemMessage"].as_str().unwrap();
    assert!(warning.contains("dibs self-update"), "{warning}");
    let asked = gated.requests();
    assert_eq!(
        run("pre-tool-use", &write_of(&dir, "src/billing/b.rs")),
        Value::Null,
        "the circuit is open, the warning given"
    );
    assert_eq!(gated.requests(), asked);
}

#[test]
fn an_html_403_in_front_of_the_server_is_treated_as_unreachable() {
    let setup = Setup::new();
    let url = dibs_test_server::fixed_answer(
        403,
        "text/html",
        "<html><title>Checking your browser</title></html>",
    )
    .unwrap();
    let dir = setup.checkout_on("proxied", &url, Some("api"));
    let run = || {
        reply(
            &setup
                .dibs("bob", &dir)
                .env("DIBS_TOKEN", &setup.bob_token)
                .args(["hook", "claude-code", "pre-tool-use"])
                .write_stdin(write_of(&dir, "src/billing/a.rs"))
                .output()
                .unwrap(),
        )
    };
    let first = run();
    assert_eq!(decision(&first), None, "{first}");
    let warning = first["systemMessage"].as_str().unwrap();
    assert!(warning.contains("for the next minute"), "{warning}");
    assert!(!warning.contains("Checking your browser"), "{warning}");
    assert_eq!(run(), Value::Null, "the circuit is open, the warning given");
}

#[test]
fn an_ungoverned_path_and_a_held_area_are_answered_without_a_request() {
    let setup = Setup::new();
    let api = setup.api();
    setup.claim("bob", &api, "gh:acme/api#7", &["billing"]);
    setup.ok(
        "bob",
        &api,
        &[
            "sync",
            "--force",
            "--host",
            "claude-code",
            "--session",
            SESSION,
        ],
    );
    let before = setup.server.requests();
    for path in ["README.md", "docs/guide.md", "src/billing/held.rs"] {
        assert_eq!(setup.write("bob", &api, path), Value::Null, "{path}");
    }
    let after_tool = setup.hook(
        "bob",
        &api,
        "post-tool-use",
        &hook_input(
            "PostToolUse",
            &api,
            json!({"tool_name": "Read", "tool_input": {}, "tool_response": {}}),
        ),
    );
    assert_eq!(reply(&after_tool), Value::Null);
    assert_eq!(
        setup.server.request_paths()[before..],
        [] as [String; 0],
        "no hook asked the server"
    );
}

#[test]
fn a_checkout_that_names_no_repository_warns_once_a_minute_per_checkout() {
    let setup = Setup::new();
    let url = setup.server.url();
    let first = setup.checkout_on("first", &url, None);
    let second = setup.checkout_on("second", &url, None);
    let warned = setup.write("bob", &first, "src/billing/a.rs");
    assert_eq!(decision(&warned), None, "{warned}");
    let warning = warned["systemMessage"].as_str().unwrap();
    assert!(warning.contains(".dibs.toml"), "{warning}");
    let asked = setup.server.requests();
    assert_eq!(
        setup.write("bob", &first, "src/billing/b.rs"),
        Value::Null,
        "not again within the minute"
    );
    assert_eq!(setup.server.requests(), asked, "and nothing was asked");
    let elsewhere = setup.write("bob", &second, "src/billing/a.rs");
    assert!(
        elsewhere["systemMessage"].is_string(),
        "another checkout has its own minute: {elsewhere}"
    );
}

#[test]
fn whatever_goes_wrong_a_hook_exits_0_without_a_decision() {
    let setup = Setup::new();
    let api = setup.api();
    let run = |args: &[&str], stdin: &str| {
        setup
            .dibs("bob", &api)
            .args(args)
            .write_stdin(stdin.to_owned())
            .output()
            .unwrap()
    };
    let write = write_of(&api, "src/billing/a.rs");
    for args in [
        &["hook", "claude-code", "pre-commit"][..],
        &["hook", "claude-code", "before-tool"][..],
        &["hook", "claude-code"][..],
    ] {
        let answer = reply(&run(args, &write));
        assert_eq!(decision(&answer), None, "{args:?}: {answer}");
        assert!(answer["systemMessage"].is_string(), "{args:?}: {answer}");
    }
    let newer = reply(&run(
        &[
            "hook",
            "claude-code",
            "pre-tool-use",
            "--from-a-newer-plugin",
        ],
        &write_of(&api, "README.md"),
    ));
    assert_eq!(
        newer,
        Value::Null,
        "an unknown option does not stop the hook"
    );
    let garbage = reply(&run(&["hook", "claude-code", "pre-tool-use"], "{ not json"));
    assert!(garbage["systemMessage"].is_string(), "{garbage}");
    let outside = setup.elsewhere();
    let away = setup
        .dibs("bob", &outside)
        .args(["hook", "claude-code", "pre-tool-use"])
        .write_stdin(write_of(&outside, "a.rs"))
        .output()
        .unwrap();
    assert_eq!(reply(&away), Value::Null);
    std::fs::write(api.join(".dibs.toml"), "server = 12\n").unwrap();
    let broken = reply(&run(&["hook", "claude-code", "pre-tool-use"], &write));
    let notice = broken["systemMessage"].as_str().unwrap_or_default();
    assert!(notice.contains(".dibs.toml"), "{broken}");
}

#[test]
fn doctor_lists_claude_code_with_what_it_carries() {
    let setup = Setup::new();
    let api = setup.api();
    let out = setup.ok("bob", &api, &["doctor", "--json"]);
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    let host = report["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|host| host["id"] == "claude-code")
        .cloned()
        .unwrap_or_else(|| panic!("{}", text(&out.stdout)));
    assert_eq!(host["capabilities"]["deny_edit"], true);
    assert_eq!(host["capabilities"]["tells_subagent"], true);
    assert_eq!(host["capabilities"]["push_to_idle"], false);
}
