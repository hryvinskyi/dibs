//! `dibs mcp` as a child process against the real server router: the JSON-RPC handshake and tool
//! list over standard input and output, tool calls whose answers are the client's JSON, a refusal
//! that comes back as a tool error naming the holder, and a server that cannot be reached.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::{API_REMOTE, Setup, make_checkout, status, text};

const WAIT: Duration = Duration::from_secs(30);

/// The tools the server offers, by name.
const TOOLS: [&str; 24] = [
    "dibs_ack",
    "dibs_ask_grant",
    "dibs_check",
    "dibs_claim",
    "dibs_done",
    "dibs_escalate",
    "dibs_feedback",
    "dibs_grant",
    "dibs_inbox",
    "dibs_items",
    "dibs_narrow",
    "dibs_new",
    "dibs_read",
    "dibs_ready",
    "dibs_release",
    "dibs_review",
    "dibs_send",
    "dibs_slot_leave",
    "dibs_slot_release",
    "dibs_slot_take",
    "dibs_slots",
    "dibs_status",
    "dibs_takeover",
    "dibs_thread",
];

/// What a tool call answered.
struct Reply {
    is_error: bool,
    text: String,
    structured: Value,
}

impl Reply {
    /// The structured answer of a call that worked.
    fn ok(&self) -> &Value {
        assert!(!self.is_error, "a tool error: {}", self.text);
        &self.structured
    }

    /// The sentence of a call that failed.
    fn error(&self) -> &str {
        assert!(self.is_error, "the call worked: {}", self.text);
        &self.text
    }
}

/// A running `dibs mcp` that has been through the handshake.
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
    handshake: Value,
}

impl Mcp {
    /// Starts `dibs mcp` as `handle` in `cwd` and performs the handshake.
    fn start(setup: &Setup, handle: &str, cwd: &Path) -> Self {
        Self::spawn(setup.process_as(handle, cwd))
    }

