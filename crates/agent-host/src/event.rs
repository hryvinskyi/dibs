//! What a host tells the guard, in words no host owns: a session started, a tool is about to run,
//! a tool ran, a prompt was submitted.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
    },
    /// A tool ran.
    AfterTool {
        /// The session.
        session_key: SessionKey,
        /// Its working directory.
        cwd: PathBuf,
        /// The thread that ran it.
        thread: Thread,
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
        }
    }

    /// The session the event belongs to.
    pub fn session_key(&self) -> &SessionKey {
        match self {
            Self::SessionStarted { session_key, .. }
            | Self::BeforeTool { session_key, .. }
            | Self::AfterTool { session_key, .. }
            | Self::PromptSubmitted { session_key, .. }
            | Self::Stopped { session_key, .. } => session_key,
        }
    }

    /// The working directory of the event.
    pub fn cwd(&self) -> &PathBuf {
        match self {
            Self::SessionStarted { cwd, .. }
            | Self::BeforeTool { cwd, .. }
            | Self::AfterTool { cwd, .. }
            | Self::PromptSubmitted { cwd, .. }
            | Self::Stopped { cwd, .. } => cwd,
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
}

impl EventKind {
    /// Every kind.
    pub const ALL: [Self; 5] = [
        Self::SessionStarted,
        Self::BeforeTool,
        Self::AfterTool,
        Self::PromptSubmitted,
        Self::Stopped,
    ];

    /// The kind's name: `session_started`, `before_tool`, `after_tool`, `prompt_submitted`,
    /// `stopped`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStarted => "session_started",
            Self::BeforeTool => "before_tool",
            Self::AfterTool => "after_tool",
            Self::PromptSubmitted => "prompt_submitted",
            Self::Stopped => "stopped",
        }
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
        };
        assert_eq!(event.session_key(), &key);
        assert_eq!(event.cwd(), &PathBuf::from("/work"));
        assert_eq!(event.kind().as_str(), "after_tool");
    }
}
