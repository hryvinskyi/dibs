//! `dibs hook claude-code <event>`: the guard's verdicts in Claude Code's hook output, the cache
//! and the circuit that keep a hook off the network, every failure answered without a decision,
//! and the worktrees Claude Code asks for: lent from the pool, kept alive by the tools run in them,
//! let go of when they are given back, their agent stops or their session ends, and made as Claude
//! Code would make them where no pool lends one.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Output, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::{
    SESSION, Setup, canonical, git, hook_input, last_line, reply, text, write_of,
};

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
    assert_eq!(
        host["capabilities"]["push_to_idle"], true,
        "through its channel, once a session opts in"
    );
}

/// The answer to a `WorktreeCreate` asking for the worktree `name`, from `cwd`, by `handle`.
fn worktree_create(setup: &Setup, handle: &str, cwd: &Path, name: &str) -> Output {
    setup.hook(
        handle,
        cwd,
        "worktree-create",
        &hook_input("WorktreeCreate", cwd, json!({"name": name})),
    )
}

#[test]
fn a_subagents_worktree_comes_from_the_pool_beats_with_its_tools_and_is_let_go_of_when_it_stops() {
    let setup = Setup::new();
    let widgets = setup.pool(2);
    setup.ok("bob", &widgets, &["worktree", "prepare", "worktree1"]);

    let created = worktree_create(&setup, "bob", &widgets, "agent-x");
    assert_eq!(created.status.code(), Some(0), "{}", text(&created.stderr));
    let worktree = setup.pool_worktree("worktree1");
    assert_eq!(last_line(&created), worktree);
    assert_eq!(
        text(&created.stdout).lines().count(),
        1,
        "the directory alone: {}",
        text(&created.stdout)
    );
    let held = setup.worktrees("bob", &widgets)[0]["held"].clone();
    assert_eq!(
        held["agent"], "x",
        "held for the agent the name carries: {held}"
    );

    let beat = setup.worktree_state("bob", "worktree1", "beat");
    let _ = fs::remove_file(&beat);
    let after = setup.hook(
        "bob",
        &worktree,
        "post-tool-use",
        &hook_input(
            "PostToolUse",
            &worktree,
            json!({"tool_name": "Read", "tool_input": {}, "agent_id": "x", "agent_type": "general-purpose"}),
        ),
    );
    assert_eq!(after.status.code(), Some(0), "{}", text(&after.stderr));
    assert!(
        beat.exists(),
        "a tool the agent ran in it is its sign of life"
    );

    let stopped = setup.hook(
        "bob",
        &worktree,
        "subagent-stop",
        &hook_input(
            "SubagentStop",
            &worktree,
            json!({"agent_id": "x", "agent_type": "general-purpose", "stop_hook_active": false}),
        ),
    );
    assert_eq!(reply(&stopped), Value::Null);
    assert_eq!(
        setup.worktrees("bob", &widgets)[0]["held"],
        Value::Null,
        "the agent's end let go of it"
    );
    assert!(worktree.exists(), "its directory stays for the next take");
}

#[test]
fn a_sessions_worktree_is_let_go_of_when_it_is_given_back_and_when_the_session_ends() {
    let setup = Setup::new();
    let widgets = setup.pool(1);
    setup.ok("bob", &widgets, &["worktree", "prepare", "worktree1"]);
    let worktree = setup.pool_worktree("worktree1");
    let held = || setup.worktrees("bob", &widgets)[0]["held"].clone();

    let created = worktree_create(&setup, "bob", &widgets, "fix-totals");
    assert_eq!(created.status.code(), Some(0), "{}", text(&created.stderr));
    assert_eq!(last_line(&created), worktree);
    let hold = held();
    assert_eq!(hold["agent"], Value::Null, "the session holds it: {hold}");
    assert!(
        hold["session"].as_str().unwrap().contains(&SESSION[..8]),
        "{hold}"
    );

    let given_back = setup.hook(
        "bob",
        &worktree,
        "worktree-remove",
        &hook_input(
            "WorktreeRemove",
            &worktree,
            json!({"worktree_path": worktree.to_string_lossy()}),
        ),
    );
    assert_eq!(reply(&given_back), Value::Null);
    assert_eq!(held(), Value::Null);
    assert!(
        worktree.exists(),
        "a pool worktree given back is never deleted"
    );

    let again = worktree_create(&setup, "bob", &widgets, "fix-totals");
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr));
    assert_eq!(last_line(&again), worktree);
    assert_ne!(held(), Value::Null);
    let ended = setup.hook(
        "bob",
        &worktree,
        "session-end",
        &hook_input("SessionEnd", &worktree, json!({"reason": "clear"})),
    );
    assert_eq!(reply(&ended), Value::Null);
    assert_eq!(held(), Value::Null, "the session's end let go of it");
}

