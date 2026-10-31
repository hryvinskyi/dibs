//! What a host tells the guard, in words no host owns: a session started, a tool is about to run,
//! a tool ran, a prompt was submitted, a worktree is wanted or returned, an agent or a session
//! ended.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use dibs_domain::worktree::AgentKey;

/// The host's own id of one running agent session, as the host reports it. It names the session
/// to the guard and is never shown to anyone else.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionKey(String);

impl SessionKey {
    /// The longest key, in characters.
    pub const MAX_CHARS: usize = 256;

    /// A key: 1 to 256 characters, none of them a control character.
    pub fn parse(value: &str) -> Option<Self> {
        let ok = !value.is_empty()
            && value.chars().count() <= Self::MAX_CHARS
            && !value.chars().any(char::is_control);
        ok.then(|| Self(value.to_owned()))
    }

    /// The key.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A key written into a fixture, which needs no check.
    #[cfg(any(test, feature = "conformance"))]
    pub fn fixture(text: &'static str) -> Self {
        Self(text.to_owned())
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The name a host gives a worktree it wants, which also names the worktree the host would make
/// itself: 1 to 64 ASCII letters, digits, `.`, `_` and `-`, starting with a letter or a digit, so
/// it is one directory name and one branch name and never climbs out of the directory it is put
/// in.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorktreeLabel(String);

impl WorktreeLabel {
    /// The longest label, in characters.
    pub const MAX_CHARS: usize = 64;

    /// A label, when the text keeps the rules above.
    pub fn parse(value: &str) -> Option<Self> {
        let mut chars = value.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
        let ok = first_ok
            && value.len() <= Self::MAX_CHARS
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            && !value.contains("..");
        ok.then(|| Self(value.to_owned()))
    }

    /// The label.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A label written into a fixture, which needs no check.
    #[cfg(any(test, feature = "conformance"))]
    pub fn fixture(text: &'static str) -> Self {
        Self(text.to_owned())
    }
}

impl fmt::Display for WorktreeLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The tool a host is about to run, as far as the guard tells tools apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCall {
    /// A tool that writes these files, absolute or relative to the event's directory.
    Edit {
        /// The files.
        paths: Vec<PathBuf>,
    },
    /// A shell command line.
    Shell {
        /// The command line as the agent wrote it.
        command: String,
    },
    /// Any other tool.
    Other,
}

/// Which thread of a session an event comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Thread {
    /// The session's main agent.
    Main,
    /// A subagent the main agent started; what it is told ends with it.
    Subagent,
    /// The host does not say.
    Unknown,
}

/// An event of a host, in neutral terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostEvent {
    /// A session started or resumed.
    SessionStarted {
        /// The session.
        session_key: SessionKey,
        /// Its working directory.
        cwd: PathBuf,
    },
    /// A tool is about to run.
    BeforeTool {
        /// The session.
        session_key: SessionKey,
        /// Its working directory.
        cwd: PathBuf,
        /// The tool.
        call: ToolCall,
        /// The thread running it.
        thread: Thread,
        /// The host's key of the agent running it, when a subagent runs it and the host names it.
        agent: Option<AgentKey>,
    },
    /// A tool ran.
    AfterTool {
        /// The session.
        session_key: SessionKey,
        /// Its working directory.
        cwd: PathBuf,
        /// The thread that ran it.
        thread: Thread,
        /// The host's key of the agent that ran it, when a subagent ran it and the host names it.
        agent: Option<AgentKey>,
    },
    /// A prompt was submitted: by the person, or by the host on a subagent's behalf when one
    /// reports back.
    PromptSubmitted {
        /// The session.
        session_key: SessionKey,
        /// Its working directory.
        cwd: PathBuf,
        /// The thread it reaches.
        thread: Thread,
    },
    /// The main agent finished its turn and the session waits for its person.
    Stopped {
        /// The session.
        session_key: SessionKey,
        /// Its working directory.
        cwd: PathBuf,
    },
    /// The host wants a worktree to run the session, or one of its agents, in, and waits for its
    /// directory: the guard's answer replaces the worktree the host would make itself.
    WorktreeWanted {
        /// The session.
        session_key: SessionKey,
        /// Its working directory: the checkout the worktree is wanted from.
        cwd: PathBuf,
        /// The name the host gives the worktree.
        label: WorktreeLabel,
        /// The agent the worktree is for; `None` for the session's main agent.
        agent: Option<AgentKey>,
    },
    /// The host is done with a worktree it was given and would remove it.
    WorktreeReturned {
        /// The session.
        session_key: SessionKey,
        /// Its working directory.
        cwd: PathBuf,
        /// The worktree's directory.
        path: PathBuf,
    },
    /// An agent the main agent started finished.
    AgentEnded {
        /// The session.
        session_key: SessionKey,
        /// The agent's working directory.
        cwd: PathBuf,
        /// The agent.
        agent: AgentKey,
    },
    /// The session ended.
    SessionEnded {
        /// The session.
        session_key: SessionKey,
        /// Its working directory.
        cwd: PathBuf,
    },
}

