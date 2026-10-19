//! Reference schemes: what the value of an external reference means, per scheme — its one written
//! form, the page of the tracker that shows it, and the repository it names.

use std::collections::BTreeMap;
use std::fmt;

use thiserror::Error;

use crate::item::ExternalRef;
use crate::repo::RemoteUrl;

/// Why a scheme refuses a value: it does not have the scheme's shape.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("its value is written {shape}")]
pub struct RefRefused {
    /// The shape a value of the scheme has, as a person reads it (`owner/repo#number`).
    pub shape: &'static str,
}

/// What one scheme of external references knows about its values.
pub trait RefScheme: Send + Sync + fmt::Debug {
    /// The one form a value of this scheme is written in, so two spellings of one reference
    /// compare equal.
    ///
    /// # Errors
    /// [`RefRefused`] when the value does not have the scheme's shape.
    fn canonical(&self, value: &str) -> Result<String, RefRefused>;

    /// The address of the tracker's page for a value in its canonical form, when the scheme has
    /// pages.
    fn link(&self, value: &str) -> Option<String>;

    /// The repository a value of this scheme names, or `None` when the value names none or does
    /// not have the scheme's shape.
    fn repository(&self, value: &str) -> Option<RemoteUrl>;
}

/// The schemes this crate ships, by name. [`ExternalRef::parse`] writes every reference of these
/// schemes in its canonical form, so every reference compares in one form wherever it was typed.
const SHIPPED: &[(&str, &dyn RefScheme)] = &[("gh", &GitHubIssues)];

/// The canonical form of `value` under the shipped scheme `scheme`; a scheme this crate does not
/// ship keeps the value as it is.
pub(crate) fn shipped_canonical(scheme: &str, value: &str) -> Result<String, RefRefused> {
    SHIPPED
        .iter()
        .find(|(name, _)| *name == scheme)
        .map_or_else(|| Ok(value.to_owned()), |(_, known)| known.canonical(value))
}

/// The reference schemes a deployment knows, by scheme name. A scheme it does not know names no
/// repository and has no page.
#[derive(Debug, Default)]
pub struct RefSchemeRegistry {
    schemes: BTreeMap<String, Box<dyn RefScheme>>,
}

impl RefSchemeRegistry {
    /// A registry that knows no scheme.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The registry of the schemes this crate ships — the same table [`ExternalRef::parse`] writes
    /// their references by.
    pub fn standard() -> Self {
        SHIPPED
            .iter()
            .fold(Self::empty(), |registry, (name, scheme)| {
                registry.with(name, Box::new(Shipped(*scheme)))
            })
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

    /// The address of the tracker's page for a reference, through its scheme.
    pub fn link(&self, reference: &ExternalRef) -> Option<String> {
        self.schemes
            .get(reference.scheme())
            .and_then(|scheme| scheme.link(reference.value()))
    }
}

/// A scheme of the shipped table, registered by reference.
#[derive(Debug, Clone, Copy)]
struct Shipped(&'static dyn RefScheme);

impl RefScheme for Shipped {
    fn canonical(&self, value: &str) -> Result<String, RefRefused> {
        self.0.canonical(value)
    }

    fn link(&self, value: &str) -> Option<String> {
        self.0.link(value)
    }

    fn repository(&self, value: &str) -> Option<RemoteUrl> {
        self.0.repository(value)
    }
}

/// The `gh` scheme: `owner/name#number`, an issue or pull request on github.com, naming the
/// repository `github.com/owner/name`. GitHub's names are case-insensitive, so the canonical form
/// is the value in lower case.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitHubIssues;

/// The shape of a `gh` value.
const GH_SHAPE: &str = "owner/repo#number";

impl GitHubIssues {
    /// The owner, the name and the number of a value of the `gh` shape.
    fn parts(value: &str) -> Option<(&str, &str, &str)> {
        let (repo, number) = value.split_once('#')?;
        let (owner, name) = repo.split_once('/')?;
        let number_ok = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
        let part_ok = |part: &str| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        };
        (number_ok && part_ok(owner) && part_ok(name)).then_some((owner, name, number))
    }
}

impl RefScheme for GitHubIssues {
    fn canonical(&self, value: &str) -> Result<String, RefRefused> {
        Self::parts(value)
            .map(|_| value.to_ascii_lowercase())
            .ok_or(RefRefused { shape: GH_SHAPE })
    }

