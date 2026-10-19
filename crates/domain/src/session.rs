//! What a session reports about itself: the machine it runs on and the kind of agent it runs.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::text::{has_control, string_value};

/// The name of the machine a session runs on, as the session reported it: 1 to 255 characters with
/// no whitespace or control characters. It is a field of its own and never parsed out of a label.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HostName(String);

/// Why a text is not a host name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostNameError {
    /// The text is empty, longer than 255 characters, or contains whitespace or control characters.
    #[error("a host name is 1 to 255 characters with no spaces or control characters")]
    Malformed,
}

impl HostName {
    /// Parses a host name.
    ///
    /// # Errors
    /// [`HostNameError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, HostNameError> {
        let ok = !value.is_empty()
            && value.chars().count() <= 255
            && !has_control(value)
            && !value.chars().any(char::is_whitespace);
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(HostNameError::Malformed)
        }
    }
}

string_value!(HostName, HostNameError);

/// The kind of agent a session runs, as the session reports it. Nothing is decided by it: 1 to 64
/// characters after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentLabel(String);

/// Why a text is not an agent label.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentLabelError {
    /// The trimmed text is empty, longer than 64 characters, or has control characters.
    #[error("an agent label is 1 to 64 characters with no control characters")]
    Malformed,
}

impl AgentLabel {
    /// Parses an agent label, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`AgentLabelError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, AgentLabelError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 64 || has_control(value) {
            return Err(AgentLabelError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(AgentLabel, AgentLabelError);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_name_table() {
        let long = "h".repeat(255);
        let too_long = "h".repeat(256);
        let cases: &[(&str, bool)] = &[
            ("laptop", true),
            ("build-01.example.com", true),
            ("DESKTOP-7Q2", true),
            (long.as_str(), true),
            (too_long.as_str(), false),
            ("", false),
            ("my laptop", false),
            ("a\tb", false),
            ("a\u{0}", false),
        ];
        for (value, ok) in cases {
            assert_eq!(HostName::parse(value).is_ok(), *ok, "{value:?}");
        }
    }

    #[test]
    fn agent_label_is_trimmed_and_bounded() {
        assert_eq!(AgentLabel::parse(" agent ").unwrap().as_str(), "agent");
        assert!(AgentLabel::parse(&"a".repeat(64)).is_ok());
        assert_eq!(
            AgentLabel::parse(&"a".repeat(65)),
            Err(AgentLabelError::Malformed)
        );
        assert_eq!(AgentLabel::parse("a\tb"), Err(AgentLabelError::Malformed));
        assert_eq!(AgentLabel::parse("  "), Err(AgentLabelError::Malformed));
    }
}
