//! The Claude Code host: its command hooks' JSON input read as neutral events, and the guard's
//! outcomes written as the JSON output Claude Code reads from a hook.
//!
//! The plugin's hook configuration runs `dibs hook claude-code <event>` for these events:
//!
//! | Argument | Claude Code event | Neutral event |
//! |---|---|---|
//! | `session-start` | `SessionStart` | `SessionStarted` |
//! | `pre-tool-use` | `PreToolUse` | `BeforeTool` |
//! | `post-tool-use` | `PostToolUse` | `AfterTool` |
//! | `user-prompt-submit` | `UserPromptSubmit` | `PromptSubmitted` |
//! | `stop` | `Stop` | `Stopped` |
//!
//! **Input** is the object Claude Code writes to the hook's standard input: `session_id` and `cwd`
//! always; `hook_event_name`, which must name the same event when present; `tool_name` and
//! `tool_input` before a tool runs; `agent_id` when the hook fires inside a subagent. `Edit`,
//! `Write` and `MultiEdit` write `tool_input.file_path`, `NotebookEdit` writes
//! `tool_input.notebook_path`, and `Bash` runs `tool_input.command`. Every other tool, the
//! `PowerShell` tool among them, is one the guard has no say about yet. Fields this adapter does
//! not know are ignored.
//!
//! **Output** is one JSON object on standard output, or nothing, and the exit status is always
//! `0`:
//!
//! - a refusal before a tool runs is `hookSpecificOutput.permissionDecision: "deny"` with the
//!   reason in `permissionDecisionReason`;
//! - text for the agent is `hookSpecificOutput.additionalContext`, with `hookEventName`, on every
//!   event but `Stop`, which reads it as feedback that keeps the turn going;
//! - a line for the person is `systemMessage`;
//! - nothing to say is no output at all.
//!
//! **A session waiting for its person** is woken through the [`channel`]: the tool server, run as
//! `dibs mcp --host claude-code` and opted in for the session, pushes what would otherwise wait
//! for the next tool call.
//!
//! No other `permissionDecision` is ever written. `"allow"` would skip the permission prompt the
//! person's own settings ask for, and `"ask"` would add one they did not; "no objection" leaves
//! Claude Code to decide exactly as it would without dibs. Claude Code reads exit status `2` as a
//! block, so no reply uses it.

pub mod channel;
mod event;
mod input;
mod reply;

#[cfg(test)]
mod tests;

use dibs_agent_host::adapter::{HostAdapter, HostId, HostReply, ParseError, Probe};
use dibs_agent_host::capabilities::Capabilities;
use dibs_agent_host::event::HostEvent;
use dibs_agent_host::outcome::GuardOutcome;
use dibs_agent_host::push::McpExtension;

use crate::channel::ClaudeChannel;
use crate::event::HookEvent;

/// The Claude Code host.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeCode;

impl ClaudeCode {
    /// The host's id.
    pub const ID: HostId = HostId::new("claude-code");
}

impl HostAdapter for ClaudeCode {
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
            push_to_idle: true,
        }
    }

    fn parse(&self, event: &str, input: &[u8]) -> Result<HostEvent, ParseError> {
        let hook = HookEvent::from_argument(event)
            .ok_or_else(|| ParseError::UnknownEvent(event.to_owned()))?;
        input::parse(hook, input)
    }

    fn render(&self, event: &str, outcome: &GuardOutcome) -> HostReply {
        reply::render(HookEvent::from_argument(event), outcome)
    }

    fn probes(&self) -> Vec<Probe> {
        vec![
            Probe::Program {
                name: "claude",
                meaning: "the Claude Code command line",
            },
            Probe::HomePath {
                path: ".claude/plugins/cache/dibs",
                meaning: "plugins installed from the dibs marketplace",
            },
        ]
    }

    fn push_extension(&self) -> Option<Box<dyn McpExtension>> {
        Some(Box::new(ClaudeChannel))
    }
}
