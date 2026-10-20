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
    /// The project's slots: shared actions one member does at a time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<SlotDocument>,
}

/// One slot as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotDocument {
    /// Its name (`deploy:prod`).
    pub name: String,
    /// The longest one hold lasts before it is renewed (`30m`; 1 minute to 24 hours).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hold: Option<String>,
    /// How long an offer to the first waiter stands (`5m`, the default; 1 minute to 1 hour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer_for: Option<String>,
    /// How long a waiter keeps their place without a sign of life (`2h`, the default; 1 minute to
    /// 24 hours).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wait: Option<String>,
    /// The commands that need the slot, first match wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CommandDocument>,
    /// Whether its commands run only inside `dibs slot run`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub run_only: bool,
    /// The group of slots it excludes, one hold across the group at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// The branches nobody but the holder may push while it is held (`api:refs/heads/main`; a
    /// project with one repository may write `refs/heads/main`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub freezes: Vec<String>,
    /// A note shown with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One command pattern as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDocument {
    /// A regular expression matched against one simple command (`^deploy\.sh( |$)`).
    pub matches: String,
    /// A regular expression that excludes a command `matches` matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless: Option<String>,
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