#[test]
fn where_no_pool_lends_a_worktree_dibs_makes_claude_codes_own_and_says_why() {
    let setup = Setup::new();
    let api = setup.api();
    let plain = setup.root.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &[
            "-c",
            "user.name=Bob",
            "-c",
            "user.email=bob@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ],
    ] {
        git(&plain, args);
    }
    for (checkout, because) in [
        (&api, "no [worktrees] table"),
        (&plain, "no .dibs.toml governs this directory"),
    ] {
        let made = worktree_create(&setup, "bob", checkout, "agent-y");
        assert_eq!(made.status.code(), Some(0), "{}", text(&made.stderr));
        let own = canonical(&checkout.join(".claude/worktrees/agent-y"));
        assert_eq!(last_line(&made), own);
        assert!(own.join(".git").is_file(), "a linked worktree");
        assert_eq!(
            git(&own, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "worktree-agent-y",
            "on a new branch, as Claude Code names it"
        );
        let said = text(&made.stderr);
        assert!(said.contains("pool of worktrees was not used"), "{said}");
        assert!(said.contains(because), "{said}");

        let given_back = setup.hook(
            "bob",
            &own,
            "worktree-remove",
            &hook_input(
                "WorktreeRemove",
                &own,
                json!({"worktree_path": own.to_string_lossy()}),
            ),
        );
        assert_eq!(reply(&given_back), Value::Null);
        assert!(!own.exists(), "removed as git removes a clean worktree");
    }
}

#[test]
fn a_pool_with_nothing_prepared_makes_claude_codes_own_worktree_at_once_and_prepares_one_meanwhile()
{
    let setup = Setup::new();
    let widgets = setup.pool(2);

    let asked = std::time::Instant::now();
    let made = worktree_create(&setup, "bob", &widgets, "agent-x");
    assert_eq!(made.status.code(), Some(0), "{}", text(&made.stderr));
    assert!(
        asked.elapsed() < std::time::Duration::from_secs(60),
        "answered without waiting for a preparation: {:?}",
        asked.elapsed()
    );
    let own = canonical(&widgets.join(".claude/worktrees/agent-x"));
    assert_eq!(last_line(&made), own);
    assert!(own.join(".git").is_file(), "a linked worktree");
    let said = text(&made.stderr);
    assert!(
        said.contains("the pool of worktrees is still being prepared")
            && said.contains("without an environment of its own")
            && said.contains("worktree1 is being prepared"),
        "{said}"
    );

    let prepared = setup.pool_worktree("worktree1").join(".dibs.toml");
    let started = std::time::Instant::now();
    while !prepared.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the background preparation never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        !setup.pool_worktree("worktree2").exists(),
        "one preparation was started, not one per worktree"
    );

    let next = worktree_create(&setup, "bob", &widgets, "agent-y");
    assert_eq!(next.status.code(), Some(0), "{}", text(&next.stderr));
    assert_eq!(
        last_line(&next),
        setup.pool_worktree("worktree1"),
        "once prepared, the pool lends it"
    );
}

#[test]
fn a_repeated_request_for_the_same_agents_worktree_gets_it_back_untouched() {
    let setup = Setup::new();
    let widgets = setup.pool(2);
    setup.ok("bob", &widgets, &["worktree", "prepare", "worktree1"]);
    let worktree = setup.pool_worktree("worktree1");

    let first = worktree_create(&setup, "bob", &widgets, "agent-x");
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert_eq!(last_line(&first), worktree);
    let scratch = worktree.join("scratch.txt");
    fs::write(&scratch, "work in progress\n").unwrap();

    let second = worktree_create(&setup, "bob", &widgets, "agent-x");
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr));
    assert_eq!(last_line(&second), worktree);
    assert!(
        scratch.exists(),
        "asking again is not a new take, so nothing is reset"
    );
    let held: Vec<Value> = setup
        .worktrees("bob", &widgets)
        .into_iter()
        .filter(|listed| !listed["held"].is_null())
        .collect();
    assert_eq!(held.len(), 1, "one hold, not two: {held:?}");
}

#[test]
fn an_agents_edit_in_its_pool_worktree_is_checked_against_claims_like_one_in_the_checkout() {
    let setup = Setup::new();
    let api = setup.api();
    let widgets = setup.pool(1);
    setup.claim("alice", &api, "gh:acme/api#1", &["billing"]);
    setup.claim("bob", &widgets, "gh:acme/api#2", &[]);
    setup.ok("bob", &widgets, &["worktree", "prepare", "worktree1"]);
    let worktree = setup.pool_worktree("worktree1");
    let created = worktree_create(&setup, "bob", &widgets, "agent-x");
    assert_eq!(created.status.code(), Some(0), "{}", text(&created.stderr));

    let write = |path: &str| {
        let mut input: Value = serde_json::from_str(&write_of(&worktree, path)).unwrap();
        input["agent_id"] = json!("x");
        input["agent_type"] = json!("general-purpose");
        reply(&setup.hook("bob", &worktree, "pre-tool-use", &input.to_string()))
    };
    let denied = write("src/billing/a.rs");
    assert_eq!(decision(&denied), Some("deny"), "{denied}");
    let reason = denied["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains("alice holds for work item"), "{reason}");
    assert_eq!(write("src/search/q.rs"), Value::Null);
}
