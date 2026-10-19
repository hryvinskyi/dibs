//! Conversations: subjects, bodies, recipients, and who may read a thread.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::MemberId;
use crate::member::Role;
use crate::text::{has_control, string_value};

/// A thread's subject: 1 to 200 characters after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Subject(String);

/// Why a text is not a subject.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SubjectError {
    /// The trimmed text is empty, longer than 200 characters, or has control characters.
    #[error("a subject is 1 to 200 characters with no control characters")]
    Malformed,
}

impl Subject {
    /// Parses a subject, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`SubjectError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, SubjectError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 200 || has_control(value) {
            return Err(SubjectError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Subject, SubjectError);

/// A message's body: plain text of 1 to 16384 bytes with no NUL, not only whitespace. Line breaks
/// and other whitespace are kept as written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Body(String);

/// Why a text is not a body.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BodyError {
    /// The text is empty or only whitespace.
    #[error("a message is empty")]
    Empty,
    /// The text is longer than 16384 bytes.
    #[error("a message is longer than 16384 bytes")]
    TooLong,
    /// The text contains a NUL character.
    #[error("a message contains a NUL character")]
    Nul,
}

impl Body {
    /// The largest body, in bytes.
    pub const MAX_BYTES: usize = 16_384;

    /// Parses a body.
    ///
    /// # Errors
    /// [`BodyError`] naming the rule broken.
    pub fn parse(value: &str) -> Result<Self, BodyError> {
        if value.trim().is_empty() {
            return Err(BodyError::Empty);
        }
        if value.len() > Self::MAX_BYTES {
            return Err(BodyError::TooLong);
        }
        if value.contains('\0') {
            return Err(BodyError::Nul);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Body, BodyError);

/// The label of the session that wrote a message (`laptop/4f2a91c0`, `web`): 1 to 64 characters
/// after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionLabel(String);

/// Why a text is not a session label.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionLabelError {
    /// The trimmed text is empty, longer than 64 characters, or has control characters.
    #[error("a session label is 1 to 64 characters with no control characters")]
    Malformed,
}

impl SessionLabel {
    /// Parses a label, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`SessionLabelError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, SessionLabelError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 64 || has_control(value) {
            return Err(SessionLabelError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(SessionLabel, SessionLabelError);

/// Whom a thread is addressed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipients {
    /// Every person in the project.
    Project,
    /// These members; built by [`Recipients::members`], so never empty.
    Members(BTreeSet<MemberId>),
}

/// Why a recipient list is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecipientsError {
    /// No member was named.
    #[error("a message names at least one recipient")]
    Empty,
}

impl Recipients {
    /// Addresses these members.
    ///
    /// # Errors
    /// [`RecipientsError::Empty`] when no member is named.
    pub fn members(members: BTreeSet<MemberId>) -> Result<Self, RecipientsError> {
        if members.is_empty() {
            return Err(RecipientsError::Empty);
        }
        Ok(Self::Members(members))
    }
}

/// Whether `reader` may read a thread `author` started with these recipients: its author, its
/// recipients, and for a project-addressed thread every person. An integration bot reads no thread.
pub fn may_read(author: MemberId, recipients: &Recipients, reader: MemberId, role: Role) -> bool {
    if !role.is_person() {
        return false;
    }
    if reader == author {
        return true;
    }
    match recipients {
        Recipients::Project => true,
        Recipients::Members(members) => members.contains(&reader),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ALICE, BOB, CAROL};

    #[test]
    fn subject_table() {
        assert_eq!(
            Subject::parse("  Who owns billing?  ").unwrap().as_str(),
            "Who owns billing?"
        );
        assert!(Subject::parse(&"s".repeat(200)).is_ok());
        for bad in ["", "   ", "two\nlines", &"s".repeat(201)] {
            assert_eq!(Subject::parse(bad), Err(SubjectError::Malformed), "{bad:?}");
        }
    }

    #[test]
    fn body_table() {
        let kept = "first line\n\n  indented\tand tabbed\n";
        assert_eq!(Body::parse(kept).unwrap().as_str(), kept);
        let cyrillic = "ж".repeat(Body::MAX_BYTES / 2);
        assert!(
            Body::parse(&cyrillic).is_ok(),
            "16 KiB of two-byte characters fit"
        );
        assert_eq!(
            Body::parse(&format!("{cyrillic}x")),
            Err(BodyError::TooLong)
        );
        assert_eq!(Body::parse(""), Err(BodyError::Empty));
        assert_eq!(Body::parse(" \n\t"), Err(BodyError::Empty));
        assert_eq!(Body::parse("a\u{0}b"), Err(BodyError::Nul));
    }

    #[test]
    fn session_label_table() {
        assert_eq!(
            SessionLabel::parse("laptop/4f2a91c0").unwrap().as_str(),
            "laptop/4f2a91c0"
        );
        assert!(SessionLabel::parse("web").is_ok());
        assert!(SessionLabel::parse(&"l".repeat(64)).is_ok());
        for bad in ["", "  ", "a\tb", &"l".repeat(65)] {
            assert_eq!(
                SessionLabel::parse(bad),
                Err(SessionLabelError::Malformed),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn recipients_name_someone() {
        assert_eq!(
            Recipients::members(BTreeSet::new()),
            Err(RecipientsError::Empty)
        );
        assert!(Recipients::members(BTreeSet::from([BOB])).is_ok());
    }

    #[test]
    fn who_may_read_a_thread() {
        let to_bob = Recipients::members(BTreeSet::from([BOB])).unwrap();
        assert!(may_read(ALICE, &to_bob, ALICE, Role::Member), "the author");
        assert!(may_read(ALICE, &to_bob, BOB, Role::Member), "a recipient");
        assert!(
            !may_read(ALICE, &to_bob, CAROL, Role::Member),
            "anyone else"
        );
        assert!(
            !may_read(ALICE, &to_bob, CAROL, Role::Admin),
            "an admin is not a recipient"
        );
        assert!(
            may_read(ALICE, &Recipients::Project, CAROL, Role::Member),
            "the project"
        );
        assert!(
            !may_read(ALICE, &Recipients::Project, CAROL, Role::Importer),
            "an integration reads no thread"
        );
    }
}