    fn link(&self, value: &str) -> Option<String> {
        Self::parts(value).map(|(owner, name, number)| {
            format!("https://github.com/{owner}/{name}/issues/{number}")
        })
    }

    fn repository(&self, value: &str) -> Option<RemoteUrl> {
        let (owner, name, _) = Self::parts(value)?;
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
    fn gh_writes_one_form_whatever_the_case() {
        assert_eq!(
            GitHubIssues.canonical("Acme/Widgets#42"),
            Ok("acme/widgets#42".to_owned())
        );
        assert_eq!(
            reference("gh:Acme/Widgets#42"),
            reference("gh:acme/widgets#42")
        );
        assert_eq!(
            reference("gh:Acme/Widgets#42").to_string(),
            "gh:acme/widgets#42"
        );
    }

    #[test]
    fn gh_refuses_a_value_without_its_shape() {
        for value in [
            "acme#12",
            "acme/web",
            "acme/web#",
            "acme/web#x",
            "/web#1",
            "acme/#1",
            "acme/web/more#1",
            "acme/we b#1",
        ] {
            assert_eq!(
                GitHubIssues.canonical(value),
                Err(RefRefused { shape: GH_SHAPE }),
                "{value}"
            );
            assert_eq!(GitHubIssues.repository(value), None, "{value}");
            assert_eq!(GitHubIssues.link(value), None, "{value}");
        }
    }

    #[test]
    fn the_standard_registry_knows_every_shipped_scheme_as_shipped() {
        let registry = RefSchemeRegistry::standard();
        let names: Vec<&str> = registry.schemes.keys().map(String::as_str).collect();
        let shipped: Vec<&str> = SHIPPED.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, shipped);
        for (name, scheme) in SHIPPED {
            let value = "Acme/Web#3";
            let typed = reference(&format!("{name}:{value}"));
            assert_eq!(Ok(typed.value().to_owned()), scheme.canonical(value));
            assert_eq!(registry.link(&typed), scheme.link(typed.value()));
        }
    }

    #[test]
    fn gh_links_to_the_issue_page() {
        let registry = RefSchemeRegistry::standard();
        assert_eq!(
            registry.link(&reference("gh:Acme/Web#12")).as_deref(),
            Some("https://github.com/acme/web/issues/12")
        );
    }

    #[test]
    fn an_unknown_scheme_passes_through_with_no_link_and_no_repository() {
        let registry = RefSchemeRegistry::standard();
        let jira = reference("jira:ACME-12");
        assert_eq!(jira.value(), "ACME-12");
        assert_eq!(registry.repository_of(&jira), None);
        assert_eq!(registry.link(&jira), None);
        let empty = RefSchemeRegistry::empty();
        assert_eq!(empty.repository_of(&reference("gh:acme/web#1")), None);
        assert_eq!(empty.link(&reference("gh:acme/web#1")), None);
    }

    #[derive(Debug)]
    struct Fixed;

    impl RefScheme for Fixed {
        fn canonical(&self, value: &str) -> Result<String, RefRefused> {
            Ok(value.to_owned())
        }

        fn link(&self, value: &str) -> Option<String> {
            Some(format!("https://tracker.example.com/{value}"))
        }

        fn repository(&self, _value: &str) -> Option<RemoteUrl> {
            RemoteUrl::parse("example.com/acme/search").ok()
        }
    }

    #[test]
    fn a_registered_scheme_is_consulted() {
        let registry = RefSchemeRegistry::empty().with("tracker", Box::new(Fixed));
        let reference = reference("tracker:anything");
        let remote = registry.repository_of(&reference).unwrap();
        assert_eq!(remote.as_str(), "example.com/acme/search");
        assert_eq!(
            registry.link(&reference).as_deref(),
            Some("https://tracker.example.com/anything")
        );
    }
}
