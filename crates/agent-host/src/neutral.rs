//! The `neutral` host: its native format is the guard's own, as JSON, so a host that runs plugins
//! in-process needs no adapter of its own, only a small shim that calls `dibs hook neutral
//! <event>` and acts on the reply.
//!
//! Protocol 1. The event is the command's argument: `session-started`, `before-tool`,
//! `after-tool`, `prompt-submitted`, `stopped` (the main agent finished its turn and the session
//! waits for its person), `worktree-wanted`, `worktree-returned`, `agent-ended` (an agent the main
//! agent started finished) or `session-ended`. The input on standard input is one JSON object:
//!
//! ```json
//! {"protocol": 1, "session": "s-1", "cwd": "/work/api", "thread": "main",
//!  "tool": {"kind": "edit", "paths": ["src/billing/a.rs"]}}
//! ```
//!
//! `thread` (`main`, `subagent` or `unknown`, the default) belongs to `before-tool`, `after-tool`
//! and `prompt-submitted`; `tool` to `before-tool`, where it is required: `{"kind": "edit", "paths": [...]}`,
//! `{"kind": "shell", "command": "..."}`, or any other kind, which is a tool the guard has no say
//! about. `agent`, the host's key of the agent the event comes from, belongs to `before-tool`,
//! `after-tool` and `worktree-wanted`, and is required by `agent-ended`; `label`, the name of the
//! worktree wanted, is required by `worktree-wanted`; `path`, the worktree's directory, by
//! `worktree-returned`. The reply on standard output is one JSON object, and the exit status is
//! always `0`:
//!
//! ```json
//! {"protocol": 1, "decision": "no_objection", "context": "...", "notice": "..."}
//! {"protocol": 1, "decision": "deny", "reason": "..."}
//! ```
//!
//! `context` and `notice` are left out when there is none. Within protocol 1 a field may be added
//! to either side and none is renamed or removed, so a reader ignores fields it does not know.
//!
//! This host does not serve worktrees: a worktree it wants is no business of the guard's, which
//! answers it with no objection, and a worktree it returns is left as it is.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::{HostAdapter, HostId, HostReply, ParseError};
use crate::capabilities::Capabilities;
use crate::event::{AgentKey, HostEvent, SessionKey, Thread, ToolCall, WorktreeLabel};
use crate::outcome::GuardOutcome;
use crate::text::Text;

/// The protocol version this adapter reads and writes.
pub const PROTOCOL: u64 = 1;

/// The `neutral` host.
#[derive(Debug, Clone, Copy, Default)]
pub struct Neutral;

impl Neutral {
    /// The host's id.
    pub const ID: HostId = HostId::new("neutral");
}

#[derive(Deserialize)]
struct Input {
    protocol: u64,
    session: String,
    cwd: String,
    #[serde(default)]
    thread: Option<String>,
    #[serde(default)]
    tool: Option<Value>,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
struct EditTool {
    paths: Vec<String>,
}

#[derive(Deserialize)]
struct ShellTool {
    command: String,
}

#[derive(Serialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
enum Reply<'a> {
    NoObjection {
        protocol: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<&'a Text>,
        #[serde(skip_serializing_if = "Option::is_none")]
        notice: Option<&'a Text>,
    },
    Deny {
        protocol: u64,
        reason: &'a Text,
    },
}

