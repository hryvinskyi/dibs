//! The Claude Code hook events the guard listens to, by the argument the hook configuration passes
//! and by the name Claude Code gives them.

use dibs_agent_host::event::EventKind;

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
}

/// Every event the guard listens to.
pub const EVENTS: [HookEvent; 5] = [
    HookEvent {
        argument: "session-start",
        name: "SessionStart",
        kind: EventKind::SessionStarted,
        carries_context: true,
    },
    HookEvent {
        argument: "pre-tool-use",
        name: "PreToolUse",
        kind: EventKind::BeforeTool,
        carries_context: true,
    },
    HookEvent {
        argument: "post-tool-use",
        name: "PostToolUse",
        kind: EventKind::AfterTool,
        carries_context: true,
    },
    HookEvent {
        argument: "user-prompt-submit",
        name: "UserPromptSubmit",
        kind: EventKind::PromptSubmitted,
        carries_context: true,
    },
    HookEvent {
        argument: "stop",
        name: "Stop",
        kind: EventKind::Stopped,
        carries_context: false,
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