    fn spawn(mut command: std::process::Command) -> Self {
        let mut child = command
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
            handshake: Value::Null,
        };
        mcp.handshake = mcp.request(
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

    /// Sends a request and answers the response to it, skipping anything else the server says.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => panic!("no answer to {method} in {WAIT:?}"),
                Err(RecvTimeoutError::Disconnected) => panic!("the server closed before {method}"),
            };
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|_| panic!("standard output carried something else: {line}"));
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    /// Calls a tool and answers its reply.
    fn tool(&mut self, name: &str, arguments: Value) -> Reply {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = &response["result"];
        assert!(result.is_object(), "not a tool result: {response}");
        Reply {
            is_error: result["isError"].as_bool().unwrap_or(false),
            text: result["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            structured: result["structuredContent"].clone(),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Closes standard input and answers the exit status once the server has ended.
    fn finish(mut self) -> Option<i32> {
        drop(self.stdin.take());
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Some(code) = self.child.try_wait().unwrap() {
                return code.code();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
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
fn the_handshake_names_the_server_and_lists_every_tool_with_a_schema() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut mcp = Mcp::start(&setup, "alice", &api);

    let init = &mcp.handshake["result"];
    assert_eq!(init["serverInfo"]["name"], "dibs");
    assert!(init["protocolVersion"].is_string());
    assert!(init["capabilities"]["tools"].is_object());

    let listed = mcp.request("tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    assert_eq!(names, TOOLS);
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert!(
            tool["description"].as_str().unwrap().len() > 40,
            "{name} says when to use it"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
    }
    let schema = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()
            .get("inputSchema")
            .unwrap()
            .clone()
    };
    assert_eq!(schema("dibs_claim")["required"], json!(["key"]));
    assert!(schema("dibs_claim")["properties"]["areas"].is_object());
    assert_eq!(schema("dibs_new")["required"], json!(["ref", "title"]));
    assert_eq!(schema("dibs_check")["required"], json!(["paths"]));
    assert_eq!(schema("dibs_send")["required"], json!(["body"]));
    assert_eq!(schema("dibs_status")["type"], "object");
}

#[test]
fn a_claim_is_taken_in_the_members_name_and_the_command_line_sees_it() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut mcp = Mcp::start(&setup, "alice", &api);

    let created = mcp.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#7", "title": "Fix billing rounding"}),
    );
    assert_eq!(created.ok()["created"], true);
    let id = created.ok()["item"]["id"].as_u64().unwrap();

    let claimed = mcp.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#7", "areas": ["billing"]}),
    );
    let answer = claimed.ok();
    assert_eq!(answer["outcome"], "new");
    assert_eq!(answer["claim"]["holder"], "alice");
    assert_eq!(answer["claim"]["areas"], json!(["api:billing"]));
    let as_text: Value = serde_json::from_str(&claimed.text).unwrap();
    assert_eq!(&as_text, answer, "the text carries the same JSON");

    let renewed = mcp.tool(
        "dibs_claim",
        json!({"key": id, "areas": ["billing", "search"]}),
    );
    assert_eq!(renewed.ok()["outcome"], "renewed");

    let from_cli = setup
        .dibs(&setup.alice, &api)
        .args(["items", "--mine"])
        .output()
        .unwrap();
    assert_eq!(status(&from_cli), 0);
    assert!(text(&from_cli.stdout).contains("gh:acme/api#7 Fix billing rounding [claimed]"));

    let mine = mcp.tool("dibs_items", json!({"mine": true, "status": "claimed"}));
    assert_eq!(mine.ok()["items"].as_array().unwrap().len(), 1);
    let open = mcp.tool("dibs_items", json!({"status": "open"}));
    assert!(open.ok()["items"].as_array().unwrap().is_empty());

    let state = mcp.tool("dibs_status", json!({}));
    assert_eq!(state.ok()["live"].as_array().unwrap().len(), 1);

    let released = mcp.tool("dibs_release", json!({"key": "gh:acme/api#7"}));
    assert_eq!(released.ok()["claim"]["item"], id);
    let reclaimed = mcp.tool("dibs_claim", json!({"key": id}));
    assert_eq!(reclaimed.ok()["outcome"], "new");
    let done = mcp.tool("dibs_done", json!({"key": id}));
    assert_eq!(done.ok()["claim"]["holder"], "alice");
    let finished = mcp.tool("dibs_items", json!({"status": "done"}));
    assert_eq!(finished.ok()["items"].as_array().unwrap().len(), 1);
}

#[test]
fn a_claim_goes_to_review_through_a_tool_and_another_members_edit_waits_for_the_merge() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#7", "title": "Fix billing rounding"}),
    );
    alice.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#7", "areas": ["billing"]}),
    );
    let reviewed = alice.tool(
        "dibs_review",
        json!({"key": "gh:acme/api#7", "pr": "acme/api#12", "more": true}),
    );
    let answer = reviewed.ok();
    assert_eq!(answer["claim"]["state"], "in_review");
    assert_eq!(answer["claim"]["review"]["prs"], json!(["acme/api#12"]));
    assert_eq!(answer["claim"]["review"]["more"], true);
    let held = bob.tool("dibs_check", json!({"paths": ["src/billing/round.rs"]}));
    let verdict = &held.ok()["verdicts"][0];
    assert_eq!(verdict["verdict"], "held_by", "{verdict}");
    assert_eq!(verdict["reason"], "in_review");
    assert_eq!(verdict["prs"], json!(["acme/api#12"]));
    let malformed = alice.tool(
        "dibs_review",
        json!({"key": "gh:acme/api#7", "pr": "acme/api"}),
    );
    assert!(malformed.error().contains("pr"), "{}", malformed.text);
    let not_mine = bob.tool(
        "dibs_review",
        json!({"key": "gh:acme/api#7", "pr": "acme/api#13"}),
    );
    assert!(not_mine.error().contains("alice"), "{}", not_mine.text);
    let dropped = alice.tool(
        "dibs_review",
        json!({"key": "gh:acme/api#7", "pr": "acme/api#12", "drop": true}),
    );
    assert_eq!(dropped.ok()["claim"]["state"], "working");
}

