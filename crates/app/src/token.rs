//! Secrets handed to people once — API tokens and browser session ids — and the forms in which they
//! are kept: a hash, and a short prefix that names a token without revealing it.

use std::fmt;

use thiserror::Error;

/// The marker every secret starts with, so secret scanners can recognise one.
pub const SECRET_MARKER: &str = "dibs_";

const MIN_RANDOM_CHARS: usize = 32;
const MAX_RANDOM_CHARS: usize = 128;
const PREFIX_CHARS: usize = 8;

const BASE64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// The character of the URL-safe base64 alphabet for the low six bits of `value`.
fn base64_char(value: u32) -> char {
    let index = usize::try_from(value & 0x3f).unwrap_or_default();
    char::from(BASE64_URL[index])
}

/// A secret: `dibs_` followed by 32 to 128 characters of `A-Z`, `a-z`, `0-9`, `_` and `-`.
///
/// It never prints: `Debug` shows `***` and there is no `Display`. The text is reached only through
/// [`SecretToken::expose`], by the code that hands it to its owner.
#[derive(PartialEq, Eq)]
pub struct SecretToken(String);

/// Why a text is not a secret.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SecretTokenError {
    /// The text does not have the shape of a secret.
    #[error("a token is 'dibs_' followed by 32 to 128 letters, digits, '_' or '-'")]
    Malformed,
}

impl SecretToken {
    /// Parses a secret.
    ///
    /// # Errors
    /// [`SecretTokenError::Malformed`] when the text does not have the shape above.
    pub fn parse(value: &str) -> Result<Self, SecretTokenError> {
        let random = value
            .strip_prefix(SECRET_MARKER)
            .ok_or(SecretTokenError::Malformed)?;
        let ok = (MIN_RANDOM_CHARS..=MAX_RANDOM_CHARS).contains(&random.len())
            && random.chars().all(is_secret_char);
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(SecretTokenError::Malformed)
        }
    }

    /// The secret made of 256 random bits: the marker, then the bits in the URL-safe base64
    /// alphabet without padding (43 characters).
    pub fn from_random(bits: &[u8; 32]) -> Self {
        let mut text = String::with_capacity(SECRET_MARKER.len() + 43);
        text.push_str(SECRET_MARKER);
        let mut buffer: u32 = 0;
        let mut held: u32 = 0;
        for byte in bits {
            buffer = (buffer << 8) | u32::from(*byte);
            held += 8;
            while held >= 6 {
                held -= 6;
                text.push(base64_char(buffer >> held));
            }
        }
        if held > 0 {
            text.push(base64_char(buffer << (6 - held)));
        }
        Self(text)
    }

    /// The secret's text, for handing it to its owner once.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The first characters after the marker: a name for the secret that shows nothing of it worth
    /// knowing.
    pub fn prefix(&self) -> TokenPrefix {
        let random = self.0.get(SECRET_MARKER.len()..).unwrap_or_default();
        TokenPrefix(random.chars().take(PREFIX_CHARS).collect())
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken(***)")
    }
}

/// The hash a secret is stored and looked up by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenHash(String);

impl TokenHash {
    /// Wraps a hash as the hasher wrote it.
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The hash's text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The first 8 characters of a token after its marker: shown in lists and used to revoke a token.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenPrefix(String);

/// Why a text is not a token prefix.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TokenPrefixError {
    /// The text is not 8 letters, digits, `_` or `-`.
    #[error("a token prefix is the 8 characters after 'dibs_'")]
    Malformed,
}

impl TokenPrefix {
    /// Parses a prefix as a person types it; a leading `dibs_` is accepted and dropped.
    ///
    /// # Errors
    /// [`TokenPrefixError::Malformed`] when the text is not 8 such characters.
    pub fn parse(value: &str) -> Result<Self, TokenPrefixError> {
        let value = value.strip_prefix(SECRET_MARKER).unwrap_or(value);
        if value.len() == PREFIX_CHARS && value.chars().all(is_secret_char) {
            Ok(Self(value.to_owned()))
        } else {
            Err(TokenPrefixError::Malformed)
        }
    }

    /// The prefix's text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TokenPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "dibs_abcdefgh0123456789ABCDEFGHIJKLMNOP_-xyz";

    #[test]
    fn secret_shape_table() {
        let longest = format!("dibs_{}", "a".repeat(128));
        let too_long = format!("dibs_{}", "a".repeat(129));
        let too_short = format!("dibs_{}", "a".repeat(31));
        let cases: &[(&str, bool)] = &[
            (GOOD, true),
            (longest.as_str(), true),
            (too_long.as_str(), false),
            (too_short.as_str(), false),
            ("abcdefgh0123456789ABCDEFGHIJKLMNOP", false),
            ("dibs_abcdefgh0123456789ABCDEFGHIJKLMNOP!", false),
            ("dibs_abcdefgh 0123456789ABCDEFGHIJKLMNOP", false),
            ("", false),
        ];
        for (value, ok) in cases {
            assert_eq!(SecretToken::parse(value).is_ok(), *ok, "{value:?}");
        }
    }

    #[test]
    fn a_secret_from_random_bits_is_base64_after_the_marker() {
        let zeros = SecretToken::from_random(&[0; 32]);
        assert_eq!(zeros.expose(), format!("dibs_{}", "A".repeat(43)));
        let ones = SecretToken::from_random(&[0xff; 32]);
        assert_eq!(ones.expose(), format!("dibs_{}8", "_".repeat(42)));
        let mut bits = [0u8; 32];
        bits[..3].copy_from_slice(&[0xfb, 0xef, 0xbe]);
        let mixed = SecretToken::from_random(&bits);
        assert!(mixed.expose().starts_with("dibs_----"));
        for token in [&zeros, &ones, &mixed] {
            assert_eq!(SecretToken::parse(token.expose()).as_ref(), Ok(token));
        }
    }

    #[test]
    fn a_secret_never_prints() {
        let token = SecretToken::parse(GOOD).unwrap();
        assert_eq!(format!("{token:?}"), "SecretToken(***)");
        assert_eq!(token.expose(), GOOD);
    }

    #[test]
    fn the_prefix_is_the_eight_characters_after_the_marker() {
        let token = SecretToken::parse(GOOD).unwrap();
        assert_eq!(token.prefix().as_str(), "abcdefgh");
        assert_eq!(TokenPrefix::parse("abcdefgh"), Ok(token.prefix()));
        assert_eq!(TokenPrefix::parse("dibs_abcdefgh"), Ok(token.prefix()));
        assert_eq!(
            TokenPrefix::parse("abcdefg"),
            Err(TokenPrefixError::Malformed)
        );
        assert_eq!(
            TokenPrefix::parse("abcdefg!"),
            Err(TokenPrefixError::Malformed)
        );
    }
}
