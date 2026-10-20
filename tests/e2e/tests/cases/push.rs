//! A session waiting for its person, woken across the built binaries: the member's tool server,
//! started for a host that can wake a session and opted in, holds the claim on an item, and the
//! session's hooks report its turn ended; a teammate's urgent message about that item reaches it
//! once, as one notification, and another session of the same member, whose hooks run between
//! tool calls, never shows it.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::harness::{Person, Team};

const SETTINGS: &str = r#"
[[areas]]
name = "billing"
paths = ["src/billing/**"]
"#;

const WAIT: Duration = Duration::from_secs(30);

/// A tool server of one member, with what it wrote on standard output so far.
struct ToolServer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
    next_id: u64,
    notifications: Vec<Value>,
}

impl ToolServer {
    fn start(person: &Person, cwd: &Path, host: &Pushing) -> (Self, Value) {
        let mut command = person.dibs(cwd);
        command
            .args(["mcp", "--host", &host.id])
            .env("DIBS_PUSH_TO_IDLE", "1");
        if let Some(var) = &host.session_var {
            command.env(var, AGENT_SESSION);
        }
        let mut child = command
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
                let value: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            stdin,
            lines,
            next_id: 0,
            notifications: Vec::new(),
        };
        let handshake = server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0.0.1"}
            }),
        );
        server.write(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        (server, handshake)
    }

    fn write(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + WAIT;
        loop {
            let message = self
                .next(deadline)
                .unwrap_or_else(|| panic!("no answer to {method}"));
            if message["id"] == json!(id) {
                return message;
            }
            self.notifications.push(message);
        }
    }

    /// Collects notifications until `deadline`.
    fn notified_until(&mut self, deadline: Instant) -> &[Value] {
        while let Some(message) = self.next(deadline) {
            self.notifications.push(message);
        }
        &self.notifications
    }

    fn next(&self, deadline: Instant) -> Option<Value> {
        match self
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(value) => Some(value),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("the tool server ended"),
        }
    }
}

impl Drop for ToolServer {
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

/// The host's id of the agent's session.
const AGENT_SESSION: &str = "s-agent";

/// A host this binary links that can wake a session waiting for its person: its id, and the
/// variable through which it names the session it started the tool server for.
struct Pushing {
    id: String,
    session_var: Option<String>,
}

fn pushing_host(person: &Person, cwd: &Path) -> Pushing {
    let (_, doctor) = person.json(cwd, &["doctor"]);
    let host = doctor["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|host| host["capabilities"]["push_to_idle"] == true)
        .unwrap_or_else(|| panic!("no linked host can wake a session: {doctor}"));
    Pushing {
        id: host["id"].as_str().unwrap().to_owned(),
        session_var: host["push"]["session_var"].as_str().map(str::to_owned),
    }
}

/// The agent's session ends its turn and waits for its person, as the host's hooks report it.
fn turn_ends(person: &Person, cwd: &Path, host: &Pushing) {
    let input = json!({"session_id": AGENT_SESSION, "cwd": cwd});
    let mut hook = person
        .dibs(cwd)
        .args(["hook", &host.id, "stop"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(hook.stdin.take().unwrap(), "{input}").unwrap();
    let out = hook.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty(), "the end of a turn answers nothing");
}

#[test]
fn a_message_about_the_item_a_waiting_session_works_on_wakes_it_once() {
    let team = Team::start(SETTINGS);
    let work = team.checkout("work", None, "git@example.com:acme/widgets.git");
    team.alice.ok(
        &work,
        &["new", "gh:acme/widgets#42", "--title", "Rework billing"],
    );
    let host = pushing_host(&team.bob, &work);
    let (mut agent, handshake) = ToolServer::start(&team.bob, &work, &host);
    assert!(
        handshake["result"]["capabilities"]["experimental"].is_object(),
        "{handshake}"
    );
    let claimed = agent.request(
        "tools/call",
        json!({"name": "dibs_claim", "arguments": {"key": "gh:acme/widgets#42", "areas": ["billing"]}}),
    );
    assert_ne!(claimed["result"]["isError"], true, "{claimed}");
    turn_ends(&team.bob, &work, &host);

    team.alice.ok(
        &work,
        &[
            "send",
            "--to",
            "bob",
            "--item",
            "gh:acme/widgets#42",
            "--subject",
            "Billing schema",
            "--importance",
            "urgent",
            "Rebase on main before you migrate.",
        ],
    );
    team.bob.ok(
        &work,
        &[
            "sync",
            "--force",
            "--host",
            "neutral",
            "--session",
            "s-hooks",
        ],
    );
    let shown = after_tool(&team.bob, &work, "s-hooks");
    assert!(
        shown.get("context").is_none(),
        "another session of the member: {shown}"
    );

    let deadline = Instant::now() + WAIT;
    while agent.notifications.is_empty() && Instant::now() < deadline {
        agent.notified_until(Instant::now() + Duration::from_millis(200));
    }
    let pushed = agent
        .notified_until(Instant::now() + Duration::from_secs(2))
        .to_vec();
    assert_eq!(pushed.len(), 1, "{pushed:?}");
    let text = pushed[0].to_string();
    assert!(
        text.contains("Rebase on main before you migrate."),
        "{text}"
    );
    assert!(text.contains("quoted as data"), "{text}");
}

/// The reply of `dibs hook neutral after-tool` on the main thread of `session`.
fn after_tool(person: &Person, cwd: &Path, session: &str) -> Value {
    let input = json!({"protocol": 1, "session": session, "cwd": cwd, "thread": "main"});
    let mut hook = person
        .dibs(cwd)
        .args(["hook", "neutral", "after-tool"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(hook.stdin.take().unwrap(), "{input}").unwrap();
    let out = hook.wait_with_output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}