#[test]
fn a_held_claim_is_a_tool_error_naming_the_holder() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#1", "title": "Billing"}),
    );
    bob.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#2", "title": "More billing"}),
    );
    alice
        .tool(
            "dibs_claim",
            json!({"key": "gh:acme/api#1", "areas": ["billing"]}),
        )
        .ok();

    let item_held = bob.tool("dibs_claim", json!({"key": "gh:acme/api#1"}));
    assert!(item_held.error().contains("alice"), "{}", item_held.text);
    assert!(item_held.error().ends_with('.'));
    let code = item_held.structured["error"]["code"].as_str().unwrap();
    assert!(code.contains("held"), "{code}");

    let area_held = bob.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#2", "areas": ["billing"]}),
    );
    assert!(area_held.error().contains("alice"), "{}", area_held.text);
    assert!(
        area_held.error().contains("on item gh:acme/api#1"),
        "the holder's item is named by its reference: {}",
        area_held.text
    );
    let details = &area_held.structured["error"]["details"];
    assert_eq!(details["holder"], "alice");
    assert_eq!(details["item_ref"], "gh:acme/api#1");
    assert_eq!(details["area"], "api:billing");

    let not_stale = bob.tool(
        "dibs_takeover",
        json!({"key": "gh:acme/api#1", "reason": "You went quiet."}),
    );
    assert!(not_stale.error().contains("alice"), "{}", not_stale.text);

    let unknown = bob.tool("dibs_claim", json!({"key": "gh:acme/api#99"}));
    assert!(
        unknown.error().contains("gh:acme/api#99"),
        "{}",
        unknown.text
    );
}

#[test]
fn the_ready_list_is_a_tool_and_a_claim_answer_names_its_blockers() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#1", "title": "Waits"}),
    );
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#2", "title": "Blocks"}),
    );
    let added = setup
        .dibs(&setup.alice, &api)
        .args([
            "dep",
            "add",
            "gh:acme/api#1",
            "--blocked-by",
            "gh:acme/api#2",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&added), 0, "{}", text(&added.stderr));

    let ready = alice.tool("dibs_ready", json!({}));
    let answer = ready.ok();
    assert_eq!(answer["ready"][0]["item"]["ref"], "gh:acme/api#2");
    assert_eq!(
        answer["ready"][0]["link"],
        "https://github.com/acme/api/issues/2"
    );
    assert_eq!(answer["excluded"][0]["reasons"][0]["reason"], "blocked_by");

    let claimed = alice.tool("dibs_claim", json!({"key": "gh:acme/api#1"}));
    assert_eq!(claimed.ok()["outcome"], "new");
    assert_eq!(claimed.ok()["blocked_by"][0]["ref"], "gh:acme/api#2");
}

#[test]
fn a_check_answers_a_verdict_per_path_and_changes_nothing() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#1", "title": "Billing"}),
    );
    alice
        .tool(
            "dibs_claim",
            json!({"key": "gh:acme/api#1", "areas": ["billing"]}),
        )
        .ok();
    let absolute = api.join("src/billing/a.rs").display().to_string();

    let theirs = bob.tool(
        "dibs_check",
        json!({"paths": ["src/billing/a.rs", "README.md", "src/search/b.rs"]}),
    );
    let verdicts = theirs.ok()["verdicts"].as_array().unwrap();
    assert_eq!(verdicts.len(), 3);
    assert_eq!(verdicts[0]["verdict"], "held_by");
    assert_eq!(verdicts[0]["holder"], "alice");
    assert_eq!(verdicts[0]["path"], "src/billing/a.rs");
    assert_eq!(verdicts[1]["verdict"], "allowed");
    assert_eq!(verdicts[2]["verdict"], "needs_claim");

    let mine = alice.tool("dibs_check", json!({"paths": [absolute]}));
    assert_eq!(mine.ok()["verdicts"][0]["verdict"], "allowed");

    let nested = api.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    let from_below =
        Mcp::start(&setup, "alice", &nested).tool("dibs_check", json!({"paths": ["billing/a.rs"]}));
    assert_eq!(from_below.ok()["verdicts"][0]["path"], "src/billing/a.rs");

    let outside = bob.tool("dibs_check", json!({"paths": ["../elsewhere/file.rs"]}));
    assert!(outside.error().ends_with('.'), "{}", outside.text);
    let none = bob.tool("dibs_check", json!({"paths": []}));
    assert!(none.error().starts_with("`paths`: "), "{}", none.text);

    let after = bob.tool("dibs_status", json!({}));
    assert_eq!(after.ok()["live"].as_array().unwrap().len(), 1);
}

