//! What a session reports about itself: the machine it runs on, the kind of agent it runs and the
//! checkouts it works in.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::ids::{MemberId, SessionId};
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

/// The kind of agent a session runs, as the session reports it: 1 to 32 characters of lowercase
/// ASCII letters, digits and `-`. It is a stable key that sessions are grouped and shown by; nothing
/// is decided by it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentLabel(String);

/// Why a text is not an agent label.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentLabelError {
    /// The text is empty, longer than 32 characters, has a character other than `a-z`, `0-9`
    /// and `-`, or does not start with a letter or digit.
    #[error(
        "an agent label is 1 to 32 characters: lowercase letters, digits and '-', starting with a letter or digit"
    )]
    Malformed,
}

impl AgentLabel {
    /// The longest label, in characters.
    pub const MAX_CHARS: usize = 32;

    /// Parses an agent label exactly as given; nothing is trimmed or folded to lower case, so one
    /// agent always has one label.
    ///
    /// # Errors
    /// [`AgentLabelError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, AgentLabelError> {
        let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        let starts_well = value.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !starts_well || value.len() > Self::MAX_CHARS || !value.chars().all(allowed) {
            return Err(AgentLabelError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(AgentLabel, AgentLabelError);

/// Which checkout a session works in, as its client names it: 32 lowercase hexadecimal characters,
/// a digest of the machine and the checkout's root directory. It tells one member's checkouts apart
/// and says nothing of where they are; tokens are compared only between sessions of one member.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CheckoutToken(String);

/// Why a text is not a checkout token.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckoutTokenError {
    /// The text is not 32 lowercase hexadecimal characters.
    #[error("a checkout is named by 32 lowercase hexadecimal characters")]
    Malformed,
}

impl CheckoutToken {
    /// How many characters a token has.
    pub const CHARS: usize = 32;

    /// Parses a token exactly as given.
    ///
    /// # Errors
    /// [`CheckoutTokenError::Malformed`] when the text breaks the rule above.
    pub fn parse(value: &str) -> Result<Self, CheckoutTokenError> {
        let hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
        if value.len() == Self::CHARS && value.chars().all(hex) {
            Ok(Self(value.to_owned()))
        } else {
            Err(CheckoutTokenError::Malformed)
        }
    }
}

string_value!(CheckoutToken, CheckoutTokenError);

/// That a session of a member reported working in a checkout, and when it last did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCheckout {
    /// The session.
    pub session: SessionId,
    /// Its member.
    pub member: MemberId,
    /// The checkout.
    pub checkout: CheckoutToken,
    /// When the session last reported it.
    pub reported_at: OffsetDateTime,
}

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
    fn agent_label_table() {
        let longest = "a".repeat(32);
        let too_long = "a".repeat(33);
        let cases: &[(&str, bool)] = &[
            ("agent", true),
            ("code-agent-2", true),
            ("-", false),
            ("-agent", false),
            ("agent-", true),
            (longest.as_str(), true),
            (too_long.as_str(), false),
            ("", false),
            ("Code Agent", false),
            ("Agent", false),
            ("my agent", false),
            (" agent", false),
            ("agent_2", false),
            ("agent.v2", false),
            ("a\tb", false),
            ("агент", false),
        ];
        for (value, ok) in cases {
            assert_eq!(AgentLabel::parse(value).is_ok(), *ok, "{value:?}");
        }
        assert_eq!(AgentLabel::parse("agent").unwrap().as_str(), "agent");
    }

    #[test]
    fn a_checkout_token_is_32_lowercase_hexadecimal_characters() {
        let token = "0123456789abcdef0123456789abcdef";
        let cases: &[(&str, bool)] = &[
            (token, true),
            ("ffffffffffffffffffffffffffffffff", true),
            ("0123456789ABCDEF0123456789ABCDEF", false),
            ("0123456789abcdef0123456789abcdeF", false),
            (&token[..31], false),
            ("0123456789abcdef0123456789abcdef0", false),
            ("", false),
            ("0123456789abcdef0123456789abcdeg", false),
            ("0123456789abcdef 123456789abcdef", false),
            ("0123456789abcdef0123456789abcd\u{e9}", false),
        ];
        for (value, ok) in cases {
            assert_eq!(CheckoutToken::parse(value).is_ok(), *ok, "{value:?}");
        }
        assert_eq!(CheckoutToken::parse(token).unwrap().as_str(), token);
        let read: CheckoutToken = serde_json::from_value(serde_json::json!(token)).unwrap();
        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            serde_json::json!(token)
        );
        assert!(serde_json::from_value::<CheckoutToken>(serde_json::json!("ABC")).is_err());
    }
}
