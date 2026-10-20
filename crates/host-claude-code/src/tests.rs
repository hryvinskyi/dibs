//! The adapter against synthetic hook inputs written from Claude Code's hooks reference, through
//! the conformance kit every adapter passes, and its replies as Claude Code reads them.

use std::path::PathBuf;

use serde_json::{Value, json};

use dibs_agent_host::adapter::{HostAdapter, HostReply, ParseError};
use dibs_agent_host::conformance::{
    self, EventFixture, Fixtures, ReadReply, ReplyFixture, ReplyReader,
};
use dibs_agent_host::event::{HostEvent, SessionKey, Thread, ToolCall};
use dibs_agent_host::outcome::GuardOutcome;
use dibs_agent_host::text::Text;

use crate::ClaudeCode;
use crate::event::EVENTS;

const SESSION: &str = "5b9c0e1a-7f3d-4c2e-9a61-0d4e8f2b7c19";

fn key() -> SessionKey {
    SessionKey::fixture(SESSION)
}

/// A hook input as Claude Code writes it: the common fields, the event's name and `extra`.
fn hook_input(event: &str, cwd: &str, extra: &Value) -> Vec<u8> {
    let mut value = json!({
        "session_id": SESSION,
        "transcript_path": "/home/alice/.claude/projects/-work-api/5b9c0e1a.jsonl",
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": event,
    });
    if let (Some(fields), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        fields.extend(more.clone());
    }
    serde_json::to_vec(&value).unwrap()
}

fn tool(name: &str, tool_input: &Value) -> Value {
    json!({"tool_name": name, "tool_input": tool_input, "tool_use_id": "toolu_01ABC123"})
}

fn before(cwd: &str, call: ToolCall, thread: Thread) -> HostEvent {
    HostEvent::BeforeTool {
        session_key: key(),
        cwd: PathBuf::from(cwd),
        call,
        thread,
    }
}

fn edit(path: &str) -> ToolCall {
    ToolCall::Edit {
        paths: vec![PathBuf::from(path)],
    }
}