#[test]
fn messages_are_sent_read_and_answered_under_the_sessions_label() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#7", "title": "Billing"}),
    );

    let sent = alice.tool(
        "dibs_send",
        json!({
            "to": ["bob"], "subject": "Billing rates", "body": "Line one\nLine two",
            "item": "gh:acme/api#7"
        }),
    );
    let thread = sent.ok()["thread"]["id"].as_u64().unwrap();
    let label = sent.ok()["message"]["label"].as_str().unwrap();
    assert!(label.ends_with(&format!("/mcp-{}", alice.pid())), "{label}");
    assert!(sent.ok()["thread"]["item"].is_u64());

    let inbox = bob.tool("dibs_inbox", json!({"unread": true}));
    let threads = inbox.ok()["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["unread"], 1);
    assert_eq!(threads[0]["thread"]["subject"], "Billing rates");

    let shown = bob.tool("dibs_thread", json!({"thread": thread}));
    assert_eq!(shown.ok()["messages"][0]["body"], "Line one\nLine two");
    let still_unread = bob.tool("dibs_inbox", json!({"unread": true}));
    assert_eq!(still_unread.ok()["threads"].as_array().unwrap().len(), 1);

    let answered = bob.tool("dibs_send", json!({"thread": thread, "body": "Thanks."}));
    let label = answered.ok()["message"]["label"].as_str().unwrap();
    assert!(label.ends_with(&format!("/mcp-{}", bob.pid())), "{label}");

    let read = bob.tool("dibs_read", json!({"thread": thread}));
    assert_eq!(read.ok()["newly_read"], 1);
    let none = bob.tool("dibs_inbox", json!({"unread": true}));
    assert!(none.ok()["threads"].as_array().unwrap().is_empty());

    let from_cli = setup
        .dibs(&setup.alice, &api)
        .args(["inbox", "--unread"])
        .output()
        .unwrap();
    assert!(
        text(&from_cli.stdout).contains(&format!("#{thread} Billing rates")),
        "{}",
        text(&from_cli.stdout)
    );

    let everyone = alice.tool(
        "dibs_send",
        json!({"project": true, "subject": "Freeze", "body": "Merge freeze on Friday."}),
    );
    assert_eq!(everyone.ok()["thread"]["to_project"], true);
    assert_eq!(everyone.ok()["message"]["importance"], "normal");
    assert_eq!(everyone.ok()["message"]["ack_required"], false);
}

#[test]
fn feedback_through_the_tools_names_a_refusal_or_the_area_of_a_conflict() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut bob = Mcp::start(&setup, "bob", &api);
    let conflict = bob.tool(
        "dibs_feedback",
        json!({"kind": "conflict", "path": "src/billing/rates.rs", "reason": "rebased over alice's change"}),
    );
    assert_eq!(
        conflict.ok()["feedback"],
        json!({"kind": "conflict", "repo": "api", "area": "api:billing"}),
        "{}",
        conflict.text
    );
    let refusal = bob.tool(
        "dibs_feedback",
        json!({"kind": "false_refusal", "refusal": "r-7f3a", "reason": "billing was free"}),
    );
    assert_eq!(
        refusal.ok()["feedback"],
        json!({"kind": "false_refusal", "refusal": "r-7f3a", "tally": null}),
        "a refusal this machine never showed has no kind"
    );
    let wrong = bob.tool("dibs_feedback", json!({"kind": "praise", "reason": "nice"}));
    assert!(wrong.error().contains("`kind`"), "{}", wrong.text);
    let log = setup.server.request_log().join("\n");
    assert!(
        log.contains("src/billing/rates.rs"),
        "the file goes to the server, which keeps only its area"
    );
}

