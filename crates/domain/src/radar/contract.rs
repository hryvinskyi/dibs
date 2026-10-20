//! Published contracts: the paths of a repository other work depends on, and who consumes them.

use std::fmt;

use thiserror::Error;

use crate::area::{AreaName, QualifiedArea};
use crate::paths::{PathGlob, RelPath};
use crate::repo::RepoId;

/// Paths of one repository that the project declares as a published contract (`schema/**`), and
/// whoever consumes them besides the areas that depend on the contract's area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// The globs of the contract's paths; at least one.
    pub paths: Vec<PathGlob>,
    /// Whoever consumes the contract, in declaration order.
    pub consumers: Vec<Consumer>,
}

impl Contract {
    /// Whether one of the contract's globs matches the path.
    pub fn covers(&self, path: &RelPath) -> bool {
        self.paths.iter().any(|glob| glob.is_match(path))
    }
}

/// Who consumes a contract: every path of a repository (`web:*`), or one area (`web:checkout`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Consumer {
    /// Every path of the repository, in an area or not.
    Repo(RepoId),
    /// The paths of one area.
    Area(QualifiedArea),
}

/// Why a text is not a consumer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConsumerError {
    /// The text is none of the forms.
    #[error("a consumer is written 'repo:*', 'repo:area' or 'area'")]
    Malformed,
}

impl Consumer {
    /// Parses `repo:*`, `repo:area`, or a bare `area` of `home`, the repository that declares the
    /// contract.
    ///
    /// # Errors
    /// [`ConsumerError::Malformed`] when the text is none of those forms.
    pub fn parse(text: &str, home: &RepoId) -> Result<Self, ConsumerError> {
        match text.split_once(':') {
            Some((repo, "*")) => RepoId::parse(repo)
                .map(Self::Repo)
                .map_err(|_| ConsumerError::Malformed),
            Some(_) => QualifiedArea::parse(text)
                .map(Self::Area)
                .map_err(|_| ConsumerError::Malformed),
            None => AreaName::parse(text)
                .map(|name| {
                    Self::Area(QualifiedArea {
                        repo: home.clone(),
                        name,
                    })
                })
                .map_err(|_| ConsumerError::Malformed),
        }
    }

    /// The repository the consumer is in.
    pub fn repo(&self) -> &RepoId {
        match self {
            Self::Repo(repo) => repo,
            Self::Area(area) => &area.repo,
        }
    }

    /// Whether a path in `area` is one the consumer reads.
    pub fn covers_area(&self, area: &QualifiedArea) -> bool {
        match self {
            Self::Repo(repo) => area.repo == *repo,
            Self::Area(own) => own == area,
        }
    }

    /// Whether a path of `repo` that lies in no area is one the consumer reads.
    pub fn covers_unzoned(&self, repo: &RepoId) -> bool {
        matches!(self, Self::Repo(own) if own == repo)
    }
}

impl fmt::Display for Consumer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repo(repo) => write!(f, "{repo}:*"),
            Self::Area(area) => write!(f, "{area}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::area;

    #[test]
    fn a_consumer_is_a_whole_repository_an_area_or_an_area_of_the_contracts_own_repository() {
        let api = RepoId::parse("api").unwrap();
        let web = RepoId::parse("web").unwrap();
        let whole = Consumer::parse("web:*", &api).unwrap();
        assert_eq!(whole, Consumer::Repo(web.clone()));
        assert_eq!(whole.to_string(), "web:*");
        assert!(whole.covers_area(&area("web:checkout")));
        assert!(!whole.covers_area(&area("api:checkout")));
        assert!(whole.covers_unzoned(&web) && !whole.covers_unzoned(&api));
        let one = Consumer::parse("web:checkout", &api).unwrap();
        assert!(one.covers_area(&area("web:checkout")));
        assert!(!one.covers_area(&area("web:billing")));
        assert!(!one.covers_unzoned(&web));
        let bare = Consumer::parse("search", &api).unwrap();
        assert_eq!(bare, Consumer::Area(area("api:search")));
        assert_eq!(bare.repo(), &api);
        for bad in ["", "Web:*", "web:Bad", "a:b:c", "*", "web:**"] {
            assert_eq!(
                Consumer::parse(bad, &api),
                Err(ConsumerError::Malformed),
                "{bad:?}"
            );
        }
    }
}
