//! A member's secret token as the client holds it.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A bearer token: 1 to 512 visible ASCII characters once surrounding whitespace is trimmed. Its
/// `Debug` form never shows the secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Token(String);

/// Why a text is not a token.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TokenError {
    /// The text is empty, too long, or holds a space or a character outside visible ASCII.
    #[error("a token is 1 to 512 visible ASCII characters with no spaces")]
    Malformed,
}

impl Token {
    /// The longest token, in characters.
    pub const MAX_CHARS: usize = 512;

    /// Parses a token, trimming whitespace at both ends (a pasted token often ends in a newline).
    ///
    /// # Errors
    /// [`TokenError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(text: &str) -> Result<Self, TokenError> {
        let text = text.trim();
        let visible = text.chars().all(|c| c.is_ascii_graphic());
        if text.is_empty() || text.len() > Self::MAX_CHARS || !visible {
            return Err(TokenError::Malformed);
        }
        Ok(Self(text.to_owned()))
    }

    /// The secret itself, for the `Authorization` header and the credential stores only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

impl TryFrom<String> for Token {
    type Error = TokenError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Token> for String {
    fn from(value: Token) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_token_is_trimmed_and_never_shown() {
        let token = Token::parse("  dibs_abc123\n").unwrap();
        assert_eq!(token.expose(), "dibs_abc123");
        assert_eq!(format!("{token:?}"), "Token(…)");
    }

    #[test]
    fn spaces_controls_and_empty_text_are_no_token() {
        for text in [
            "",
            "   ",
            "dibs abc",
            "dibs\u{7}abc",
            "dibs_é",
            &"a".repeat(513),
        ] {
            assert_eq!(Token::parse(text), Err(TokenError::Malformed), "{text:?}");
        }
        assert!(Token::parse(&"a".repeat(512)).is_ok());
    }
}