#[test]
fn an_escalation_through_the_tools_reaches_the_arbiter_from_dibs_and_counts_toward_no_limit() {
    let setup = Setup::new();
    let mut settings = dibs_test_server::two_repositories();
    settings["arbiters"] = json!(["alice"]);
    settings["raised_per_hour"] = json!(1);
    setup.server.settings(&setup.alice, settings).unwrap();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    let urgent = bob.tool(
        "dibs_send",
        json!({"to": ["alice"], "subject": "Rates", "body": "Now?", "importance": "urgent"}),
    );
    assert!(!urgent.is_error, "{}", urgent.text);
    let item = bob
        .tool(
            "dibs_new",
            json!({"ref": "gh:acme/api#78", "title": "Rates"}),
        )
        .ok()["item"]["id"]
        .as_u64()
        .unwrap();
    let other = bob
        .tool(
            "dibs_new",
            json!({"ref": "gh:acme/api#79", "title": "Search"}),
        )
        .ok()["item"]["id"]
        .as_u64()
        .unwrap();
    for subject in [item, other] {
        let sent = bob.tool(
            "dibs_escalate",
            json!({"item": subject, "reason": "our agents keep undoing each other"}),
        );
        assert_eq!(
            sent.ok()["escalation"]["arbiters"],
            json!(["alice"]),
            "{}",
            sent.text
        );
    }
    let again = bob.tool(
        "dibs_escalate",
        json!({"item": item, "reason": "still undoing each other"}),
    );
    assert!(
        again
            .error()
            .starts_with("This is escalated already, in conversation"),
        "{}",
        again.error()
    );
    let both = bob.tool(
        "dibs_escalate",
        json!({"item": item, "slot": "deploy:prod", "reason": "?"}),
    );
    assert!(both.error().starts_with("`item`: "), "{}", both.error());

    let inbox = alice.tool("dibs_inbox", json!({"unacked": true}));
    let unacked = inbox.ok()["unacked"].as_array().unwrap().clone();
    assert_eq!(unacked.len(), 2);
    assert_eq!(unacked[0]["message"]["author"], "dibs");
    assert_eq!(unacked[0]["message"]["system"], true);
    assert_eq!(unacked[0]["message"]["importance"], "urgent");
    let status = alice.tool("dibs_status", json!({}));
    assert_eq!(status.ok()["escalations"].as_array().unwrap().len(), 2);
}

#[test]
fn an_urgent_message_is_acked_through_the_tools_once() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    let sent = alice.tool(
        "dibs_send",
        json!({
            "to": ["bob"], "subject": "Staging is down", "body": "Do not deploy.",
            "importance": "urgent", "ack_required": true
        }),
    );
    assert_eq!(sent.ok()["message"]["importance"], "urgent");
    assert_eq!(sent.ok()["message"]["ack_required"], true);
    let thread = sent.ok()["thread"]["id"].as_u64().unwrap();
    let message = sent.ok()["message"]["id"].as_u64().unwrap();

    let reply = bob.tool("dibs_send", json!({"thread": thread, "body": "Holding."}));
    assert_eq!(
        reply.ok()["message"]["importance"],
        "normal",
        "a reply is normal"
    );
    assert_eq!(reply.ok()["message"]["ack_required"], false);

    let inbox = bob.tool("dibs_inbox", json!({"unacked": true}));
    assert_eq!(inbox.ok()["unacked"][0]["message"]["id"], message);
    assert_eq!(inbox.ok()["threads"][0]["awaiting_ack"], 1);
    let shown = bob.tool("dibs_thread", json!({"thread": thread}));
    assert_eq!(
        shown.ok()["messages"][0]["acks"],
        json!([{"member": "bob", "acked_at": null}])
    );

    let acked = bob.tool("dibs_ack", json!({"message": message}));
    assert_eq!(acked.ok()["ack"]["member"], "bob");
    assert_eq!(acked.ok()["ack"]["thread"], thread);
    assert_eq!(acked.ok()["created"], true);
    let again = bob.tool("dibs_ack", json!({"message": message}));
    assert_eq!(again.ok()["created"], false, "a retry is no tool error");
    assert_eq!(again.ok()["ack"]["at"], acked.ok()["ack"]["at"]);
    let by_author = alice.tool("dibs_ack", json!({"message": message}));
    assert!(
        by_author
            .error()
            .starts_with("Only a member this message was sent to"),
        "{}",
        by_author.error()
    );
    let zero = bob.tool("dibs_ack", json!({"message": 0}));
    assert!(zero.error().starts_with("`message`: "), "{}", zero.error());
    let loud = alice.tool(
        "dibs_send",
        json!({"thread": thread, "body": "x", "importance": "loud"}),
    );
    assert!(loud.is_error);
    let left = bob.tool("dibs_inbox", json!({"unacked": true}));
    assert_eq!(left.ok()["unacked"], json!([]));
}

