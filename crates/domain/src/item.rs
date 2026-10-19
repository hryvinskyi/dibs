//! Work items: how they are keyed, titled and created.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::ItemId;
use crate::member::Role;
use crate::ref_scheme::RefSchemeRegistry;
use crate::repo::{RemoteUrl, RepoId};
use crate::settings::ProjectSettings;
use crate::text::{has_control, string_value};

/// A reference to a work item in an outside tracker, `scheme:value` (`gh:acme/web#12`).
///
/// The scheme is 1 to 16 characters: a lowercase letter, then lowercase letters, digits, `+`, `.`
/// or `-`. The value is 1 to 200 characters with no control characters and no whitespace at either
/// end.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExternalRef {
    scheme: String,
    value: String,
}

/// Why a text is not an external reference.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExternalRefError {
    /// The text has no `:` between scheme and value.
    #[error("an external reference is written 'scheme:value'")]
    MissingScheme,
    /// The scheme breaks its character rules.
    #[error(
        "a reference scheme is 1 to 16 characters: a lowercase letter, then lowercase letters, digits, '+', '.' or '-'"
    )]
    InvalidScheme,
    /// The value is empty, too long, has control characters or whitespace at an end.
    #[error(
        "a reference value is 1 to 200 characters, with no control characters and no spaces at either end"
    )]
    InvalidValue,
}

impl ExternalRef {
    /// Parses `scheme:value`, splitting at the first `:`.
    ///
    /// # Errors
    /// [`ExternalRefError`] naming the part that is invalid.
    pub fn parse(text: &str) -> Result<Self, ExternalRefError> {
        let (scheme, value) = text
            .split_once(':')
            .ok_or(ExternalRefError::MissingScheme)?;
        let mut scheme_chars = scheme.chars();
        let scheme_ok = scheme_chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && scheme.len() <= 16
            && scheme_chars
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+.-".contains(c));
        if !scheme_ok {
            return Err(ExternalRefError::InvalidScheme);
        }
        let value_ok = !value.is_empty()
            && value.chars().count() <= 200
            && !has_control(value)
            && value.trim() == value;
        if !value_ok {
            return Err(ExternalRefError::InvalidValue);
        }
        Ok(Self {
            scheme: scheme.to_owned(),
            value: value.to_owned(),
        })
    }

    /// The scheme (`gh`).
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The value after the scheme (`acme/web#12`).
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for ExternalRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.scheme, self.value)
    }
}

impl TryFrom<String> for ExternalRef {
    type Error = ExternalRefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ExternalRef> for String {
    fn from(value: ExternalRef) -> String {
        value.to_string()
    }
}

/// How a work item is named by the people and tools that refer to it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKey {
    /// By its reference in an outside tracker.
    External(ExternalRef),
    /// By its local id.
    Local(ItemId),
}

impl ItemKey {
    /// Parses a key as people type it: digits are a local id, anything else an external reference.
    ///
    /// # Errors
    /// [`ExternalRefError`] when the text is neither.
    pub fn parse(text: &str) -> Result<Self, ExternalRefError> {
        if !text.is_empty()
            && text.chars().all(|c| c.is_ascii_digit())
            && let Ok(number) = text.parse::<u64>()
        {
            return Ok(Self::Local(ItemId(number)));
        }
        ExternalRef::parse(text).map(Self::External)
    }
}

impl fmt::Display for ItemKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::External(reference) => reference.fmt(f),
            Self::Local(id) => id.fmt(f),
        }
    }
}

/// A work item's title: 1 to 200 characters after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Title(String);

/// Why a text is not a title.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TitleError {
    /// The trimmed text is empty or longer than 200 characters, or has control characters.
    #[error("a title is 1 to 200 characters with no control characters")]
    Malformed,
}

impl Title {
    /// Parses a title, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`TitleError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, TitleError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 200 || has_control(value) {
            return Err(TitleError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Title, TitleError);

/// Where a work item stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Nobody holds it.
    Open,
    /// A member holds it.
    Claimed,
    /// Completed.
    Done,
    /// Given up.
    Dropped,
}

/// The repository a new work item belongs to, as far as its reference says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemRepository {
    /// One of the project's repositories.
    Listed(RepoId),
    /// A repository the project does not list. Only the importer creates such items; they may be
    /// claimed, but no area exists for them.
    Unlisted(RemoteUrl),
    /// The reference names no repository (a local item, or a scheme that names none).
    Unnamed,
}

/// Why a work item may not be created.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CreateRefused {
    /// A person named a repository the project does not list.
    #[error("the reference names the repository {remote}, which this project does not list")]
    UnlistedRepository {
        /// The repository the reference named.
        remote: RemoteUrl,
    },
}

