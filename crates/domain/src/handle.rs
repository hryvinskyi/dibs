//! A member's handle: the short name people type and see.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::text::{is_slug, string_value};

/// A member's handle: 1 to 32 characters, lowercase letters, digits and `-`, starting with a letter
/// or digit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Handle(String);

/// Why a text is not a handle.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HandleError {
    /// The text breaks the handle's character rules.
    #[error(
        "a handle is 1 to 32 characters: lowercase letters, digits and '-', starting with a letter or digit"
    )]
    Malformed,
}

impl Handle {
    /// Parses a handle.
    ///
    /// # Errors
    /// [`HandleError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, HandleError> {
        if is_slug(value, 32, &['-']) {
            Ok(Self(value.to_owned()))
        } else {
            Err(HandleError::Malformed)
        }
    }
}

string_value!(Handle, HandleError);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_table() {
        let long = "a".repeat(32);
        let too_long = "a".repeat(33);
        let cases: &[(&str, bool)] = &[
            ("alice", true),
            ("bob-2", true),
            ("7", true),
            (long.as_str(), true),
            (too_long.as_str(), false),
            ("", false),
            ("-alice", false),
            ("Alice", false),
            ("al_ice", false),
            ("al ice", false),
            ("alicé", false),
        ];
        for (value, ok) in cases {
            assert_eq!(Handle::parse(value).is_ok(), *ok, "{value:?}");
        }
    }

    #[test]
    fn handle_round_trips_through_text() {
        let handle = Handle::parse("alice").unwrap();
        assert_eq!(handle.as_str(), "alice");
        assert_eq!(String::from(handle.clone()), "alice");
        assert_eq!("alice".parse::<Handle>().unwrap(), handle);
        assert_eq!(
            Handle::try_from("ALICE".to_owned()),
            Err(HandleError::Malformed)
        );
    }
}