#[test]
fn an_edit_refused_by_a_held_area_asks_its_holder_for_a_grant_through_the_tools() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#1", "title": "Invoices"}),
    );
    bob.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#2", "title": "Refunds"}),
    );
    alice
        .tool(
            "dibs_claim",
            json!({"key": "gh:acme/api#1", "areas": ["billing"]}),
        )
        .ok();
    bob.tool("dibs_claim", json!({"key": "gh:acme/api#2"})).ok();
    let held = bob.tool("dibs_check", json!({"paths": ["src/billing/a.rs"]}));
    assert_eq!(held.ok()["verdicts"][0]["verdict"], "held_by");

    let asked = bob.tool(
        "dibs_ask_grant",
        json!({"key": "gh:acme/api#2", "area": "billing"}),
    );
    let answer = asked.ok();
    assert_eq!(answer["outcome"], "asked");
    assert_eq!(answer["holder"], "alice");
    assert_eq!(answer["holder_item"]["ref"], "gh:acme/api#1");
    assert_eq!(answer["item"]["ref"], "gh:acme/api#2");
    assert_eq!(answer["area"], "api:billing");
    let thread = answer["thread_id"].as_u64().unwrap();

    let read = alice.tool("dibs_thread", json!({"thread": thread}));
    let detail = read.ok();
    assert_eq!(detail["thread"]["author"], "bob");
    assert_eq!(detail["thread"]["to"], json!(["alice"]));
    let message = &detail["messages"][0];
    assert_eq!(message["importance"], "high");
    let body = message["body"].as_str().unwrap();
    assert!(
        body.contains("dibs grant gh:acme/api#2 --area api:billing --to bob"),
        "{body}"
    );
    alice
        .tool(
            "dibs_grant",
            json!({"key": "gh:acme/api#2", "area": "api:billing", "to": "bob"}),
        )
        .ok();
    let again = bob.tool(
        "dibs_ask_grant",
        json!({"key": "gh:acme/api#2", "area": "billing"}),
    );
    assert_eq!(again.ok()["outcome"], "granted");
    assert_eq!(again.ok()["thread_id"], serde_json::Value::Null);
    let granted = bob.tool("dibs_check", json!({"paths": ["src/billing/a.rs"]}));
    assert_eq!(granted.ok()["verdicts"][0]["verdict"], "granted");

    let not_mine = bob.tool(
        "dibs_ask_grant",
        json!({"key": "gh:acme/api#1", "area": "billing"}),
    );
    assert!(
        not_mine.error().contains("no live claim"),
        "{}",
        not_mine.text
    );
    assert_eq!(not_mine.structured["error"]["code"], "grantee_has_no_claim");
    let bad_key = bob.tool(
        "dibs_ask_grant",
        json!({"key": "not a key", "area": "billing"}),
    );
    assert!(bad_key.error().starts_with("`key`: "), "{}", bad_key.text);
}

#[test]
fn input_that_cannot_be_used_is_a_tool_error_that_says_what_to_change() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut mcp = Mcp::start(&setup, "alice", &api);

    let bad_key = mcp.tool("dibs_claim", json!({"key": "not a key"}));
    assert!(
        bad_key
            .error()
            .starts_with("`key`: 'not a key' is not a work item"),
        "{}",
        bad_key.text
    );
    let no_recipient = mcp.tool("dibs_send", json!({"subject": "s", "body": "b"}));
    assert!(
        no_recipient.error().starts_with("`to`: "),
        "{}",
        no_recipient.text
    );
    let mixed = mcp.tool(
        "dibs_send",
        json!({"thread": 3, "subject": "s", "body": "b"}),
    );
    assert!(mixed.error().starts_with("`thread`: "), "{}", mixed.text);
    let no_reason = mcp.tool("dibs_takeover", json!({"key": 1, "reason": " "}));
    assert!(
        no_reason.error().starts_with("`reason`: "),
        "{}",
        no_reason.text
    );
    let bad_grantee = mcp.tool(
        "dibs_grant",
        json!({"key": 1, "area": "api:billing", "to": "Not A Handle"}),
    );
    assert!(
        bad_grantee.error().starts_with("`to`: "),
        "{}",
        bad_grantee.text
    );

    let misspelt = mcp.tool("dibs_claim", json!({"key": 1, "area": ["billing"]}));
    assert!(misspelt.error().contains("area"), "{}", misspelt.text);
    let wrong_type = mcp.tool("dibs_inbox", json!({"unread": "yes"}));
    assert!(wrong_type.is_error);

    let unknown = mcp.request(
        "tools/call",
        json!({"name": "dibs_nothing", "arguments": {}}),
    );
    assert!(unknown["error"].is_object() || unknown["result"]["isError"] == true);
}

