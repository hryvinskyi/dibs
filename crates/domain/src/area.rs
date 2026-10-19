//! Areas: named sets of path globs inside one repository.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::{PathGlob, RelPath};
use crate::repo::{RepoId, RepoIdError};
use crate::text::{is_slug, string_value};

/// The name of an area inside its repository (`billing`): 1 to 64 characters, lowercase letters,
/// digits, `.`, `_` and `-`, starting with a letter or digit. Two repositories may each have an
/// area of the same name; they are two areas.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AreaName(String);

/// Why a text is not an area name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AreaNameError {
    /// The text breaks the name's character rules.
    #[error(
        "an area name is 1 to 64 characters: lowercase letters, digits, '.', '_' and '-', starting with a letter or digit"
    )]
    Malformed,
}

impl AreaName {
    /// Parses an area name.
    ///
    /// # Errors
    /// [`AreaNameError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, AreaNameError> {
        if is_slug(value, 64, &['.', '_', '-']) {
            Ok(Self(value.to_owned()))
        } else {
            Err(AreaNameError::Malformed)
        }
    }
}

string_value!(AreaName, AreaNameError);

/// An area named together with its repository; shown and stored as `repo:name` (`api:billing`).
/// This is how claims, holds, grants and verdicts name an area.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QualifiedArea {
    /// The repository the area belongs to.
    pub repo: RepoId,
    /// The area's name inside that repository.
    pub name: AreaName,
}

/// Why a text is not a qualified area.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QualifiedAreaError {
    /// The text has no `repo:` prefix.
    #[error("a qualified area is written 'repo:area'")]
    Unqualified,
    /// The repository part is not a repository id.
    #[error(transparent)]
    Repo(#[from] RepoIdError),
    /// The name part is not an area name.
    #[error(transparent)]
    Name(#[from] AreaNameError),
}

impl QualifiedArea {
    /// Parses `repo:area`.
    ///
    /// # Errors
    /// [`QualifiedAreaError`] when either part is invalid or the prefix is missing.
    pub fn parse(value: &str) -> Result<Self, QualifiedAreaError> {
        let (repo, name) = value
            .split_once(':')
            .ok_or(QualifiedAreaError::Unqualified)?;
        Ok(Self {
            repo: RepoId::parse(repo)?,
            name: AreaName::parse(name)?,
        })
    }
}

impl fmt::Display for QualifiedArea {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.repo, self.name)
    }
}

impl TryFrom<String> for QualifiedArea {
    type Error = QualifiedAreaError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<QualifiedArea> for String {
    fn from(value: QualifiedArea) -> String {
        value.to_string()
    }
}

/// An area as declared in a repository's settings: its name and the globs it governs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Area {
    /// The area's name inside its repository.
    pub name: AreaName,
    /// The globs the area governs; at least one.
    pub paths: Vec<PathGlob>,
}

impl Area {
    /// Whether one of the area's globs matches the path.
    pub fn contains(&self, path: &RelPath) -> bool {
        self.paths.iter().any(|glob| glob.is_match(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_name_table() {
        let cases: &[(&str, bool)] = &[
            ("billing", true),
            ("framework-config", true),
            ("v2.api_x", true),
            ("", false),
            ("Billing", false),
            ("api:billing", false),
            ("-billing", false),
            (&"a".repeat(65), false),
        ];
        for (value, ok) in cases {
            assert_eq!(AreaName::parse(value).is_ok(), *ok, "{value:?}");
        }
    }

    #[test]
    fn qualified_area_round_trips() {
        let area = QualifiedArea::parse("api:billing").unwrap();
        assert_eq!(area.repo.as_str(), "api");
        assert_eq!(area.name.as_str(), "billing");
        assert_eq!(area.to_string(), "api:billing");
        assert_eq!(
            QualifiedArea::parse("billing"),
            Err(QualifiedAreaError::Unqualified)
        );
        assert!(matches!(
            QualifiedArea::parse("API:billing"),
            Err(QualifiedAreaError::Repo(_))
        ));
        assert!(matches!(
            QualifiedArea::parse("api:Bill"),
            Err(QualifiedAreaError::Name(_))
        ));
    }

    #[test]
    fn one_name_in_two_repositories_is_two_areas() {
        let api = QualifiedArea::parse("api:billing").unwrap();
        let web = QualifiedArea::parse("web:billing").unwrap();
        assert_ne!(api, web);
        assert_eq!(api.name, web.name);
    }

    #[test]
    fn area_contains_what_any_glob_matches() {
        let area = Area {
            name: AreaName::parse("billing").unwrap(),
            paths: vec![
                PathGlob::parse("packages/billing/**").unwrap(),
                PathGlob::parse("docs/billing.md").unwrap(),
            ],
        };
        assert!(area.contains(&RelPath::parse("packages/billing/src/a.php").unwrap()));
        assert!(area.contains(&RelPath::parse("docs/billing.md").unwrap()));
        assert!(!area.contains(&RelPath::parse("packages/search/a.php").unwrap()));
    }
}
