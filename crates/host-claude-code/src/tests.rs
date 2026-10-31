//! The adapter against synthetic hook inputs written from Claude Code's hooks reference and from
//! the payloads Claude Code was seen to send, through the conformance kit every adapter passes, and
//! its replies as Claude Code reads them.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use dibs_agent_host::adapter::{HostAdapter, HostReply, ParseError};
use dibs_agent_host::conformance::{
    self, EventFixture, Fixtures, ReadReply, ReadWorktree, ReplyFixture, ReplyReader,
};
use dibs_agent_host::event::{AgentKey, HostEvent, SessionKey, Thread, ToolCall, WorktreeLabel};
use dibs_agent_host::outcome::GuardOutcome;
use dibs_agent_host::text::Text;

use crate::ClaudeCode;
use crate::event::EVENTS;

const SESSION: &str = "5b9c0e1a-7f3d-4c2e-9a61-0d4e8f2b7c19";

/// A worktree of the pool beside the checkout `/work/api`.
const POOL_WORKTREE: &str = "/work/api-worktrees/worktree2";

/// A hook input of one of the events about worktrees and ends, with exactly the common fields
/// Claude Code was seen to send for them (no `permission_mode` where it sends none) and `extra`.
fn bare_input(event: &str, cwd: &str, extra: &Value) -> Vec<u8> {
    let mut value = json!({
        "session_id": SESSION,
        "transcript_path": "/home/alice/.claude/projects/-work-api/5b9c0e1a.jsonl",
        "cwd": cwd,
        "hook_event_name": event,
    });
    if let (Some(fields), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        fields.extend(more.clone());
    }
    serde_json::to_vec(&value).unwrap()
}

fn wanted(label: &'static str, agent: Option<&str>) -> HostEvent {
    HostEvent::WorktreeWanted {
        session_key: key(),
        cwd: PathBuf::from("/work/api"),
        label: WorktreeLabel::fixture(label),
        agent: agent.and_then(|agent| AgentKey::parse(agent).ok()),
    }
}

/// The events about worktrees and ends, as Claude Code 2.1.316 was seen to send them: a subagent
/// started with `isolation: worktree`, a session started with `--worktree`, `EnterWorktree`,
/// `ExitWorktree` discarding the worktree, a subagent stopping in its worktree, and `/clear`.
fn worktree_events() -> Vec<EventFixture> {
    let create = |extra: Value| bare_input("WorktreeCreate", "/work/api", &extra);
    vec![
        EventFixture::parses(
            "worktree-create",
            &create(json!({
                "prompt_id": "39bbc0e3-d254-4f21-b4aa-f10fe4570041",
                "name": "agent-a8f3c2d1e0b94a7"
            })),
            wanted("agent-a8f3c2d1e0b94a7", Some("a8f3c2d1e0b94a7")),
        ),
        EventFixture::parses(
            "worktree-create",
            &create(json!({"name": "fix-totals"})),
            wanted("fix-totals", None),
        ),
        EventFixture::parses(
            "worktree-create",
            &create(
                json!({"prompt_id": "fe6c3dee-5dc8-4dd0-b058-dc8fbc61266a", "name": "billing.v2"}),
            ),
            wanted("billing.v2", None),
        ),
        EventFixture::parses(
            "worktree-create",
            &create(json!({"name": "agent-"})),
            wanted("agent-", None),
        ),
        EventFixture::refuses("worktree-create", &create(json!({}))),
        EventFixture::refuses("worktree-create", &create(json!({"name": ""}))),
        EventFixture::refuses("worktree-create", &create(json!({"name": "../escape"}))),
        EventFixture::refuses("worktree-create", &create(json!({"name": "a/b"}))),
        EventFixture::refuses(
            "worktree-create",
            &bare_input(
                "WorktreeRemove",
                "/work/api",
                &json!({"name": "fix-totals", "worktree_path": POOL_WORKTREE}),
            ),
        ),
        EventFixture::parses(
            "worktree-remove",
            &bare_input(
                "WorktreeRemove",
                POOL_WORKTREE,
                &json!({
                    "prompt_id": "6081a9de-2a50-4ac8-ac37-b5ae8c70c057",
                    "worktree_path": POOL_WORKTREE
                }),
            ),
            HostEvent::WorktreeReturned {
                session_key: key(),
                cwd: PathBuf::from(POOL_WORKTREE),
                path: PathBuf::from(POOL_WORKTREE),
            },
        ),
        EventFixture::refuses(
            "worktree-remove",
            &bare_input("WorktreeRemove", POOL_WORKTREE, &json!({})),
        ),
        EventFixture::parses(
            "subagent-stop",
            &bare_input(
                "SubagentStop",
                POOL_WORKTREE,
                &json!({
                    "prompt_id": "849701a5-d9c3-4d3b-9800-c5962f05ac81",
                    "permission_mode": "acceptEdits",
                    "agent_id": "a8f3c2d1e0b94a7",
                    "agent_type": "general-purpose",
                    "agent_transcript_path": "/home/alice/.claude/projects/-work-api/subagents/agent-a8f3c2d1e0b94a7.jsonl",
                    "stop_hook_active": false,
                    "last_assistant_message": "done",
                    "background_tasks": [],
                    "session_crons": []
                }),
            ),
            HostEvent::AgentEnded {
                session_key: key(),
                cwd: PathBuf::from(POOL_WORKTREE),
                agent: AgentKey::parse("a8f3c2d1e0b94a7").unwrap(),
            },
        ),
        EventFixture::refuses(
            "subagent-stop",
            &bare_input(
                "SubagentStop",
                POOL_WORKTREE,
                &json!({"agent_type": "Explore"}),
            ),
        ),
        EventFixture::refuses(
            "subagent-stop",
            &bare_input("SubagentStop", POOL_WORKTREE, &json!({"agent_id": "a b"})),
        ),
        EventFixture::parses(
            "session-end",
            &bare_input(
                "SessionEnd",
                POOL_WORKTREE,
                &json!({"prompt_id": "6304e432-3eff-485e-ac0d-0aaa3b66c3a0", "reason": "clear"}),
            ),
            HostEvent::SessionEnded {
                session_key: key(),
                cwd: PathBuf::from(POOL_WORKTREE),
            },
        ),
        EventFixture::parses(
            "session-end",
            &bare_input("SessionEnd", "/work/api", &json!({"reason": "other"})),
            HostEvent::SessionEnded {
                session_key: key(),
                cwd: PathBuf::from("/work/api"),
            },
        ),
        EventFixture::refuses(
            "session-end",
            &bare_input("Stop", "/work/api", &json!({"reason": "other"})),
        ),
    ]
}

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
        agent: None,
    }
}