#[test]
fn a_server_that_cannot_be_reached_is_one_plain_sentence_and_the_tools_still_list() {
    let setup = Setup::new();
    let url = dibs_test_server::unreachable().unwrap();
    let dir = setup.root.path().join("offline");
    make_checkout(&dir, &url, "acme", Some("api"), API_REMOTE);
    let mut command = setup.process_as("bob", &dir);
    command.env("DIBS_TOKEN", &setup.bob.token);
    let mut mcp = Mcp::spawn(command);

    let listed = mcp.request("tools/list", json!({}));
    assert_eq!(
        listed["result"]["tools"].as_array().unwrap().len(),
        TOOLS.len()
    );
    for (tool, arguments) in [
        ("dibs_status", json!({})),
        ("dibs_claim", json!({"key": 1})),
        (
            "dibs_send",
            json!({"to": ["alice"], "subject": "s", "body": "b"}),
        ),
    ] {
        let reply = mcp.tool(tool, arguments);
        let sentence = reply.error();
        assert!(sentence.contains("cannot be reached"), "{tool}: {sentence}");
        assert!(
            sentence.ends_with('.') && !sentence.contains('\n'),
            "{sentence}"
        );
        assert!(
            !sentence.contains("127.0.0.1") && !sentence.contains("http"),
            "{sentence}"
        );
        assert_eq!(reply.structured["error"]["code"], "unreachable");
    }
}

#[test]
fn a_server_that_goes_away_mid_session_is_met_the_same_way_and_a_session_ends_with_its_input() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut mcp = Mcp::start(&setup, "alice", &api);
    assert!(!mcp.tool("dibs_status", json!({})).is_error);

    let Setup { server, .. } = setup;
    drop(server);

    let reply = mcp.tool("dibs_items", json!({}));
    assert!(
        reply.error().contains("cannot be reached"),
        "{}",
        reply.text
    );
    assert_eq!(reply.structured["error"]["code"], "unreachable");
    assert_eq!(mcp.finish(), Some(0));
}

#[test]
fn a_wide_claim_through_the_tools_is_announced_and_its_acknowledgement_grants() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#1", "title": "Invoices"}),
    );
    bob.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#2", "title": "Upgrade"}),
    );
    alice
        .tool(
            "dibs_claim",
            json!({"key": "gh:acme/api#1", "areas": ["billing"]}),
        )
        .ok();
    let wide = bob.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#2",
               "wide": {"areas": ["billing", "search"], "reason": "Upgrade the HTTP client"}}),
    );
    let made = wide.ok();
    assert_eq!(made["claim"]["areas"], json!([]));
    assert_eq!(made["claim"]["wide"]["reason"], "Upgrade the HTTP client");
    assert_eq!(made["announced"][0]["to"], "alice");
    let message = made["announced"][0]["message_id"].as_u64().unwrap();

    let held = bob.tool("dibs_check", json!({"paths": ["src/billing/a.rs"]}));
    assert_eq!(held.ok()["verdicts"][0]["reason"], "awaiting_ack");

    let acked = alice.tool("dibs_ack", json!({"message": message}));
    assert_eq!(acked.ok()["granted"][0]["to"], "bob");
    assert_eq!(acked.ok()["granted"][0]["area"], "api:billing");
    let granted = bob.tool("dibs_check", json!({"paths": ["src/billing/a.rs"]}));
    assert_eq!(granted.ok()["verdicts"][0]["verdict"], "granted");

    let overlap = alice.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#1", "wide": {"areas": ["search"], "reason": "Rename"}}),
    );
    assert!(
        overlap.error().contains("wide claim of bob"),
        "{}",
        overlap.text
    );
    let blank = alice.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#1", "wide": {"areas": ["web:checkout"], "reason": " "}}),
    );
    assert!(
        blank.error().starts_with("`wide.reason`: "),
        "{}",
        blank.error()
    );
}

