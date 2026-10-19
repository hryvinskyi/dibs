//! The port every agent host is reached through: an adapter turns the host's native hook input
//! into a neutral event and a neutral outcome into the host's native reply.
//!
//! An adapter is pure. It reads no file, clock or network and keeps no state, so the same input
//! always gives the same event and the same outcome always gives the same reply.

use std::fmt;

use thiserror::Error;

use crate::capabilities::Capabilities;
use crate::event::HostEvent;
use crate::outcome::GuardOutcome;

/// A host's id: the name `dibs hook <host> <event>` takes and the agent label its sessions report.
/// It is 1 to 32 characters of lowercase ASCII letters, digits and `-`, starting with a letter or
/// digit; the conformance kit checks that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(&'static str);

impl HostId {
    /// The id.
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    /// The id as text.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// What the hook process prints on standard output and the status it exits with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReply {
    /// Standard output.
    pub stdout: Vec<u8>,
    /// The exit status.
    pub exit: u8,
}

impl HostReply {
    /// Nothing printed, exit `0`: the form of "no objection" every command hook understands.
    pub fn empty() -> Self {
        Self {
            stdout: Vec::new(),
            exit: 0,
        }
    }
}

/// Why a host's input could not be read as an event.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// The host has no event of this name, or none the guard listens to.
    #[error("the event '{0}' is not one this host sends to dibs")]
    UnknownEvent(String),
    /// The input is not in the host's format.
    #[error("the hook input could not be read: {0}")]
    Malformed(String),
    /// The input is in a version of the format this dibs does not read.
    #[error("the hook input uses protocol {0}, which this dibs does not read; update dibs")]
    UnsupportedProtocol(u64),
}

/// Something on this machine that shows a host's integration is installed, checked by
/// `dibs doctor`. The adapter names it; the command line looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// A program on the `PATH`.
    Program {
        /// Its name, without an extension.
        name: &'static str,
        /// What it stands for, shown with the result.
        meaning: &'static str,
    },
    /// A file or directory under the person's home directory.
    HomePath {
        /// The path relative to the home directory, with `/` separators.
        path: &'static str,
        /// What it stands for, shown with the result.
        meaning: &'static str,
    },
}

/// One agent host.
pub trait HostAdapter: Send + Sync {
    /// The host's id.
    fn id(&self) -> HostId;

    /// What the host can carry.
    fn capabilities(&self) -> Capabilities;

    /// Reads the host's input for the event named `event` (the name the host's hook
    /// configuration passes to `dibs hook <host> <event>`).
    ///
    /// # Errors
    /// [`ParseError`] when the event is not one of the host's or the input is not in its format.
    fn parse(&self, event: &str, input: &[u8]) -> Result<HostEvent, ParseError>;

    /// The host's reply carrying `outcome` for the event named `event`: the same name
    /// [`parse`](Self::parse) was given, whether or not the input could be read, so a host whose
    /// reply names its event can still answer an input it could not read. A name the host does
    /// not send gets a reply every one of its events accepts.
    fn render(&self, event: &str, outcome: &GuardOutcome) -> HostReply;

    /// What shows the host's integration is installed on a machine; nothing by default.
    fn probes(&self) -> Vec<Probe> {
        Vec::new()
    }
}
