//! Reference schemes: what the value of an external reference means, per scheme.

use std::collections::BTreeMap;
use std::fmt;

use crate::item::ExternalRef;
use crate::repo::RemoteUrl;

/// What one scheme of external references knows about its values.
pub trait RefScheme: Send + Sync + fmt::Debug {
    /// The repository a value of this scheme names, or `None` when the value names none or does
    /// not have the scheme's shape.
    fn repository(&self, value: &str) -> Option<RemoteUrl>;
}

/// The reference schemes a deployment knows, by scheme name. A scheme it does not know names no
/// repository.
#[derive(Debug, Default)]
pub struct RefSchemeRegistry {
    schemes: BTreeMap<String, Box<dyn RefScheme>>,
}

impl RefSchemeRegistry {
    /// A registry that knows no scheme.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The registry of the schemes this crate ships: `gh`.
    pub fn standard() -> Self {
        Self::empty().with("gh", Box::new(GitHubIssues))
    }

    /// The registry with `scheme` registered under `name`, replacing an earlier one of that name.
    #[must_use]
    pub fn with(mut self, name: &str, scheme: Box<dyn RefScheme>) -> Self {
        self.schemes.insert(name.to_owned(), scheme);
        self
    }

    /// The repository a reference names, through its scheme.
    pub fn repository_of(&self, reference: &ExternalRef) -> Option<RemoteUrl> {
        self.schemes
            .get(reference.scheme())
            .and_then(|scheme| scheme.repository(reference.value()))
    }
}

/// The `gh` scheme: `owner/name#number`, an issue or pull request on github.com, naming the
/// repository `github.com/owner/name`.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitHubIssues;

impl RefScheme for GitHubIssues {
    fn repository(&self, value: &str) -> Option<RemoteUrl> {
        let (repo, number) = value.split_once('#')?;
        let (owner, name) = repo.split_once('/')?;
        let number_ok = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
        let part_ok = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        };
        if !(number_ok && part_ok(owner) && part_ok(name)) {
            return None;
        }
        RemoteUrl::parse(&format!("github.com/{owner}/{name}")).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(text: &str) -> ExternalRef {
        ExternalRef::parse(text).unwrap()
    }

    #[test]
    fn gh_names_its_repository_case_insensitively() {
        let registry = RefSchemeRegistry::standard();
        let remote = registry
            .repository_of(&reference("gh:Acme/Web#12"))
            .unwrap();
        assert_eq!(remote.as_str(), "github.com/acme/web");
    }

    #[test]
    fn a_value_without_the_gh_shape_names_no_repository() {
        let registry = RefSchemeRegistry::standard();
        for text in [
            "gh:acme#12",
            "gh:acme/web",
            "gh:acme/web#",
            "gh:acme/web#x",
            "gh:/web#1",
        ] {
            assert_eq!(registry.repository_of(&reference(text)), None, "{text}");
        }
    }

    #[test]
    fn an_unknown_scheme_names_no_repository() {
        let registry = RefSchemeRegistry::standard();
        assert_eq!(registry.repository_of(&reference("jira:ACME-12")), None);
        assert_eq!(
            RefSchemeRegistry::empty().repository_of(&reference("gh:acme/web#1")),
            None
        );
    }

    #[derive(Debug)]
    struct Fixed;

    impl RefScheme for Fixed {
        fn repository(&self, _value: &str) -> Option<RemoteUrl> {
            RemoteUrl::parse("example.com/acme/search").ok()
        }
    }

    #[test]
    fn a_registered_scheme_is_consulted() {
        let registry = RefSchemeRegistry::empty().with("tracker", Box::new(Fixed));
        let remote = registry
            .repository_of(&reference("tracker:anything"))
            .unwrap();
        assert_eq!(remote.as_str(), "example.com/acme/search");
    }
}