#[test]
fn a_slot_taken_through_the_tools_is_the_members_and_a_second_member_waits_for_it() {
    let setup = Setup::new();
    let mut settings = dibs_test_server::two_repositories();
    settings["slots"] = json!([{"name": "deploy:prod", "max_hold": "30m"}]);
    setup.server.settings(&setup.alice, settings).unwrap();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);

    let taken = alice.tool(
        "dibs_slot_take",
        json!({"slot": "deploy:prod", "purpose": "ship 2.4"}),
    );
    assert_eq!(taken.ok()["outcome"], "taken");
    assert_eq!(taken.ok()["slot"]["hold"]["holder"], "alice");
    let renewed = setup
        .dibs(&setup.alice, &api)
        .args([
            "--json",
            "slot",
            "take",
            "deploy:prod",
            "--purpose",
            "ship 2.4",
        ])
        .output()
        .unwrap();
    assert_eq!(
        status(&renewed),
        0,
        "the command line's session holds it too"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&renewed.stdout).unwrap()["outcome"],
        "renewed"
    );

    let queued = bob.tool(
        "dibs_slot_take",
        json!({"slot": "deploy:prod", "purpose": "hotfix"}),
    );
    assert_eq!(queued.ok()["outcome"], "queued");
    assert_eq!(queued.ok()["position"], 1);
    let too_long = bob.tool(
        "dibs_slot_take",
        json!({"slot": "deploy:prod", "purpose": "hotfix", "wait_seconds": 600}),
    );
    assert!(too_long.error().contains("540"), "{}", too_long.text);
    let listed = bob.tool("dibs_slots", json!({}));
    assert_eq!(listed.ok()["slots"][0]["queue"][0]["member"], "bob");

    let released = alice.tool("dibs_slot_release", json!({"slot": "deploy:prod"}));
    assert_eq!(released.ok()["slot"]["offer"]["member"], "bob");
    let mine = bob.tool(
        "dibs_slot_take",
        json!({"slot": "deploy:prod", "purpose": "hotfix", "wait_seconds": 5}),
    );
    assert_eq!(mine.ok()["outcome"], "taken");
    let not_waiting = alice.tool("dibs_slot_leave", json!({"slot": "deploy:prod"}));
    assert!(
        not_waiting.error().contains("not waiting"),
        "{}",
        not_waiting.text
    );
    let unknown = alice.tool("dibs_slot_release", json!({"slot": "Bad Name"}));
    assert!(unknown.error().contains("`slot`"), "{}", unknown.text);
}

#[test]
fn a_debt_claim_through_the_tools_hands_its_area_over_by_narrowing() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let mut alice = Mcp::start(&setup, "alice", &api);
    let mut bob = Mcp::start(&setup, "bob", &api);
    alice.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#7", "title": "Tidy billing"}),
    );
    bob.tool(
        "dibs_new",
        json!({"ref": "gh:acme/api#42", "title": "Currency"}),
    );
    let debt = alice.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#7", "areas": ["billing"], "debt": true}),
    );
    assert_eq!(debt.ok()["claim"]["kind"], "debt");
    let waiting = bob.tool(
        "dibs_claim",
        json!({"key": "gh:acme/api#42", "areas": ["billing"]}),
    );
    let made = waiting.ok();
    assert_eq!(made["pending"][0]["area"], "api:billing");
    assert_eq!(made["pending"][0]["holder"], "alice");
    let held = bob.tool("dibs_check", json!({"paths": ["src/billing/a.rs"]}));
    assert_eq!(held.ok()["verdicts"][0]["reason"], "awaiting_handover");

    let narrowed = alice.tool(
        "dibs_narrow",
        json!({"key": "gh:acme/api#7", "areas": ["billing"]}),
    );
    let answer = narrowed.ok();
    assert_eq!(answer["areas"], json!(["api:billing"]));
    assert_eq!(answer["handed_over"][0]["holder"], "bob");
    let free = bob.tool("dibs_check", json!({"paths": ["src/billing/a.rs"]}));
    assert_eq!(free.ok()["verdicts"][0]["verdict"], "allowed");
    let not_theirs = bob.tool(
        "dibs_narrow",
        json!({"key": "gh:acme/api#7", "areas": ["billing"]}),
    );
    assert!(not_theirs.is_error, "{}", not_theirs.text);
    setup.server.advance(time::Duration::hours(2));
    let taken = bob.tool(
        "dibs_takeover",
        json!({"key": "gh:acme/api#7", "reason": "alice is away", "kind": "feature"}),
    );
    assert_eq!(taken.ok()["claim"]["kind"], "feature");
}