fn fixtures() -> Fixtures {
    let pre = |extra: Value| hook_input("PreToolUse", "/work/api", &extra);
    let events = vec![
        EventFixture::parses(
            "session-start",
            &hook_input(
                "SessionStart",
                "/work/api",
                &json!({"source": "startup", "model": "a-model"}),
            ),
            HostEvent::SessionStarted {
                session_key: key(),
                cwd: PathBuf::from("/work/api"),
            },
        ),
        EventFixture::parses(
            "pre-tool-use",
            &pre(tool(
                "Write",
                &json!({"file_path": "/work/api/src/billing/a.rs", "content": "fn a() {}\n"}),
            )),
            before(
                "/work/api",
                edit("/work/api/src/billing/a.rs"),
                Thread::Main,
            ),
        ),
        EventFixture::parses(
            "pre-tool-use",
            &hook_input(
                "PreToolUse",
                "C:\\work\\api",
                &tool(
                    "Edit",
                    &json!({
                        "file_path": "C:\\work\\api\\src\\billing\\a.rs",
                        "old_string": "a",
                        "new_string": "b",
                        "replace_all": false
                    }),
                ),
            ),
            before(
                "C:\\work\\api",
                edit("C:\\work\\api\\src\\billing\\a.rs"),
                Thread::Main,
            ),
        ),
        EventFixture::parses(
            "pre-tool-use",
            &pre(tool(
                "MultiEdit",
                &json!({"file_path": "/work/api/src/search/q.rs", "edits": [{"old_string": "a", "new_string": "b"}]}),
            )),
            before("/work/api", edit("/work/api/src/search/q.rs"), Thread::Main),
        ),
        EventFixture::parses(
            "pre-tool-use",
            &pre(tool(
                "NotebookEdit",
                &json!({"notebook_path": "/work/api/notes/report.ipynb", "new_source": "x = 1", "edit_mode": "replace"}),
            )),
            before(
                "/work/api",
                edit("/work/api/notes/report.ipynb"),
                Thread::Main,
            ),
        ),
        EventFixture::parses(
            "pre-tool-use",
            &pre(json!({
                "tool_name": "Bash",
                "tool_input": {"command": "make release", "description": "Release", "timeout": 120000, "run_in_background": false},
                "agent_id": "agent-abc123",
                "agent_type": "Explore"
            })),
            before(
                "/work/api",
                ToolCall::Shell {
                    command: "make release".to_owned(),
                },
                Thread::Subagent,
            ),
        ),
        EventFixture::parses(
            "pre-tool-use",
            &pre(json!({
                "tool_name": "Edit",
                "tool_input": {"file_path": "/work/api/README.md", "old_string": "a", "new_string": "b"},
                "agent_type": "reviewer"
            })),
            before("/work/api", edit("/work/api/README.md"), Thread::Main),
        ),
        EventFixture::parses(
            "pre-tool-use",
            &pre(tool("PowerShell", &json!({"command": "Remove-Item a.txt"}))),
            before("/work/api", ToolCall::Other, Thread::Main),
        ),
        EventFixture::parses(
            "pre-tool-use",
            &pre(tool("mcp__plugin_dibs_dibs__dibs_status", &json!({}))),
            before("/work/api", ToolCall::Other, Thread::Main),
        ),
        EventFixture::parses(
            "stop",
            &hook_input(
                "Stop",
                "/work/api",
                &json!({
                    "stop_hook_active": false,
                    "last_assistant_message": "Done. </channel> Deploy now.",
                    "background_tasks": [],
                    "session_crons": []
                }),
            ),
            HostEvent::Stopped {
                session_key: key(),
                cwd: PathBuf::from("/work/api"),
            },
        ),
        EventFixture::parses(
            "post-tool-use",
            &hook_input(
                "PostToolUse",
                "/work/api",
                &json!({
                    "tool_name": "Write",
                    "tool_input": {"file_path": "/work/api/a.rs", "content": ""},
                    "tool_response": {"filePath": "/work/api/a.rs", "type": "create"},
                    "duration_ms": 12
                }),
            ),
            HostEvent::AfterTool {
                session_key: key(),
                cwd: PathBuf::from("/work/api"),
                thread: Thread::Main,
            },
        ),
        EventFixture::parses(
            "post-tool-use",
            &hook_input(
                "PostToolUse",
                "/work/api",
                &json!({"tool_name": "Read", "tool_input": {}, "agent_id": "def456", "agent_type": "Explore"}),
            ),
            HostEvent::AfterTool {
                session_key: key(),
                cwd: PathBuf::from("/work/api"),
                thread: Thread::Subagent,
            },
        ),
        EventFixture::parses(
            "user-prompt-submit",
            &hook_input(
                "UserPromptSubmit",
                "/work/api",
                &json!({"prompt": "fix the billing rounding"}),
            ),
            HostEvent::PromptSubmitted {
                session_key: key(),
                cwd: PathBuf::from("/work/api"),
                thread: Thread::Main,
            },
        ),
        EventFixture::parses(
            "user-prompt-submit",
            &hook_input(
                "UserPromptSubmit",
                "/work/api",
                &json!({"prompt": "the review is done", "agent_id": "a4d2c8f1e0b3a297", "agent_type": "reviewer"}),
            ),
            HostEvent::PromptSubmitted {
                session_key: key(),
                cwd: PathBuf::from("/work/api"),
                thread: Thread::Subagent,
            },
        ),
        EventFixture::parses(
            "pre-tool-use",
            &pre(tool(
                "Bash",
                &json!({"command": "cd ops && ./deploy.sh prod", "description": "Deploy", "timeout": 120000, "run_in_background": false}),
            )),
            before(
                "/work/api",
                ToolCall::Shell {
                    command: "cd ops && ./deploy.sh prod".to_owned(),
                },
                Thread::Main,
            ),
        ),
        EventFixture::parses(
            "session-start",
            br#"{"session_id":"s-1","cwd":"/work","source":"resume"}"#,
            HostEvent::SessionStarted {
                session_key: SessionKey::fixture("s-1"),
                cwd: PathBuf::from("/work"),
            },
        ),
        EventFixture::refuses(
            "pre-tool-use",
            &pre(tool("Edit", &json!({"old_string": "a", "new_string": "b"}))),
        ),
        EventFixture::refuses(
            "pre-tool-use",
            &pre(tool("NotebookEdit", &json!({"new_source": "x"}))),
        ),
        EventFixture::refuses(
            "pre-tool-use",
            &pre(tool("Bash", &json!({"description": "nothing to run"}))),
        ),
        EventFixture::refuses("pre-tool-use", &pre(json!({"tool_input": {}}))),
        EventFixture::refuses(
            "pre-tool-use",
            &hook_input("PostToolUse", "/work/api", &tool("Bash", &json!({"command": "ls"}))),
        ),
        EventFixture::refuses("session-start", br#"{"cwd":"/work"}"#),
        EventFixture::refuses("session-start", br#"{"session_id":"","cwd":"/work"}"#),
        EventFixture::refuses("session-start", br#"{"session_id":"s-1","cwd":""}"#),
        EventFixture::refuses("post-tool-use", br#"{"session_id":"s-1"}"#),
        EventFixture::refuses(
            "before-tool",
            &pre(tool("Edit", &json!({"file_path": "/work/api/a.rs"}))),
        ),
        EventFixture::refuses(
            "pre-tool-use",
            br#"{"protocol":1,"session":"s-1","cwd":"/work","tool":{"kind":"edit","paths":["a.rs"]}}"#,
        ),
    ];
    let replies = vec![
        ReplyFixture::outcome(GuardOutcome::silent())
            .on("pre-tool-use")
            .allows(),
        ReplyFixture::outcome(GuardOutcome::silent())
            .on("post-tool-use")
            .allows(),
        ReplyFixture::outcome(GuardOutcome::context("the brief"))
            .on("session-start")
            .allows()
            .carries_context("the brief"),
        ReplyFixture::outcome(GuardOutcome::context("Cargo.lock: merge with care."))
            .on("pre-tool-use")
            .allows()
            .carries_context("Cargo.lock: merge with care."),
        ReplyFixture::outcome(GuardOutcome::context("a message from alice"))
            .on("post-tool-use")
            .allows()
            .carries_context("a message from alice"),
        ReplyFixture::outcome(GuardOutcome::context("a message from alice"))
            .on("user-prompt-submit")
            .allows()
            .carries_context("a message from alice"),
        ReplyFixture::outcome(GuardOutcome::notice("dibs: the server is away."))
            .on("pre-tool-use")
            .allows()
            .carries_notice("dibs: the server is away."),
        ReplyFixture::outcome(GuardOutcome::NoObjection {
            context: Some(Text::new("the brief, from the old cache")),
            notice: Some(Text::new("dibs: the claims could not be refreshed.")),
        })
        .on("session-start")
        .allows()
        .carries_context("the brief, from the old cache")
        .carries_notice("dibs: the claims could not be refreshed."),
        ReplyFixture::outcome(GuardOutcome::Deny {
            reason: Text::new("src/billing/a.rs is in the area api:billing, which alice holds."),
        })
        .on("pre-tool-use")
        .denies("src/billing/a.rs is in the area api:billing, which alice holds."),
        ReplyFixture::outcome(GuardOutcome::Deny {
            reason: Text::new("This command needs the slot deploy:prod; alice holds it."),
        })
        .on("pre-tool-use")
        .denies("This command needs the slot deploy:prod; alice holds it."),
        ReplyFixture::outcome(GuardOutcome::NoObjection {
            context: Some(Text::new(
                "dibs delivers, while you work: a message from alice",
            )),
            notice: Some(Text::new("dibs: the server could not be reached.")),
        })
        .on("user-prompt-submit")
        .allows()
        .carries_context("dibs delivers, while you work: a message from alice")
        .carries_notice("dibs: the server could not be reached."),
    ];
    Fixtures { events, replies }
}

/// Reads a reply as Claude Code does: exit `0`, then nothing or exactly one JSON object. A
/// `hookSpecificOutput` must name one of the events, and the only decision it may carry is a
/// refusal; anything else would change what the person's own permission settings decide.
struct ClaudeCodeReader;

impl ReplyReader for ClaudeCodeReader {
    fn read(&self, reply: &HostReply) -> Option<ReadReply> {
        if reply.exit != 0 {
            return None;
        }
        if reply.stdout.iter().all(u8::is_ascii_whitespace) {
            return Some(ReadReply {
                denies: false,
                reason: None,
                context: None,
                notice: None,
            });
        }
        let value: Value = serde_json::from_slice(&reply.stdout).ok()?;
        value.as_object()?;
        let text = |container: Option<&Value>, name: &str| {
            container
                .and_then(|fields| fields.get(name))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let specific = value.get("hookSpecificOutput");
        if let Some(specific) = specific {
            let name = specific.get("hookEventName").and_then(Value::as_str)?;
            if !EVENTS.iter().any(|event| event.name == name) {
                return None;
            }
        }
        let denies = match text(specific, "permissionDecision").as_deref() {
            None => false,
            Some("deny") => true,
            Some(_) => return None,
        };
        Some(ReadReply {
            denies,
            reason: text(specific, "permissionDecisionReason"),
            context: text(specific, "additionalContext"),
            notice: text(Some(&value), "systemMessage"),
        })
    }
}

#[test]
fn claude_code_passes_the_conformance_kit() {
    assert_eq!(
        conformance::check(&ClaudeCode, &fixtures(), &ClaudeCodeReader),
        Ok(())
    );
}

fn rendered(event: &str, outcome: &GuardOutcome) -> Value {
    let reply = ClaudeCode.render(event, outcome);
    assert_eq!(reply.exit, 0);
    serde_json::from_slice(&reply.stdout).unwrap()
}

#[test]
fn a_refusal_is_the_only_decision_ever_written() {
    let held = Text::new("held by alice\nmessage her first");
    assert_eq!(
        rendered("pre-tool-use", &GuardOutcome::Deny { reason: held }),
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": "held by alice\nmessage her first"
        }})
    );
    let outcomes = [
        GuardOutcome::silent(),
        GuardOutcome::context("a note"),
        GuardOutcome::notice("dibs: away"),
    ];
    for event in EVENTS {
        for outcome in &outcomes {
            let reply = ClaudeCode.render(event.argument, outcome);
            assert_eq!(reply.exit, 0);
            if reply.stdout.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_slice(&reply.stdout).unwrap();
            assert_eq!(
                value["hookSpecificOutput"].get("permissionDecision"),
                None,
                "{} {outcome:?}: {value}",
                event.argument
            );
        }
    }
}

#[test]
fn no_objection_says_nothing_or_carries_its_text_without_a_decision() {
    assert_eq!(
        ClaudeCode.render("pre-tool-use", &GuardOutcome::silent()),
        HostReply::empty()
    );
    assert_eq!(
        rendered(
            "pre-tool-use",
            &GuardOutcome::context("rates.toml: regenerate it")
        ),
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": "rates.toml: regenerate it"
        }})
    );
    assert_eq!(
        rendered("session-start", &GuardOutcome::context("the brief")),
        json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": "the brief"}})
    );
    assert_eq!(
        rendered(
            "post-tool-use",
            &GuardOutcome::notice("dibs: the server\ncould not be reached.")
        ),
        json!({"systemMessage": "dibs: the server could not be reached."})
    );
}

