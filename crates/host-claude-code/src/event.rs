//! The Claude Code hook events the guard listens to, by the argument the hook configuration passes
//! and by the name Claude Code gives them, with the shape of the reply each one reads.

use dibs_agent_host::event::EventKind;

/// How Claude Code reads a hook's reply to an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyShape {
    /// One JSON object on standard output, or nothing, and exit status `0`.
    Json,
    /// A directory's path as the last line of standard output and exit status `0`; any other
    /// status fails what the event announces, with standard error as the reason.
    Path,
}

/// One hook event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookEvent {
    /// The argument of `dibs hook claude-code <event>`.
    pub argument: &'static str,
    /// Claude Code's name of the event: `hook_event_name` in its input, `hookEventName` in a
    /// hook's output.
    pub name: &'static str,
    /// The neutral event it is.
    pub kind: EventKind,
    /// Whether text for the agent may go in its reply. `Stop` takes it as feedback that keeps the
    /// turn going, so nothing is ever put there.
    pub carries_context: bool,
    /// How Claude Code reads the reply.
    pub reply: ReplyShape,
}

/// Every event the guard listens to.
pub const EVENTS: [HookEvent; 9] = [
    HookEvent {
        argument: "session-start",
        name: "SessionStart",
        kind: EventKind::SessionStarted,
        carries_context: true,
        reply: ReplyShape::Json,
    },
    HookEvent {
        argument: "pre-tool-use",
        name: "PreToolUse",
        kind: EventKind::BeforeTool,
        carries_context: true,
        reply: ReplyShape::Json,
    },
    HookEvent {
        argument: "post-tool-use",
        name: "PostToolUse",
        kind: EventKind::AfterTool,
        carries_context: true,
        reply: ReplyShape::Json,
    },
    HookEvent {
        argument: "user-prompt-submit",
        name: "UserPromptSubmit",
        kind: EventKind::PromptSubmitted,
        carries_context: true,
        reply: ReplyShape::Json,
    },
    HookEvent {
        argument: "stop",
        name: "Stop",
        kind: EventKind::Stopped,
        carries_context: false,
        reply: ReplyShape::Json,
    },
    HookEvent {
        argument: "worktree-create",
        name: "WorktreeCreate",
        kind: EventKind::WorktreeWanted,
        carries_context: false,
        reply: ReplyShape::Path,
    },
    HookEvent {
        argument: "worktree-remove",
        name: "WorktreeRemove",
        kind: EventKind::WorktreeReturned,
        carries_context: false,
        reply: ReplyShape::Json,
    },
    HookEvent {
        argument: "subagent-stop",
        name: "SubagentStop",
        kind: EventKind::AgentEnded,
        carries_context: false,
        reply: ReplyShape::Json,
    },
    HookEvent {
        argument: "session-end",
        name: "SessionEnd",
        kind: EventKind::SessionEnded,
        carries_context: false,
        reply: ReplyShape::Json,
    },
];

impl HookEvent {
    /// The event this argument names.
    pub fn from_argument(argument: &str) -> Option<Self> {
        EVENTS
            .iter()
            .copied()
            .find(|event| event.argument == argument)
    }

    /// Whether a refusal on this event stops what it announces: only a tool that has not run yet
    /// can be stopped.
    pub fn can_deny(self) -> bool {
        self.kind == EventKind::BeforeTool
    }
}
