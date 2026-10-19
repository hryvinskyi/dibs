//! A project's slug: the name its members and their tools address it by.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::text::{is_slug, string_value};

/// A project's slug: 1 to 32 characters, lowercase letters, digits and `-`, starting with a letter
/// or digit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectSlug(String);

/// Why a text is not a project slug.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectSlugError {
    /// The text breaks the slug's character rules.
    #[error(
        "a project slug is 1 to 32 characters: lowercase letters, digits and '-', starting with a letter or digit"
    )]
    Malformed,
}

impl ProjectSlug {
    /// Parses a slug.
    ///
    /// # Errors
    /// [`ProjectSlugError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, ProjectSlugError> {
        if is_slug(value, 32, &['-']) {
            Ok(Self(value.to_owned()))
        } else {
            Err(ProjectSlugError::Malformed)
        }
    }
}

string_value!(ProjectSlug, ProjectSlugError);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_slug_table() {
        let long = "p".repeat(32);
        let too_long = "p".repeat(33);
        let cases: &[(&str, bool)] = &[
            ("widgets", true),
            ("acme-2", true),
            ("7", true),
            (long.as_str(), true),
            (too_long.as_str(), false),
            ("", false),
            ("-widgets", false),
            ("Widgets", false),
            ("wid_gets", false),
            ("wid gets", false),
        ];
        for (value, ok) in cases {
            assert_eq!(ProjectSlug::parse(value).is_ok(), *ok, "{value:?}");
        }
    }

    #[test]
    fn project_slug_round_trips_through_text() {
        let slug: ProjectSlug = "widgets".parse().unwrap();
        assert_eq!(slug.to_string(), "widgets");
        assert_eq!(String::from(slug.clone()), "widgets");
        assert_eq!(ProjectSlug::try_from("widgets".to_owned()), Ok(slug));
        assert_eq!(
            serde_json::from_str::<ProjectSlug>("\"Widgets\"").ok(),
            None
        );
    }
}