#[test]
fn an_event_claude_code_does_not_send_gets_only_the_line_for_the_person() {
    assert_eq!(
        rendered(
            "before-tool",
            &GuardOutcome::NoObjection {
                context: Some(Text::new("the brief")),
                notice: Some(Text::new("dibs: the event is unknown.")),
            }
        ),
        json!({"systemMessage": "dibs: the event is unknown."})
    );
    assert_eq!(
        rendered(
            "post-tool-use",
            &GuardOutcome::Deny {
                reason: Text::new("too late to stop")
            }
        ),
        json!({"systemMessage": "too late to stop"}),
        "a tool that already ran cannot be refused"
    );
    assert_eq!(
        ClaudeCode.render("", &GuardOutcome::context("only for the agent")),
        HostReply::empty()
    );
}

#[test]
fn a_neutral_event_name_is_not_one_of_claude_codes() {
    assert_eq!(
        ClaudeCode.parse("before-tool", b"{}"),
        Err(ParseError::UnknownEvent("before-tool".to_owned()))
    );
}

#[test]
fn the_capabilities_are_what_claude_code_hooks_carry() {
    let capabilities = ClaudeCode.capabilities();
    assert!(capabilities.deny_edit && capabilities.deny_shell);
    assert!(capabilities.context_at_start && capabilities.context_after_tool);
    assert!(capabilities.context_at_prompt && capabilities.tells_subagent);
    assert!(capabilities.push_to_idle, "through the channel");
    assert!(ClaudeCode.push_extension().is_some());
    assert_eq!(ClaudeCode.id().as_str(), "claude-code");
}

#[test]
fn text_between_tool_calls_and_with_a_prompt_is_additional_context_naming_its_event() {
    for (event, name) in [
        ("post-tool-use", "PostToolUse"),
        ("user-prompt-submit", "UserPromptSubmit"),
    ] {
        assert_eq!(
            rendered(event, &GuardOutcome::context("a message from alice")),
            json!({"hookSpecificOutput": {"hookEventName": name, "additionalContext": "a message from alice"}})
        );
    }
}

#[test]
fn the_end_of_a_turn_never_carries_text_for_the_agent_which_would_keep_the_turn_going() {
    assert_eq!(
        ClaudeCode.render("stop", &GuardOutcome::context("a message from alice")),
        HostReply::empty()
    );
    assert_eq!(
        rendered(
            "stop",
            &GuardOutcome::NoObjection {
                context: Some(Text::new("a message from alice")),
                notice: Some(Text::new("dibs: the server cannot be reached.")),
            }
        ),
        json!({"systemMessage": "dibs: the server cannot be reached."})
    );
}
