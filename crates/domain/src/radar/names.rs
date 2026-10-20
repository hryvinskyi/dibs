//! The names the radar reads from a checkout and a forge: a branch, and the commit a branch's
//! changes are measured from.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::text::string_value;

/// The short name of a branch (`main`, `bob/42-refund-rounding`), as git names the branch a checkout
/// is on. The rules are the part of git's own that matter here: 1 to 255 bytes; no whitespace,
/// control character, `~`, `^`, `:`, `?`, `*`, `[` or `\`; no `..`, `@{` or `//`; it neither starts
/// with `-` or `/` nor ends with `/`, `.` or `.lock`, and no segment starts with `.`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BranchName(String);

/// Why a text is not a branch name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BranchNameError {
    /// The text breaks the rules above.
    #[error("'{0}' is not a branch name git accepts")]
    Malformed(String),
}

impl BranchName {
    /// The longest branch name, in bytes.
    pub const MAX_BYTES: usize = 255;

    /// Parses a branch name.
    ///
    /// # Errors
    /// [`BranchNameError::Malformed`] when the text breaks the rules of [`BranchName`].
    pub fn parse(value: &str) -> Result<Self, BranchNameError> {
        let forbidden = |c: char| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c);
        let ok = !value.is_empty()
            && value.len() <= Self::MAX_BYTES
            && !value.chars().any(forbidden)
            && !value.contains("..")
            && !value.contains("@{")
            && !value.contains("//")
            && !value.starts_with(['-', '/'])
            && !value.ends_with(['/', '.'])
            && !value.ends_with(".lock")
            && value != "@"
            && value.split('/').all(|segment| !segment.starts_with('.'));
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(BranchNameError::Malformed(value.chars().take(80).collect()))
        }
    }
}

impl BranchName {
    /// Parses a branch as a forge names a pull request's base or head: `main`, or the full ref
    /// `refs/heads/main`.
    ///
    /// # Errors
    /// [`BranchNameError::Malformed`] when what is left is no branch name.
    pub fn parse_ref(value: &str) -> Result<Self, BranchNameError> {
        Self::parse(value.strip_prefix("refs/heads/").unwrap_or(value))
    }
}

string_value!(BranchName, BranchNameError);

/// A commit, by its hexadecimal name: 4 to 64 hexadecimal digits, kept in lowercase, so an
/// abbreviated name and the full one of a SHA-1 or a SHA-256 repository are both accepted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitSha(String);

/// Why a text is not a commit name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommitShaError {
    /// The text is not 4 to 64 hexadecimal digits.
    #[error("a commit is named by 4 to 64 hexadecimal digits")]
    Malformed,
}

impl CommitSha {
    /// Parses a commit name, trimming whitespace at both ends and lowercasing it.
    ///
    /// # Errors
    /// [`CommitShaError::Malformed`] when the text is not 4 to 64 hexadecimal digits.
    pub fn parse(value: &str) -> Result<Self, CommitShaError> {
        let value = value.trim();
        if (4..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit()) {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(CommitShaError::Malformed)
        }
    }
}

string_value!(CommitSha, CommitShaError);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names_follow_the_rules_git_holds_them_to() {
        for good in [
            "main",
            "bob/42-refund-rounding",
            "release-2.4",
            "feature/x_y",
            "a",
            "Ünïcode",
        ] {
            assert_eq!(BranchName::parse(good).unwrap().as_str(), good, "{good:?}");
        }
        let long = "b".repeat(256);
        for bad in [
            "",
            " main",
            "main ",
            "with space",
            "tab\there",
            "a..b",
            "a@{1}",
            "a//b",
            "-main",
            "/main",
            "main/",
            "main.",
            "main.lock",
            "a/.hidden",
            ".hidden",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[b",
            "a\\b",
            "@",
            long.as_str(),
        ] {
            assert!(BranchName::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            BranchName::parse(&"b".repeat(255)).unwrap().as_str().len(),
            255
        );
    }

    #[test]
    fn a_commit_is_four_to_sixty_four_hex_digits_kept_in_lowercase() {
        assert_eq!(CommitSha::parse(" ABCD12 ").unwrap().as_str(), "abcd12");
        assert!(CommitSha::parse(&"f".repeat(64)).is_ok());
        for bad in ["", "abc", "abcg", &"f".repeat(65), "ab cd"] {
            assert!(CommitSha::parse(bad).is_err(), "{bad:?}");
        }
    }
}