impl HostEvent {
    /// The kind of the event.
    pub fn kind(&self) -> EventKind {
        match self {
            Self::SessionStarted { .. } => EventKind::SessionStarted,
            Self::BeforeTool { .. } => EventKind::BeforeTool,
            Self::AfterTool { .. } => EventKind::AfterTool,
            Self::PromptSubmitted { .. } => EventKind::PromptSubmitted,
            Self::Stopped { .. } => EventKind::Stopped,
            Self::WorktreeWanted { .. } => EventKind::WorktreeWanted,
            Self::WorktreeReturned { .. } => EventKind::WorktreeReturned,
            Self::AgentEnded { .. } => EventKind::AgentEnded,
            Self::SessionEnded { .. } => EventKind::SessionEnded,
        }
    }

    /// The session the event belongs to.
    pub fn session_key(&self) -> &SessionKey {
        match self {
            Self::SessionStarted { session_key, .. }
            | Self::BeforeTool { session_key, .. }
            | Self::AfterTool { session_key, .. }
            | Self::PromptSubmitted { session_key, .. }
            | Self::Stopped { session_key, .. }
            | Self::WorktreeWanted { session_key, .. }
            | Self::WorktreeReturned { session_key, .. }
            | Self::AgentEnded { session_key, .. }
            | Self::SessionEnded { session_key, .. } => session_key,
        }
    }

    /// The working directory of the event.
    pub fn cwd(&self) -> &PathBuf {
        match self {
            Self::SessionStarted { cwd, .. }
            | Self::BeforeTool { cwd, .. }
            | Self::AfterTool { cwd, .. }
            | Self::PromptSubmitted { cwd, .. }
            | Self::Stopped { cwd, .. }
            | Self::WorktreeWanted { cwd, .. }
            | Self::WorktreeReturned { cwd, .. }
            | Self::AgentEnded { cwd, .. }
            | Self::SessionEnded { cwd, .. } => cwd,
        }
    }

    /// The host's key of the agent the event comes from or is about, when the host names one;
    /// `None` for the session's main agent and for an event no agent sends.
    pub fn agent(&self) -> Option<&AgentKey> {
        match self {
            Self::BeforeTool { agent, .. }
            | Self::AfterTool { agent, .. }
            | Self::WorktreeWanted { agent, .. } => agent.as_ref(),
            Self::AgentEnded { agent, .. } => Some(agent),
            Self::SessionStarted { .. }
            | Self::PromptSubmitted { .. }
            | Self::Stopped { .. }
            | Self::WorktreeReturned { .. }
            | Self::SessionEnded { .. } => None,
        }
    }
}

