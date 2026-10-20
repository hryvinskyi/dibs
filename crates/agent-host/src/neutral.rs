//! The `neutral` host: its native format is the guard's own, as JSON, so a host that runs plugins
//! in-process needs no adapter of its own, only a small shim that calls `dibs hook neutral
//! <event>` and acts on the reply.
//!
//! Protocol 1. The event is the command's argument: `session-started`, `before-tool`,
//! `after-tool` or `prompt-submitted`. The input on standard input is one JSON object:
//!
//! ```json
//! {"protocol": 1, "session": "s-1", "cwd": "/work/api", "thread": "main",
//!  "tool": {"kind": "edit", "paths": ["src/billing/a.rs"]}}
//! ```
//!
//! `thread` (`main`, `subagent` or `unknown`, the default) belongs to `before-tool`, `after-tool`
//! and `prompt-submitted`; `tool` to `before-tool`, where it is required: `{"kind": "edit", "paths": [...]}`,
//! `{"kind": "shell", "command": "..."}`, or any other kind, which is a tool the guard has no say
//! about. The reply on standard output is one JSON object, and the exit status is always `0`:
//!
//! ```json
//! {"protocol": 1, "decision": "no_objection", "context": "...", "notice": "..."}
//! {"protocol": 1, "decision": "deny", "reason": "..."}
//! ```
//!
//! `context` and `notice` are left out when there is none. Within protocol 1 a field may be added
//! to either side and none is renamed or removed, so a reader ignores fields it does not know.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::{HostAdapter, HostId, HostReply, ParseError};
use crate::capabilities::Capabilities;
use crate::event::{HostEvent, SessionKey, Thread, ToolCall};
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
        Ok(match name {
            EventName::SessionStarted => HostEvent::SessionStarted { session_key, cwd },
            EventName::BeforeTool => HostEvent::BeforeTool {
                session_key,
                cwd,
                call: tool(input.tool)?,
                thread,
            },
            EventName::AfterTool => HostEvent::AfterTool {
                session_key,
                cwd,
                thread,
            },
            EventName::PromptSubmitted => HostEvent::PromptSubmitted {
                session_key,
                cwd,
                thread,
            },
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
        };
        let mut stdout = serde_json::to_vec(&reply).unwrap_or_default();
        stdout.push(b'\n');
        HostReply { stdout, exit: 0 }
    }
}

/// The events of protocol 1, by the name the command takes.
#[derive(Debug, Clone, Copy)]
enum EventName {
    SessionStarted,
    BeforeTool,
    AfterTool,
    PromptSubmitted,
}

impl EventName {
    fn from_argument(name: &str) -> Option<Self> {
        match name {
            "session-started" => Some(Self::SessionStarted),
            "before-tool" => Some(Self::BeforeTool),
            "after-tool" => Some(Self::AfterTool),
            "prompt-submitted" => Some(Self::PromptSubmitted),
            _ => None,
        }
    }
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
            },
        ),
        EventFixture::parses(
            "after-tool",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","thread":"unknown"}"#,
            HostEvent::AfterTool {
                session_key: key("s-1"),
                cwd: PathBuf::from("/work"),
                thread: Thread::Unknown,
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
            },
        ),
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