/// Decides whether `by` may create an item with this reference, and which repository it belongs to.
///
/// A person (admin or member) is refused an item in a repository the project does not list, so a
/// typo in a reference never opens a second item beside the real one. The importer is not refused:
/// a blocker it found in another repository must exist to block anything. A project whose settings
/// list no repositories has no remote to compare with, so there every item is accepted as naming
/// none.
///
/// # Errors
/// [`CreateRefused::UnlistedRepository`] for a person naming an unlisted repository.
pub fn decide_create_item(
    reference: Option<&ExternalRef>,
    by: Role,
    settings: &ProjectSettings,
    schemes: &RefSchemeRegistry,
) -> Result<ItemRepository, CreateRefused> {
    let named = reference.and_then(|r| schemes.repository_of(r));
    let Some(remote) = named.filter(|_| settings.lists_repositories()) else {
        return Ok(ItemRepository::Unnamed);
    };
    if let Some(repo) = settings.repo_by_remote(&remote) {
        return Ok(ItemRepository::Listed(repo.id.clone()));
    }
    if by.is_person() {
        Err(CreateRefused::UnlistedRepository { remote })
    } else {
        Ok(ItemRepository::Unlisted(remote))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ref_table() {
        let long_value = format!("x:{}", "v".repeat(200));
        let too_long_value = format!("x:{}", "v".repeat(201));
        type Expected<'a> = Result<(&'a str, &'a str), ExternalRefError>;
        let cases: &[(&str, Expected<'_>)] = &[
            ("gh:acme/web#12", Ok(("gh", "acme/web#12"))),
            ("jira:ACME-1", Ok(("jira", "ACME-1"))),
            ("x+y.z-1:a b", Ok(("x+y.z-1", "a b"))),
            (
                "url:https://example.com/a",
                Ok(("url", "https://example.com/a")),
            ),
            (
                long_value.as_str(),
                Ok(("x", long_value.trim_start_matches("x:"))),
            ),
            ("acme-web-12", Err(ExternalRefError::MissingScheme)),
            (":value", Err(ExternalRefError::InvalidScheme)),
            ("1gh:x", Err(ExternalRefError::InvalidScheme)),
            ("GH:x", Err(ExternalRefError::InvalidScheme)),
            ("abcdefghijklmnopq:x", Err(ExternalRefError::InvalidScheme)),
            ("gh_x:y", Err(ExternalRefError::InvalidScheme)),
            ("gh:", Err(ExternalRefError::InvalidValue)),
            ("gh: x", Err(ExternalRefError::InvalidValue)),
            ("gh:x ", Err(ExternalRefError::InvalidValue)),
            ("gh:a\nb", Err(ExternalRefError::InvalidValue)),
            (too_long_value.as_str(), Err(ExternalRefError::InvalidValue)),
        ];
        for (text, expected) in cases {
            let got = ExternalRef::parse(text);
            match expected {
                Ok((scheme, value)) => {
                    let got = got.unwrap();
                    assert_eq!((got.scheme(), got.value()), (*scheme, *value), "{text:?}");
                    assert_eq!(got.to_string(), *text);
                }
                Err(error) => assert_eq!(got.as_ref(), Err(error), "{text:?}"),
            }
        }
    }

    #[test]
    fn item_key_reads_digits_as_a_local_id() {
        assert_eq!(ItemKey::parse("42").unwrap(), ItemKey::Local(ItemId(42)));
        assert_eq!(
            ItemKey::parse("gh:acme/web#12").unwrap(),
            ItemKey::External(ExternalRef::parse("gh:acme/web#12").unwrap())
        );
        assert_eq!(ItemKey::parse("abc"), Err(ExternalRefError::MissingScheme));
        assert_eq!(ItemKey::parse(""), Err(ExternalRefError::MissingScheme));
        assert_eq!(ItemKey::Local(ItemId(7)).to_string(), "7");
    }

    #[test]
    fn title_table() {
        let cases: &[(&str, Option<&str>)] = &[
            ("Fix the invoice rounding", Some("Fix the invoice rounding")),
            ("  padded  ", Some("padded")),
            ("", None),
            ("   ", None),
            ("tab\tinside", None),
            (&"t".repeat(201), None),
        ];
        for (text, expected) in cases {
            assert_eq!(
                Title::parse(text).ok().as_ref().map(Title::as_str),
                *expected,
                "{text:?}"
            );
        }
        assert!(Title::parse(&"t".repeat(200)).is_ok());
    }

    fn github_repos() -> ProjectSettings {
        crate::testing::settings(
            r#"{"repos":[
                {"id":"api","remote":"https://github.com/acme/api.git"},
                {"id":"web","remote":"git@github.com:acme/web.git"}]}"#,
        )
    }

    fn create(text: Option<&str>, by: Role) -> Result<ItemRepository, CreateRefused> {
        let reference = text.map(|t| ExternalRef::parse(t).unwrap());
        decide_create_item(
            reference.as_ref(),
            by,
            &github_repos(),
            &RefSchemeRegistry::standard(),
        )
    }

    #[test]
    fn an_item_in_a_listed_repository_belongs_to_it() {
        for role in [Role::Admin, Role::Member, Role::Importer] {
            assert_eq!(
                create(Some("gh:Acme/Web#12"), role),
                Ok(ItemRepository::Listed(RepoId::parse("web").unwrap()))
            );
        }
    }

    #[test]
    fn a_person_creating_an_item_in_an_unlisted_repository_is_refused() {
        let remote = RemoteUrl::parse("github.com/acme/wbe").unwrap();
        for role in [Role::Admin, Role::Member] {
            assert_eq!(
                create(Some("gh:acme/wbe#12"), role),
                Err(CreateRefused::UnlistedRepository {
                    remote: remote.clone()
                }),
                "{role:?}"
            );
        }
    }

    #[test]
    fn the_importer_creates_an_item_in_an_unlisted_repository() {
        assert_eq!(
            create(Some("gh:acme/search#3"), Role::Importer),
            Ok(ItemRepository::Unlisted(
                RemoteUrl::parse("github.com/acme/search").unwrap()
            ))
        );
    }

    #[test]
    fn a_reference_naming_no_repository_is_not_checked() {
        for text in [None, Some("jira:ACME-12"), Some("gh:not-an-issue")] {
            assert_eq!(
                create(text, Role::Member),
                Ok(ItemRepository::Unnamed),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_project_without_listed_repositories_accepts_any_reference() {
        let single = crate::testing::settings("{}");
        let reference = ExternalRef::parse("gh:acme/api#1").unwrap();
        assert_eq!(
            decide_create_item(
                Some(&reference),
                Role::Member,
                &single,
                &RefSchemeRegistry::standard()
            ),
            Ok(ItemRepository::Unnamed)
        );
    }
}
