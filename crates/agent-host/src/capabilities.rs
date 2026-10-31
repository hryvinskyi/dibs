//! What a host can carry, declared by its adapter. The guard never produces what a host cannot
//! carry: an answer the host would drop is not computed, and a call that would only feed such an
//! answer is not made.

use serde::{Deserialize, Serialize};

/// What a host can carry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capabilities {
    /// A refusal before a file-writing tool runs stops it.
    pub deny_edit: bool,
    /// A refusal before a shell command runs stops it.
    pub deny_shell: bool,
    /// Text for the agent at the start of a session reaches it.
    pub context_at_start: bool,
    /// Text for the agent after a tool ran reaches it.
    pub context_after_tool: bool,
    /// Text for the agent when the person submits a prompt reaches it.
    pub context_at_prompt: bool,
    /// The host says whether an event comes from the main agent or a subagent.
    pub tells_subagent: bool,
    /// The host can wake an idle session with a pushed message.
    pub push_to_idle: bool,
    /// The host asks the guard for the worktrees its sessions and agents run in, and waits for
    /// the directory: its own way of making one is replaced by the guard's answer.
    pub serves_worktrees: bool,
}

impl Capabilities {
    /// Every capability with its name, in a fixed order, for showing to a person.
    pub fn named(&self) -> [(&'static str, bool); 8] {
        [
            ("deny_edit", self.deny_edit),
            ("deny_shell", self.deny_shell),
            ("context_at_start", self.context_at_start),
            ("context_after_tool", self.context_after_tool),
            ("context_at_prompt", self.context_at_prompt),
            ("tells_subagent", self.tells_subagent),
            ("push_to_idle", self.push_to_idle),
            ("serves_worktrees", self.serves_worktrees),
        ]
    }
}
