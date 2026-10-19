//! The plain settings document, as a person writes it in a settings file or sends it to the API.
//! Nothing here is validated; [`ProjectSettings::parse`](super::ProjectSettings::parse) does that.

use serde::{Deserialize, Serialize};

use super::EditPolicy;

/// A project's settings as written.
///
/// A project with one repository declares its `areas` and `hot` rules at the top level and lists
/// no `repos`. A project with several lists them under `repos`, each with its own areas and rules;
/// the two layouts are never mixed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsDocument {
    /// How long a claim stays live without a heartbeat (`24h`; 1 hour to 30 days; default 24 hours).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_after: Option<String>,
    /// How often a session sends a heartbeat (`1m`; 1 to 15 minutes; default 1 minute).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_every: Option<String>,
    /// Whether a governed path needs a live claim (`require-claim`, the default) or only advises
    /// (`advise`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_policy: Option<EditPolicy>,
    /// The areas of the project's only repository.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<AreaDocument>,
    /// The hot-file rules of the project's only repository, first match wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hot: Vec<HotRuleDocument>,
    /// The project's repositories, when it has several.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<RepoDocument>,
}

/// One repository as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoDocument {
    /// The repository's id (`api`).
    pub id: String,
    /// Its remote, in any form git accepts (`git@example.com:acme/api.git`).
    pub remote: String,
    /// Its areas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<AreaDocument>,
    /// Its hot-file rules, first match wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hot: Vec<HotRuleDocument>,
}

/// One area as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AreaDocument {
    /// The area's name inside its repository.
    pub name: String,
    /// The globs it governs.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// One hot-file rule as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotRuleDocument {
    /// The globs the rule applies to.
    #[serde(default)]
    pub paths: Vec<String>,
    /// The rule's kind (`regenerate`, `owner`, `warn-shared`, `free`).
    pub kind: String,
    /// The area of an `owner` rule, by its name inside the same repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// A note shown with the verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
