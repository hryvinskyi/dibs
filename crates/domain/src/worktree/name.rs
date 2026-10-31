//! The names a worktree pool is addressed by: a worktree's name, a pool's id and an agent's key.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::text::{has_control, is_slug, string_value};

/// The name of one worktree of a pool (`worktree2`): 1 to 64 characters, lowercase ASCII letters,
/// digits, `.`, `_` and `-`, the first a letter or a digit. It is also the directory's name, so it
/// never holds a path separator and never starts with a dot.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorktreeName(String);

/// Why a text is not a worktree's name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorktreeNameError {
    /// The text breaks the rules above.
    #[error(
        "a worktree is named in 1 to 64 lowercase letters, digits, '.', '_' or '-', starting with \
         a letter or a digit"
    )]
    Malformed,
}

impl WorktreeName {
    /// The longest name, in characters.
    pub const MAX_CHARS: usize = 64;

    /// Parses a worktree's name exactly as given.
    ///
    /// # Errors
    /// [`WorktreeNameError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, WorktreeNameError> {
        if is_slug(value, Self::MAX_CHARS, &['.', '_', '-']) {
            Ok(Self(value.to_owned()))
        } else {
            Err(WorktreeNameError::Malformed)
        }
    }
}

string_value!(WorktreeName, WorktreeNameError);

/// The id of one pool: a digest of the pool's root directory that the member's machine computes,
/// 1 to 64 lowercase ASCII letters and digits. It tells two pools of one machine apart and is never
/// shown to anyone.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PoolId(String);

/// Why a text is not a pool's id.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PoolIdError {
    /// The text breaks the rule above.
    #[error("a pool's id is 1 to 64 lowercase letters and digits")]
    Malformed,
}

impl PoolId {
    /// The longest id, in characters.
    pub const MAX_CHARS: usize = 64;

    /// Parses a pool's id exactly as given.
    ///
    /// # Errors
    /// [`PoolIdError::Malformed`] when the text breaks the rule above.
    pub fn parse(value: &str) -> Result<Self, PoolIdError> {
        if is_slug(value, Self::MAX_CHARS, &[]) {
            Ok(Self(value.to_owned()))
        } else {
            Err(PoolIdError::Malformed)
        }
    }
}

string_value!(PoolId, PoolIdError);

/// The agent host's own id of one agent a session started: opaque to dibs, 1 to 64 characters with
/// no whitespace or control characters. It tells two agents of one session apart and is never
/// recorded in the project's log.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentKey(String);

/// Why a text is not an agent's key.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentKeyError {
    /// The text breaks the rules above.
    #[error("an agent's key is 1 to 64 characters with no spaces or control characters")]
    Malformed,
}

impl AgentKey {
    /// The longest key, in characters.
    pub const MAX_CHARS: usize = 64;

    /// Parses an agent's key exactly as given.
    ///
    /// # Errors
    /// [`AgentKeyError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, AgentKeyError> {
        let ok = !value.is_empty()
            && value.chars().count() <= Self::MAX_CHARS
            && !has_control(value)
            && !value.chars().any(char::is_whitespace);
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(AgentKeyError::Malformed)
        }
    }
}

string_value!(AgentKey, AgentKeyError);
