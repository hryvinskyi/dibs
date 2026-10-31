//! What the guard answers a host.
//!
//! There is no "allow". A host has a permission system of its own, which asks its person before a
//! tool runs or lets the tool run as the person configured; the guard never answers in its place.
//! It either has no objection, which leaves the host's own decision standing, or it denies.
//!
//! A host that waits for a worktree gets a value instead: the directory to work in, or the reason
//! there is none.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::text::Text;

/// The guard's answer to one event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GuardOutcome {
    /// The guard does not stand in the way; the host decides as it would without the guard.
    NoObjection {
        /// Text for the agent: a brief, a note about the files, a delivered message.
        context: Option<Text>,
        /// One line for the person: the guard could not decide, and why.
        notice: Option<Text>,
    },
    /// The tool must not run; the reason says why and what to do instead.
    Deny {
        /// The reason, for the agent.
        reason: Text,
    },
    /// The worktree a host asked for: the directory to work in. Given only for a wanted worktree,
    /// and only to a host that serves worktrees.
    Worktree {
        /// The worktree's directory, absolute.
        path: PathBuf,
        /// One line for the person: where the worktree came from, when that is worth saying.
        notice: Option<Text>,
    },
    /// No worktree can be given: the host's creation of one fails with this reason. Given only
    /// for a wanted worktree, and only to a host that serves worktrees.
    NoWorktree {
        /// Why, and what to do about it.
        reason: Text,
    },
}

impl GuardOutcome {
    /// No objection with nothing to say.
    pub fn silent() -> Self {
        Self::NoObjection {
            context: None,
            notice: None,
        }
    }

    /// No objection, with one line for the person.
    pub fn notice(notice: impl Into<Text>) -> Self {
        Self::NoObjection {
            context: None,
            notice: Some(notice.into()),
        }
    }

    /// No objection, with text for the agent.
    pub fn context(context: impl Into<Text>) -> Self {
        Self::NoObjection {
            context: Some(context.into()),
            notice: None,
        }
    }

    /// Whether the outcome stops the tool.
    pub fn denies(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }
}