fn edit(path: &str) -> ToolCall {
    ToolCall::Edit {
        paths: vec![PathBuf::from(path)],
    }
}

fn fixtures() -> Fixtures {
    let pre = |extra: Value| hook_input("PreToolUse", "/work/api", &extra);
    let mut events = vec![
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
            HostEvent::BeforeTool {
                session_key: key(),
                cwd: PathBuf::from("/work/api"),
                call: ToolCall::Shell {
                    command: "make release".to_owned(),
                },
                thread: Thread::Subagent,
                agent: AgentKey::parse("agent-abc123").ok(),
            },
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
                agent: None,
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
                agent: AgentKey::parse("def456").ok(),
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
    events.extend(worktree_events());
    let replies =
        vec![
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
        ReplyFixture::outcome(GuardOutcome::silent())
            .on("worktree-remove")
            .allows(),
        ReplyFixture::outcome(GuardOutcome::notice(
            "dibs: the worktree /work/api/.claude/worktrees/fix-totals was kept: it has changes.",
        ))
        .on("worktree-remove")
        .allows()
        .carries_notice(
            "dibs: the worktree /work/api/.claude/worktrees/fix-totals was kept: it has changes.",
        ),
        ReplyFixture::outcome(GuardOutcome::context("a message from alice"))
            .on("subagent-stop")
            .allows(),
        ReplyFixture::outcome(GuardOutcome::notice("dibs: worktree2 could not be released."))
            .on("subagent-stop")
            .allows()
            .carries_notice("dibs: worktree2 could not be released."),
        ReplyFixture::outcome(GuardOutcome::context("a message from alice"))
            .on("session-end")
            .allows(),
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

    /// Reads a reply to `WorktreeCreate` as Claude Code does: exit `0` and the last line of
    /// standard output is the directory, which nothing on it fails; any other status fails the
    /// creation with standard error as the reason, `2` aside, which no hook may use.
    fn read_worktree(&self, reply: &HostReply) -> Option<ReadWorktree> {
        match reply.exit {
            0 => {
                let stdout = std::str::from_utf8(&reply.stdout).ok()?;
                let line = stdout.lines().rfind(|line| !line.trim().is_empty())?;
                Some(ReadWorktree::Path(line.to_owned()))
            }
            2 => None,
            _ => Some(ReadWorktree::Refused(
                String::from_utf8_lossy(&reply.stderr).trim().to_owned(),
            )),
        }
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
    assert!(
        capabilities.serves_worktrees,
        "WorktreeCreate waits for a path"
    );
    let terms = ClaudeCode.worktree_terms().unwrap();
    assert_eq!(terms.hook_timeout, Duration::from_secs(900));
    assert_eq!(
        terms.deadline(),
        Duration::from_secs(840),
        "a take leaves a minute to answer"
    );
    assert_eq!(terms.own.dir, ".claude/worktrees");
    assert_eq!(terms.own.branch_prefix, "worktree-");
}

fn worktree_reply(outcome: &GuardOutcome) -> HostReply {
    ClaudeCode.render("worktree-create", outcome)
}

#[test]
fn a_worktree_is_its_directory_alone_on_standard_output_and_exit_0() {
    let reply = worktree_reply(&GuardOutcome::Worktree {
        path: PathBuf::from(POOL_WORKTREE),
        notice: Some(Text::new(
            "dibs: worktree2 was reset;\nyour work is on a rescue branch.",
        )),
    });
    assert_eq!(reply.stdout, format!("{POOL_WORKTREE}\n").into_bytes());
    assert_eq!(
        String::from_utf8(reply.stderr).unwrap(),
        "dibs: worktree2 was reset; your work is on a rescue branch.\n",
        "the line for the person goes where Claude Code shows a hook's message"
    );
    assert_eq!(reply.exit, 0);
    let quiet = worktree_reply(&GuardOutcome::Worktree {
        path: PathBuf::from(POOL_WORKTREE),
        notice: None,
    });
    assert_eq!(
        quiet,
        HostReply::stdout(format!("{POOL_WORKTREE}\n").into_bytes())
    );
}

#[test]
fn a_refused_worktree_fails_the_creation_with_its_reason_on_standard_error() {
    for outcome in [
        GuardOutcome::NoWorktree {
            reason: Text::new("dibs: every worktree of the pool is held:\nworktree1 by bob."),
        },
        GuardOutcome::Deny {
            reason: Text::new("dibs: every worktree of the pool is held:\nworktree1 by bob."),
        },
    ] {
        let reply = worktree_reply(&outcome);
        assert_eq!(reply.stdout, Vec::<u8>::new(), "{outcome:?}");
        assert_eq!(
            String::from_utf8(reply.stderr).unwrap(),
            "dibs: every worktree of the pool is held: worktree1 by bob.\n"
        );
        assert_eq!(reply.exit, 1, "a failed creation, never the block of 2");
    }
}

#[test]
fn no_objection_to_a_wanted_worktree_names_no_directory() {
    assert_eq!(worktree_reply(&GuardOutcome::silent()), HostReply::empty());
    let reply = worktree_reply(&GuardOutcome::NoObjection {
        context: Some(Text::new("the brief")),
        notice: Some(Text::new("dibs: the hook input could not be read.")),
    });
    assert_eq!(
        reply.stdout,
        Vec::<u8>::new(),
        "no text for the agent either"
    );
    assert_eq!(
        String::from_utf8(reply.stderr).unwrap(),
        "dibs: the hook input could not be read.\n"
    );
    assert_eq!(
        reply.exit, 0,
        "no directory and exit 0: whoever runs the hook makes the worktree"
    );
}

#[test]
fn a_worktree_answered_for_an_event_that_takes_no_directory_is_its_line_for_the_person() {
    assert_eq!(
        rendered(
            "before-tool",
            &GuardOutcome::Worktree {
                path: PathBuf::from(POOL_WORKTREE),
                notice: Some(Text::new("dibs: taken from the pool.")),
            }
        ),
        json!({"systemMessage": "dibs: taken from the pool."})
    );
    assert_eq!(
        rendered(
            "session-end",
            &GuardOutcome::NoWorktree {
                reason: Text::new("dibs: every worktree is held.")
            }
        ),
        json!({"systemMessage": "dibs: every worktree is held."})
    );
}

#[test]
fn ends_and_a_worktree_given_back_carry_nothing_for_the_agent() {
    for event in ["worktree-remove", "subagent-stop", "session-end"] {
        assert_eq!(
            ClaudeCode.render(event, &GuardOutcome::context("a message from alice")),
            HostReply::empty(),
            "{event}"
        );
        assert_eq!(
            rendered(event, &GuardOutcome::notice("dibs: worktree2 is kept.")),
            json!({"systemMessage": "dibs: worktree2 is kept."}),
            "{event}"
        );
    }
}

#[test]
fn a_subagents_worktree_is_wanted_for_the_agent_its_name_carries() {
    let parse = |name: &str| {
        ClaudeCode.parse(
            "worktree-create",
            &bare_input("WorktreeCreate", "/work/api", &json!({"name": name})),
        )
    };
    let Ok(HostEvent::WorktreeWanted { label, agent, .. }) = parse("agent-a8f3c2d1e0b94a7") else {
        panic!("not a wanted worktree");
    };
    assert_eq!(label.as_str(), "agent-a8f3c2d1e0b94a7");
    assert_eq!(agent, AgentKey::parse("a8f3c2d1e0b94a7").ok());
    let Ok(HostEvent::WorktreeWanted { label, agent, .. }) = parse("fix-totals") else {
        panic!("not a wanted worktree");
    };
    assert_eq!(label.as_str(), "fix-totals");
    assert_eq!(
        agent, None,
        "a session's own worktree is held by the session"
    );
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