impl HostAdapter for Neutral {
    fn id(&self) -> HostId {
        Self::ID
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            deny_edit: true,
            deny_shell: true,
            context_at_start: true,
            context_after_tool: true,
            context_at_prompt: true,
            tells_subagent: true,
            push_to_idle: false,
            serves_worktrees: false,
        }
    }

    fn parse(&self, event: &str, input: &[u8]) -> Result<HostEvent, ParseError> {
        let name = EventName::from_argument(event)
            .ok_or_else(|| ParseError::UnknownEvent(event.to_owned()))?;
        let input: Input = serde_json::from_slice(input).map_err(|error| {
            ParseError::Malformed(format!("not a protocol {PROTOCOL} object ({error})"))
        })?;
        if input.protocol != PROTOCOL {
            return Err(ParseError::UnsupportedProtocol(input.protocol));
        }
        let session_key = SessionKey::parse(&input.session).ok_or_else(|| {
            ParseError::Malformed(
                "session is 1 to 256 characters with no control characters".to_owned(),
            )
        })?;
        if input.cwd.is_empty() {
            return Err(ParseError::Malformed("cwd is empty".to_owned()));
        }
        let cwd = PathBuf::from(input.cwd);
        let thread = thread(input.thread.as_deref())?;
        let agent = agent(input.agent.as_deref())?;
        Ok(match name {
            EventName::SessionStarted => HostEvent::SessionStarted { session_key, cwd },
            EventName::BeforeTool => HostEvent::BeforeTool {
                session_key,
                cwd,
                call: tool(input.tool)?,
                thread,
                agent,
            },
            EventName::AfterTool => HostEvent::AfterTool {
                session_key,
                cwd,
                thread,
                agent,
            },
            EventName::PromptSubmitted => HostEvent::PromptSubmitted {
                session_key,
                cwd,
                thread,
            },
            EventName::Stopped => HostEvent::Stopped { session_key, cwd },
            EventName::WorktreeWanted => HostEvent::WorktreeWanted {
                session_key,
                cwd,
                label: label(input.label.as_deref())?,
                agent,
            },
            EventName::WorktreeReturned => HostEvent::WorktreeReturned {
                session_key,
                cwd,
                path: input
                    .path
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .ok_or_else(|| {
                        ParseError::Malformed("worktree-returned needs a path".to_owned())
                    })?,
            },
            EventName::AgentEnded => HostEvent::AgentEnded {
                session_key,
                cwd,
                agent: agent.ok_or_else(|| {
                    ParseError::Malformed("agent-ended needs an agent".to_owned())
                })?,
            },
            EventName::SessionEnded => HostEvent::SessionEnded { session_key, cwd },
        })
    }

    fn render(&self, _event: &str, outcome: &GuardOutcome) -> HostReply {
        let reply = match outcome {
            GuardOutcome::NoObjection { context, notice } => Reply::NoObjection {
                protocol: PROTOCOL,
                context: context.as_ref(),
                notice: notice.as_ref(),
            },
            GuardOutcome::Deny { reason } => Reply::Deny {
                protocol: PROTOCOL,
                reason,
            },
            GuardOutcome::Worktree { notice, .. } => Reply::NoObjection {
                protocol: PROTOCOL,
                context: None,
                notice: notice.as_ref(),
            },
            GuardOutcome::NoWorktree { reason } => Reply::NoObjection {
                protocol: PROTOCOL,
                context: None,
                notice: Some(reason),
            },
        };
        let mut stdout = serde_json::to_vec(&reply).unwrap_or_default();
        stdout.push(b'\n');
        HostReply::stdout(stdout)
    }
}

/// The events of protocol 1, by the name the command takes.
#[derive(Debug, Clone, Copy)]
enum EventName {
    SessionStarted,
    BeforeTool,
    AfterTool,
    PromptSubmitted,
    Stopped,
    WorktreeWanted,
    WorktreeReturned,
    AgentEnded,
    SessionEnded,
}

impl EventName {
    fn from_argument(name: &str) -> Option<Self> {
        match name {
            "session-started" => Some(Self::SessionStarted),
            "before-tool" => Some(Self::BeforeTool),
            "after-tool" => Some(Self::AfterTool),
            "prompt-submitted" => Some(Self::PromptSubmitted),
            "stopped" => Some(Self::Stopped),
            "worktree-wanted" => Some(Self::WorktreeWanted),
            "worktree-returned" => Some(Self::WorktreeReturned),
            "agent-ended" => Some(Self::AgentEnded),
            "session-ended" => Some(Self::SessionEnded),
            _ => None,
        }
    }
}

fn agent(value: Option<&str>) -> Result<Option<AgentKey>, ParseError> {
    value
        .map(|key| {
            AgentKey::parse(key).map_err(|_| {
                ParseError::Malformed(
                    "agent is 1 to 64 characters with no spaces or control characters".to_owned(),
                )
            })
        })
        .transpose()
}

fn label(value: Option<&str>) -> Result<WorktreeLabel, ParseError> {
    value.and_then(WorktreeLabel::parse).ok_or_else(|| {
        ParseError::Malformed(
            "label is 1 to 64 letters, digits, '.', '_' or '-', starting with a letter or a digit"
                .to_owned(),
        )
    })
}