/// The kinds of event, named the same way on every host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// [`HostEvent::SessionStarted`].
    SessionStarted,
    /// [`HostEvent::BeforeTool`].
    BeforeTool,
    /// [`HostEvent::AfterTool`].
    AfterTool,
    /// [`HostEvent::PromptSubmitted`].
    PromptSubmitted,
    /// [`HostEvent::Stopped`].
    Stopped,
    /// [`HostEvent::WorktreeWanted`].
    WorktreeWanted,
    /// [`HostEvent::WorktreeReturned`].
    WorktreeReturned,
    /// [`HostEvent::AgentEnded`].
    AgentEnded,
    /// [`HostEvent::SessionEnded`].
    SessionEnded,
}

impl EventKind {
    /// Every kind.
    pub const ALL: [Self; 9] = [
        Self::SessionStarted,
        Self::BeforeTool,
        Self::AfterTool,
        Self::PromptSubmitted,
        Self::Stopped,
        Self::WorktreeWanted,
        Self::WorktreeReturned,
        Self::AgentEnded,
        Self::SessionEnded,
    ];

    /// The kind's name: `session_started`, `before_tool`, `after_tool`, `prompt_submitted`,
    /// `stopped`, `worktree_wanted`, `worktree_returned`, `agent_ended`, `session_ended`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStarted => "session_started",
            Self::BeforeTool => "before_tool",
            Self::AfterTool => "after_tool",
            Self::PromptSubmitted => "prompt_submitted",
            Self::Stopped => "stopped",
            Self::WorktreeWanted => "worktree_wanted",
            Self::WorktreeReturned => "worktree_returned",
            Self::AgentEnded => "agent_ended",
            Self::SessionEnded => "session_ended",
        }
    }

    /// Whether a run of the kind is timed into the hook timings. A worktree wanted is not: it may
    /// wait minutes for a worktree to be freed and reset, which is no hook's speed.
    pub fn timed(self) -> bool {
        self != Self::WorktreeWanted
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_key_is_short_text_without_control_characters() {
        assert!(SessionKey::parse("3f2a-session").is_some());
        assert!(SessionKey::parse("").is_none());
        assert!(SessionKey::parse("a\nb").is_none());
        assert!(SessionKey::parse(&"k".repeat(257)).is_none());
        assert!(SessionKey::parse(&"k".repeat(256)).is_some());
    }

    #[test]
    fn every_event_names_its_session_directory_and_kind() {
        let key = SessionKey::parse("s1").unwrap();
        let event = HostEvent::AfterTool {
            session_key: key.clone(),
            cwd: PathBuf::from("/work"),
            thread: Thread::Main,
            agent: None,
        };
        assert_eq!(event.session_key(), &key);
        assert_eq!(event.cwd(), &PathBuf::from("/work"));
        assert_eq!(event.kind().as_str(), "after_tool");
        assert_eq!(event.agent(), None);
    }

    #[test]
    fn an_agent_is_named_by_the_events_that_come_from_or_are_about_one() {
        let agent = AgentKey::parse("a1b2").unwrap();
        let ended = HostEvent::AgentEnded {
            session_key: SessionKey::fixture("s1"),
            cwd: PathBuf::from("/work"),
            agent: agent.clone(),
        };
        assert_eq!(ended.agent(), Some(&agent));
        assert_eq!(ended.kind().as_str(), "agent_ended");
    }

    #[test]
    fn a_worktree_label_is_one_safe_directory_and_branch_name() {
        for good in ["agent-a1b2", "feature.x", "W_1", "a"] {
            assert!(WorktreeLabel::parse(good).is_some(), "{good}");
        }
        for bad in [
            "",
            ".hidden",
            "-dash",
            "a/b",
            "a..b",
            "a b",
            "ü",
            &"k".repeat(65),
        ] {
            assert!(WorktreeLabel::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn only_a_wanted_worktree_is_left_out_of_the_hook_timings() {
        let untimed: Vec<EventKind> = EventKind::ALL
            .into_iter()
            .filter(|kind| !kind.timed())
            .collect();
        assert_eq!(untimed, [EventKind::WorktreeWanted]);
    }
}