fn thread(value: Option<&str>) -> Result<Thread, ParseError> {
    match value {
        None | Some("unknown") => Ok(Thread::Unknown),
        Some("main") => Ok(Thread::Main),
        Some("subagent") => Ok(Thread::Subagent),
        Some(other) => Err(ParseError::Malformed(format!(
            "thread is main, subagent or unknown, not '{other}'"
        ))),
    }
}

fn tool(value: Option<Value>) -> Result<ToolCall, ParseError> {
    let value =
        value.ok_or_else(|| ParseError::Malformed("before-tool needs a tool".to_owned()))?;
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| ParseError::Malformed("tool.kind is missing".to_owned()))?;
    let malformed = |error: serde_json::Error| ParseError::Malformed(format!("tool: {error}"));
    match kind {
        "edit" => {
            let edit: EditTool = serde_json::from_value(value).map_err(malformed)?;
            Ok(ToolCall::Edit {
                paths: edit.paths.into_iter().map(PathBuf::from).collect(),
            })
        }
        "shell" => {
            let shell: ShellTool = serde_json::from_value(value).map_err(malformed)?;
            Ok(ToolCall::Shell {
                command: shell.command,
            })
        }
        _ => Ok(ToolCall::Other),
    }
}

/// The fixtures the conformance kit checks this adapter against.
#[cfg(any(test, feature = "conformance"))]
pub fn fixtures() -> crate::conformance::Fixtures {
    use crate::conformance::{EventFixture, Fixtures, ReplyFixture};

    let key = SessionKey::fixture;
    let Ok(a1b2) = AgentKey::parse("a1b2") else {
        return Fixtures::default();
    };
    let events = vec![
        EventFixture::parses(
            "session-started",
            br#"{"protocol":1,"session":"s-1","cwd":"/work/api"}"#,
            HostEvent::SessionStarted {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work/api"),
            },
        ),
        EventFixture::parses(
            "before-tool",
            br#"{"protocol":1,"session":"s-1","cwd":"/work/api","thread":"main","tool":{"kind":"edit","paths":["src/a.rs","C:\\work\\api\\b.rs"]},"later":"ignored"}"#,
            HostEvent::BeforeTool {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work/api"),
                call: ToolCall::Edit {
                    paths: vec![PathBuf::from("src/a.rs"), PathBuf::from("C:\\work\\api\\b.rs")],
                },
                thread: Thread::Main,
                agent: None,
            },
        ),
        EventFixture::parses(
            "before-tool",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","thread":"subagent","tool":{"kind":"shell","command":"make release"}}"#,
            HostEvent::BeforeTool {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                call: ToolCall::Shell {
                    command: "make release".to_owned(),
                },
                thread: Thread::Subagent,
                agent: None,
            },
        ),
        EventFixture::parses(
            "before-tool",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","tool":{"kind":"search","query":"x"}}"#,
            HostEvent::BeforeTool {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                call: ToolCall::Other,
                thread: Thread::Unknown,
                agent: None,
            },
        ),
        EventFixture::parses(
            "after-tool",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","thread":"unknown"}"#,
            HostEvent::AfterTool {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                thread: Thread::Unknown,
                agent: None,
            },
        ),
        EventFixture::parses(
            "prompt-submitted",
            br#"{"protocol":1,"session":"s-1","cwd":"/work"}"#,
            HostEvent::PromptSubmitted {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                thread: Thread::Unknown,
            },
        ),
        EventFixture::parses(
            "stopped",
            br#"{"protocol":1,"session":"s-1","cwd":"/work"}"#,
            HostEvent::Stopped {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
            },
        ),
        EventFixture::parses(
            "prompt-submitted",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","thread":"main"}"#,
            HostEvent::PromptSubmitted {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                thread: Thread::Main,
            },
        ),
        EventFixture::parses(
            "before-tool",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","thread":"main","tool":{"kind":"shell","command":"git push origin main"}}"#,
            HostEvent::BeforeTool {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                call: ToolCall::Shell {
                    command: "git push origin main".to_owned(),
                },
                thread: Thread::Main,
                agent: None,
            },
        ),
        EventFixture::parses(
            "after-tool",
            br#"{"protocol":1,"session":"s-1","cwd":"/work/pool/w2","thread":"subagent","agent":"a1b2"}"#,
            HostEvent::AfterTool {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work/pool/w2"),
                thread: Thread::Subagent,
                agent: Some(a1b2.clone()),
            },
        ),
        EventFixture::parses(
            "worktree-wanted",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","label":"agent-a1b2","agent":"a1b2"}"#,
            HostEvent::WorktreeWanted {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                label: WorktreeLabel::fixture("agent-a1b2"),
                agent: Some(a1b2.clone()),
            },
        ),
        EventFixture::parses(
            "worktree-returned",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","path":"/work/pool/w2"}"#,
            HostEvent::WorktreeReturned {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                path: PathBuf::from("/work/pool/w2"),
            },
        ),
        EventFixture::parses(
            "agent-ended",
            br#"{"protocol":1,"session":"s-1","cwd":"/work/pool/w2","agent":"a1b2"}"#,
            HostEvent::AgentEnded {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work/pool/w2"),
                agent: a1b2.clone(),
            },
        ),
        EventFixture::parses(
            "session-ended",
            br#"{"protocol":1,"session":"s-1","cwd":"/work"}"#,
            HostEvent::SessionEnded {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
            },
        ),
        EventFixture::refuses("worktree-wanted", br#"{"protocol":1,"session":"s-1","cwd":"/work"}"#),
        EventFixture::refuses("worktree-wanted", br#"{"protocol":1,"session":"s-1","cwd":"/work","label":"../up"}"#),
        EventFixture::refuses("worktree-returned", br#"{"protocol":1,"session":"s-1","cwd":"/work"}"#),
        EventFixture::refuses("agent-ended", br#"{"protocol":1,"session":"s-1","cwd":"/work"}"#),
        EventFixture::refuses("after-tool", br#"{"protocol":1,"session":"s-1","cwd":"/work","agent":"a b"}"#),
        EventFixture::refuses(
            "before-tool",
            br#"{"protocol":2,"session":"s-1","cwd":"/work","tool":{"kind":"other"}}"#,
        ),
        EventFixture::refuses("before-tool", br#"{"protocol":1,"session":"s-1","cwd":"/work"}"#),
        EventFixture::refuses("after-tool", br#"{"protocol":1,"session":"","cwd":"/work"}"#),
        EventFixture::refuses("after-tool", br#"{"protocol":1,"session":"s","cwd":"/w","thread":"side"}"#),
        EventFixture::refuses("pre-tool-use", br#"{"protocol":1,"session":"s","cwd":"/w"}"#),
    ];
    let replies = vec![
        ReplyFixture::outcome(GuardOutcome::silent()).allows(),
        ReplyFixture::outcome(GuardOutcome::context("the brief"))
            .allows()
            .carries_context("the brief"),
        ReplyFixture::outcome(GuardOutcome::context("dibs delivers, while you work:"))
            .on("after-tool")
            .allows()
            .carries_context("dibs delivers, while you work:"),
        ReplyFixture::outcome(GuardOutcome::Deny {
            reason: Text::new("This command needs the slot deploy:prod"),
        })
        .on("before-tool")
        .denies("This command needs the slot deploy:prod"),
        ReplyFixture::outcome(GuardOutcome::notice("server away"))
            .allows()
            .carries_notice("server away"),
        ReplyFixture::outcome(GuardOutcome::Deny {
            reason: Text::new("held by alice"),
        })
        .denies("held by alice"),
        ReplyFixture::outcome(GuardOutcome::NoWorktree {
            reason: Text::new("every worktree is held"),
        })
        .on("worktree-wanted")
        .allows()
        .carries_notice("every worktree is held"),
    ];
    Fixtures { events, replies }
}

/// Reads the neutral reply back, for the conformance kit.
#[cfg(any(test, feature = "conformance"))]
pub struct NeutralReader;

#[cfg(any(test, feature = "conformance"))]
impl crate::conformance::ReplyReader for NeutralReader {
    fn read(&self, reply: &HostReply) -> Option<crate::conformance::ReadReply> {
        let value: Value = serde_json::from_slice(&reply.stdout).ok()?;
        let text = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_owned);
        if value.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL) {
            return None;
        }
        Some(crate::conformance::ReadReply {
            denies: text("decision").as_deref() == Some("deny"),
            reason: text("reason"),
            context: text("context"),
            notice: text("notice"),
        })
    }
}
