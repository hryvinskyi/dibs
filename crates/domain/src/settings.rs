//! A project's settings: timing, edit policy, and per repository its areas and hot-file rules.

mod document;
mod duration;
mod radar;
mod review;
mod slots;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Duration;

pub use document::{
    AreaDocument, CommandDocument, ContractDocument, HotRuleDocument, PrStatusDocument,
    ProtectedRefDocument, RepoDocument, SettingsDocument, SlotDocument,
};
pub use duration::{DurationError, format_duration, parse_duration};
pub use slots::SlotError;

use crate::area::{Area, AreaName, AreaNameError, QualifiedArea};
use crate::handle::Handle;
use crate::hot_rule::{
    HotRuleConfig, HotRuleRefused, HotRuleRegistry, HotRuleSpec, Note, NoteError,
};
use crate::paths::{PathGlob, PathGlobError, RelPath, RepoPath};
use crate::pr_status::PrStatusSettings;
use crate::protection::{ProtectedRef, ProtectedTarget, ProtectedTargetError, RefKind};
use crate::radar::Contract;
use crate::repo::{RemoteUrl, RemoteUrlError, RepoId, RepoIdError};
use crate::review::PrRef;
use crate::slot::{RepoRef, SlotCatalog};

/// Whether a governed path needs a live claim.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EditPolicy {
    /// A member with no live claim gets `needs-claim` on a governed path.
    #[default]
    RequireClaim,
    /// A member with no live claim may edit governed paths nobody holds.
    Advise,
}

/// One repository of a project: its remote, areas, hot-file rules, protected branches, how its
/// pull requests get a status, its published contracts and which of its areas depend on which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSpec {
    /// The repository's id.
    pub id: RepoId,
    /// Its normalised remote. The one repository of a project that lists none has no remote.
    pub remote: Option<RemoteUrl>,
    /// Its areas, in declaration order.
    pub areas: Vec<Area>,
    /// Its hot-file rules, in declaration order.
    pub hot: Vec<HotRuleSpec>,
    /// Its branches and tags only some members may push to, in declaration order, each ref or
    /// pattern once.
    pub protected_refs: Vec<ProtectedRef>,
    /// Whether and how its pull requests get a status.
    pub pr_status: PrStatusSettings,
    /// Its published contracts, in declaration order.
    pub contracts: Vec<Contract>,
    /// For each of its areas that depends on others, those areas, of any repository; an area
    /// never depends on itself.
    pub area_deps: BTreeMap<AreaName, BTreeSet<QualifiedArea>>,
}

impl RepoSpec {
    /// The area of this name.
    pub fn area(&self, name: &AreaName) -> Option<&Area> {
        self.areas.iter().find(|area| area.name == *name)
    }

    /// The first area, in declaration order, that contains the path.
    pub fn area_for(&self, path: &RelPath) -> Option<&Area> {
        self.areas.iter().find(|area| area.contains(path))
    }

    /// The first hot-file rule, in declaration order, whose globs match the path.
    pub fn hot_rule_for(&self, path: &RelPath) -> Option<&HotRuleSpec> {
        self.hot.iter().find(|rule| rule.matches(path))
    }

    /// Whether an area or a hot-file rule matches the path.
    pub fn is_governed(&self, path: &RelPath) -> bool {
        self.hot_rule_for(path).is_some() || self.area_for(path).is_some()
    }

    /// Whether it protects a branch, by its name or by a pattern; protected tags alone do not
    /// count.
    pub fn protects_branch(&self) -> bool {
        self.protected_refs
            .iter()
            .any(|rule| rule.target.kind() == RefKind::Branch)
    }

    /// Whether it protects branches and yet neither `main` nor `master`, the names a repository's
    /// default branch usually has: a merge then delivers its work only into a protected branch, so
    /// a pull request merged into the default branch completes no claim.
    pub fn leaves_main_unprotected(&self) -> bool {
        let covered = |name: &str| {
            self.protected_refs
                .iter()
                .any(|rule| rule.target.kind() == RefKind::Branch && rule.target.covers(name))
        };
        self.protects_branch() && !covered("refs/heads/main") && !covered("refs/heads/master")
    }

    /// The branch `name` (`refs/heads/main`, or `main`) of this repository.
    pub fn branch(&self, name: &str) -> Option<RepoRef> {
        let full = if name.starts_with("refs/") {
            name.to_owned()
        } else {
            format!("refs/heads/{name}")
        };
        RepoRef::of(self.id.clone(), &full)
    }

    /// The contracts, in declaration order, whose globs match the path.
    pub fn contracts_for<'a>(&'a self, path: &'a RelPath) -> impl Iterator<Item = &'a Contract> {
        self.contracts
            .iter()
            .filter(move |contract| contract.covers(path))
    }

    /// The area name qualified by this repository.
    pub fn qualify(&self, name: &AreaName) -> QualifiedArea {
        QualifiedArea {
            repo: self.id.clone(),
            name: name.clone(),
        }
    }
}

/// A project's validated settings. Built only by [`ProjectSettings::parse`], so every value in it
/// holds the rules stated there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSettings {
    stale_after: Duration,
    heartbeat_every: Duration,
    yield_escalates_after: Duration,
    edit_policy: EditPolicy,
    raised_per_hour: u32,
    lists_repositories: bool,
    repos: Vec<RepoSpec>,
    slots: SlotCatalog,
    arbiters: BTreeSet<Handle>,
}

/// Why a settings document is refused. Each message names the setting at fault.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SettingsError {
    /// `stale_after` is not a duration.
    #[error("stale_after: {0}")]
    StaleAfter(DurationError),
    /// `stale_after` is outside 1 hour to 30 days.
    #[error("stale_after is between 1 hour and 30 days")]
    StaleAfterOutOfRange,
    /// `heartbeat_every` is not a duration.
    #[error("heartbeat_every: {0}")]
    HeartbeatEvery(DurationError),
    /// `heartbeat_every` is outside 1 to 15 minutes.
    #[error("heartbeat_every is between 1 and 15 minutes")]
    HeartbeatEveryOutOfRange,
    /// `yield_escalates_after` is not a duration.
    #[error("yield_escalates_after: {0}")]
    YieldEscalatesAfter(DurationError),
    /// `yield_escalates_after` is outside 5 minutes to 24 hours.
    #[error("yield_escalates_after is between 5 minutes and 24 hours")]
    YieldEscalatesAfterOutOfRange,
    /// `raised_per_hour` is outside 1 to 1000.
    #[error("raised_per_hour is between 1 and 1000")]
    RaisedPerHourOutOfRange,
    /// Top-level areas, hot rules, protected refs, a pull-request status, contracts or area
    /// dependencies sit beside a `repos` list.
    #[error(
        "areas, hot rules, protected refs, pr_status, contracts and area_deps are declared either at the top level, for a project with one repository, or inside each entry of repos, never both"
    )]
    MixedLayout,
    /// A repository id is invalid.
    #[error("repository '{id}': {error}")]
    RepoId {
        /// The id as written.
        id: String,
        /// What is wrong with it.
        #[source]
        error: RepoIdError,
    },
    /// Two repositories share an id.
    #[error("repository '{repo}' is declared twice")]
    DuplicateRepo {
        /// The id.
        repo: RepoId,
    },
    /// A repository's remote is invalid.
    #[error("repository '{repo}': {error}")]
    Remote {
        /// The repository.
        repo: RepoId,
        /// What is wrong with its remote.
        #[source]
        error: RemoteUrlError,
    },
    /// Two repositories share a remote.
    #[error("two repositories have the remote {remote}")]
    DuplicateRemote {
        /// The remote.
        remote: RemoteUrl,
    },
    /// An area name is invalid.
    #[error("area '{name}' of repository '{repo}': {error}")]
    AreaName {
        /// The repository.
        repo: RepoId,
        /// The name as written.
        name: String,
        /// What is wrong with it.
        #[source]
        error: AreaNameError,
    },
    /// Two areas of one repository share a name.
    #[error("area {area} is declared twice")]
    DuplicateArea {
        /// The area.
        area: QualifiedArea,
    },
    /// An area has no glob.
    #[error("area {area} has no paths")]
    AreaWithoutPaths {
        /// The area.
        area: QualifiedArea,
    },
    /// An area's glob is invalid.
    #[error("area {area}, path '{glob}': {error}")]
    AreaGlob {
        /// The area.
        area: QualifiedArea,
        /// The glob as written.
        glob: String,
        /// What is wrong with it.
        #[source]
        error: PathGlobError,
    },
    /// A hot-file rule has no glob.
    #[error("hot rule {number} of repository '{repo}' has no paths")]
    HotRuleWithoutPaths {
        /// The repository.
        repo: RepoId,
        /// The rule's position, counting from 1.
        number: usize,
    },
    /// A hot-file rule's glob is invalid.
    #[error("hot rule {number} of repository '{repo}', path '{glob}': {error}")]
    HotRuleGlob {
        /// The repository.
        repo: RepoId,
        /// The rule's position, counting from 1.
        number: usize,
        /// The glob as written.
        glob: String,
        /// What is wrong with it.
        #[source]
        error: PathGlobError,
    },
    /// A hot-file rule names an area its repository does not declare.
    #[error(
        "hot rule {number} of repository '{repo}' names the area '{area}', which that repository does not declare"
    )]
    HotRuleArea {
        /// The repository.
        repo: RepoId,
        /// The rule's position, counting from 1.
        number: usize,
        /// The area as written.
        area: String,
    },
    /// A hot-file rule's note is invalid.
    #[error("hot rule {number} of repository '{repo}': {error}")]
    HotRuleNote {
        /// The repository.
        repo: RepoId,
        /// The rule's position, counting from 1.
        number: usize,
        /// What is wrong with the note.
        #[source]
        error: NoteError,
    },
    /// A hot-file rule's kind is unknown or refuses its configuration.
    #[error("hot rule {number} of repository '{repo}': {error}")]
    HotRule {
        /// The repository.
        repo: RepoId,
        /// The rule's position, counting from 1.
        number: usize,
        /// Why the rule was refused.
        #[source]
        error: HotRuleRefused,
    },
    /// A slot is refused.
    #[error("slot '{slot}': {error}")]
    Slot {
        /// The slot's name as written.
        slot: String,
        /// What is wrong with it.
        #[source]
        error: SlotError,
    },
    /// An arbiter is not a handle.
    #[error("arbiters: '{text}' is not a member's handle")]
    Arbiter {
        /// The text as written.
        text: String,
    },
    /// A protected ref names no branches and no tags as its rules allow.
    #[error("protected ref '{text}' of repository '{repo}': {error}")]
    ProtectedRef {
        /// The repository.
        repo: RepoId,
        /// The ref as written.
        text: String,
        /// What is wrong with it.
        #[source]
        error: ProtectedTargetError,
    },
    /// A ref or pattern is protected twice.
    #[error("protected ref '{text}' of repository '{repo}' is declared twice")]
    DuplicateProtectedRef {
        /// The repository.
        repo: RepoId,
        /// The ref as written.
        text: String,
    },
    /// A pusher is not a handle.
    #[error("protected ref '{target}': pusher '{text}' is not a member's handle")]
    Pusher {
        /// The protected ref.
        target: ProtectedTarget,
        /// The text as written.
        text: String,
    },
    /// A pull-request status's context is not one.
    #[error("pr_status of repository '{repo}': {error}")]
    StatusContext {
        /// The repository.
        repo: RepoId,
        /// What is wrong with it.
        #[source]
        error: crate::pr_status::StatusContextError,
    },
    /// A contract has no glob.
    #[error("contract {number} of repository '{repo}' has no paths")]
    ContractWithoutPaths {
        /// The repository.
        repo: RepoId,
        /// The contract's position, counting from 1.
        number: usize,
    },
    /// A contract's glob is invalid.
    #[error("contract {number} of repository '{repo}', path '{glob}': {error}")]
    ContractGlob {
        /// The repository.
        repo: RepoId,
        /// The contract's position, counting from 1.
        number: usize,
        /// The glob as written.
        glob: String,
        /// What is wrong with it.
        #[source]
        error: PathGlobError,
    },
    /// A contract's consumer is malformed or names what the project does not declare.
    #[error(
        "contract {number} of repository '{repo}': the consumer '{text}' is not written 'repo:*', 'repo:area' or 'area', or names a repository or area the project does not declare"
    )]
    ContractConsumer {
        /// The repository.
        repo: RepoId,
        /// The contract's position, counting from 1.
        number: usize,
        /// The consumer as written.
        text: String,
    },
    /// An area dependency is declared for an area the repository does not declare.
    #[error("area_deps of repository '{repo}': '{area}' is not an area of that repository")]
    AreaDepArea {
        /// The repository.
        repo: RepoId,
        /// The area as written.
        area: String,
    },
    /// An area depends on an area that is malformed, not declared, or itself.
    #[error(
        "area_deps of repository '{repo}': area '{area}' depends on '{text}', which is not another area the project declares"
    )]
    AreaDep {
        /// The repository.
        repo: RepoId,
        /// The depending area as written.
        area: String,
        /// The dependency as written.
        text: String,
    },
}

/// Why an area named by a person does not resolve.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AreaLookupError {
    /// The text is neither `repo:area` nor an area name.
    #[error("'{text}' is not an area name")]
    Malformed {
        /// The text as given.
        text: String,
    },
    /// No repository declares such an area.
    #[error("the project declares no area '{text}'")]
    NotFound {
        /// The text as given.
        text: String,
    },
    /// Several repositories declare an area of that name.
    #[error("several repositories declare an area '{text}'; name one of them as repo:area")]
    Ambiguous {
        /// The text as given.
        text: String,
        /// The areas it could mean.
        candidates: Vec<QualifiedArea>,
    },
}

/// Why a checkout cannot be governed by these settings.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckoutRefused {
    /// The project lists repositories and the checkout does not say which one it is.
    #[error("this project has several repositories; the checkout's configuration names its repo")]
    RepoRequired,
    /// The checkout names a repository the settings do not list.
    #[error("the project lists no repository '{repo}'")]
    UnknownRepo {
        /// The repository named.
        repo: RepoId,
    },
}

const DEFAULT_STALE_AFTER: Duration = Duration::hours(24);
const DEFAULT_HEARTBEAT_EVERY: Duration = Duration::minutes(1);
const STALE_AFTER_RANGE: (Duration, Duration) = (Duration::hours(1), Duration::days(30));
const HEARTBEAT_EVERY_RANGE: (Duration, Duration) = (Duration::minutes(1), Duration::minutes(15));
const DEFAULT_YIELD_ESCALATES_AFTER: Duration = Duration::minutes(30);
const YIELD_ESCALATES_AFTER_RANGE: (Duration, Duration) =
    (Duration::minutes(5), Duration::hours(24));

/// How many high or urgent messages one member may write within an hour, unless the settings say.
pub const DEFAULT_RAISED_PER_HOUR: u32 = 10;

const RAISED_PER_HOUR_RANGE: std::ops::RangeInclusive<u32> = 1..=1000;

/// The parts of one repository as written, before validation.
struct RepoInput<'a> {
    id: &'a str,
    remote: Option<&'a str>,
    areas: &'a [AreaDocument],
    hot: &'a [HotRuleDocument],
    protected_refs: &'a [ProtectedRefDocument],
    pr_status: Option<&'a PrStatusDocument>,
    contracts: &'a [ContractDocument],
    area_deps: &'a BTreeMap<String, Vec<String>>,
}

impl ProjectSettings {
    /// Validates a settings document.
    ///
    /// The rules: `stale_after` between 1 hour and 30 days (default 24 hours); `heartbeat_every`
    /// between 1 and 15 minutes (default 1 minute); `yield_escalates_after` between 5 minutes and
    /// 24 hours (default 30 minutes); `raised_per_hour` between 1 and 1000 (default
    /// 10); either top-level areas and rules or a `repos`
    /// list, never both; repository ids and remotes unique; area names unique within their
    /// repository, each area with at least one glob; each hot rule with at least one glob, a kind
    /// the registry knows, and, for kinds that take one, an area its repository declares; slot
    /// names unique, each slot with `max_hold` between 1 minute and 24 hours, `offer_for` between
    /// 1 minute and 1 hour (default 5 minutes), `max_wait` between 1 minute and 24 hours (default
    /// 2 hours), at most 20 command patterns that compile within their limits, and frozen refs of
    /// declared repositories, each a branch or a prefix of branches; arbiters named by handle, each
    /// once; per repository, protected refs written `refs/heads/<branch>`, `refs/heads/<prefix>*`,
    /// `refs/tags/<name>` or `refs/tags/<prefix>*`, each once, their pushers named by handle, and a
    /// pull-request status whose context is 1 to 100 characters (off, `dibs` and not describing the
    /// holder by default); per repository, contracts each with at least one valid glob and
    /// consumers that name a listed repository (`repo:*`) or a declared area, and area
    /// dependencies from a declared area of the repository to other declared areas. Whether an arbiter or a pusher is a member is for the project to say, not
    /// the document.
    ///
    /// # Errors
    /// The first [`SettingsError`] found, in document order.
    pub fn parse(
        document: &SettingsDocument,
        hot_rules: &HotRuleRegistry,
    ) -> Result<Self, SettingsError> {
        let stale_after = parse_in_range(
            document.stale_after.as_deref(),
            DEFAULT_STALE_AFTER,
            STALE_AFTER_RANGE,
            SettingsError::StaleAfter,
            SettingsError::StaleAfterOutOfRange,
        )?;
        let heartbeat_every = parse_in_range(
            document.heartbeat_every.as_deref(),
            DEFAULT_HEARTBEAT_EVERY,
            HEARTBEAT_EVERY_RANGE,
            SettingsError::HeartbeatEvery,
            SettingsError::HeartbeatEveryOutOfRange,
        )?;
        let yield_escalates_after = parse_in_range(
            document.yield_escalates_after.as_deref(),
            DEFAULT_YIELD_ESCALATES_AFTER,
            YIELD_ESCALATES_AFTER_RANGE,
            SettingsError::YieldEscalatesAfter,
            SettingsError::YieldEscalatesAfterOutOfRange,
        )?;
        let raised_per_hour = document.raised_per_hour.unwrap_or(DEFAULT_RAISED_PER_HOUR);
        if !RAISED_PER_HOUR_RANGE.contains(&raised_per_hour) {
            return Err(SettingsError::RaisedPerHourOutOfRange);
        }
        let lists_repositories = !document.repos.is_empty();
        let top_level_repo = !document.areas.is_empty()
            || !document.hot.is_empty()
            || !document.protected_refs.is_empty()
            || document.pr_status.is_some()
            || !document.contracts.is_empty()
            || !document.area_deps.is_empty();
        if lists_repositories && top_level_repo {
            return Err(SettingsError::MixedLayout);
        }
        let inputs: Vec<RepoInput<'_>> = if lists_repositories {
            document
                .repos
                .iter()
                .map(|repo| RepoInput {
                    id: &repo.id,
                    remote: Some(&repo.remote),
                    areas: &repo.areas,
                    hot: &repo.hot,
                    protected_refs: &repo.protected_refs,
                    pr_status: repo.pr_status.as_ref(),
                    contracts: &repo.contracts,
                    area_deps: &repo.area_deps,
                })
                .collect()
        } else {
            vec![RepoInput {
                id: RepoId::IMPLICIT,
                remote: None,
                areas: &document.areas,
                hot: &document.hot,
                protected_refs: &document.protected_refs,
                pr_status: document.pr_status.as_ref(),
                contracts: &document.contracts,
                area_deps: &document.area_deps,
            }]
        };
        let mut repos: Vec<RepoSpec> = Vec::with_capacity(inputs.len());
        for input in &inputs {
            let repo = parse_repo(input, hot_rules)?;
            if repos.iter().any(|known| known.id == repo.id) {
                return Err(SettingsError::DuplicateRepo { repo: repo.id });
            }
            if let Some(remote) = &repo.remote
                && repos
                    .iter()
                    .any(|known| known.remote.as_ref() == Some(remote))
            {
                return Err(SettingsError::DuplicateRemote {
                    remote: remote.clone(),
                });
            }
            repos.push(repo);
        }
        radar::check_references(&repos)?;
        let slots = slots::parse_slots(&document.slots, &repos, lists_repositories)
            .map_err(|(slot, error)| SettingsError::Slot { slot, error })?;
        let arbiters = document
            .arbiters
            .iter()
            .map(|text| {
                Handle::parse(text).map_err(|_| SettingsError::Arbiter { text: text.clone() })
            })
            .collect::<Result<BTreeSet<Handle>, _>>()?;
        Ok(Self {
            stale_after,
            heartbeat_every,
            yield_escalates_after,
            edit_policy: document.edit_policy.unwrap_or_default(),
            raised_per_hour,
            lists_repositories,
            repos,
            slots,
            arbiters,
        })
    }

    /// The members who settle what agents do not agree on, by handle.
    pub fn arbiters(&self) -> &BTreeSet<Handle> {
        &self.arbiters
    }

    /// How long a claim stays live without a heartbeat.
    pub fn stale_after(&self) -> Duration {
        self.stale_after
    }

    /// How often a session sends a heartbeat.
    pub fn heartbeat_every(&self) -> Duration {
        self.heartbeat_every
    }

    /// How long a request to hand an area over from debt work waits before it is escalated to the
    /// arbiters.
    pub fn yield_escalates_after(&self) -> Duration {
        self.yield_escalates_after
    }

    /// The edit policy.
    pub fn edit_policy(&self) -> EditPolicy {
        self.edit_policy
    }

    /// How many high or urgent messages one member may write within an hour.
    pub fn raised_per_hour(&self) -> u32 {
        self.raised_per_hour
    }

    /// Whether the settings list their repositories, rather than describing one implicit repository.
    pub fn lists_repositories(&self) -> bool {
        self.lists_repositories
    }

    /// The slots, in declaration order.
    pub fn slots(&self) -> &SlotCatalog {
        &self.slots
    }

    /// The repositories, in declaration order; never empty.
    pub fn repos(&self) -> &[RepoSpec] {
        &self.repos
    }

    /// The same settings with every protected tag left out and the protected branches kept: what
    /// a reader that knows protected branches only can parse.
    #[must_use]
    pub fn without_protected_tags(&self) -> Self {
        let mut kept = self.clone();
        for repo in &mut kept.repos {
            repo.protected_refs
                .retain(|rule| rule.target.kind() == RefKind::Branch);
        }
        kept
    }

    /// The same settings with every branch pattern left out — each protected ref that covers
    /// branches by a prefix, and each frozen ref of a slot that does — and the branches named
    /// exactly, the tags and the slots themselves kept: what a reader that knows branches by their
    /// exact names only can parse.
    #[must_use]
    pub fn without_branch_patterns(&self) -> Self {
        let mut kept = self.clone();
        for repo in &mut kept.repos {
            repo.protected_refs.retain(|rule| {
                rule.target.kind() == RefKind::Tag || rule.target.prefix().is_none()
            });
        }
        let slots = self
            .slots
            .slots()
            .iter()
            .cloned()
            .map(|mut spec| {
                spec.freezes.retain(|frozen| frozen.prefix().is_none());
                spec
            })
            .collect();
        kept.slots = SlotCatalog::new(slots);
        kept
    }

    /// The repository of this id.
    pub fn repo(&self, id: &RepoId) -> Option<&RepoSpec> {
        self.repos.iter().find(|repo| repo.id == *id)
    }

    /// The repository with this remote.
    pub fn repo_by_remote(&self, remote: &RemoteUrl) -> Option<&RepoSpec> {
        self.repos
            .iter()
            .find(|repo| repo.remote.as_ref() == Some(remote))
    }

    /// The repository a pull request belongs to: the one whose remote's path is the pull request's
    /// (`acme/api` for `acme/api#12`), or, in a project that lists no repositories, its one
    /// repository, whatever the pull request names.
    pub fn repo_of_pr(&self, pr: &PrRef) -> Option<&RepoSpec> {
        if !self.lists_repositories {
            return self.repos.first();
        }
        self.repos.iter().find(|repo| {
            repo.remote
                .as_ref()
                .is_some_and(|remote| remote.path() == pr.repo_path())
        })
    }

    /// A repository named as an integration names it: by its id (`api`) or by its remote's path
    /// (`acme/api`).
    pub fn repo_named(&self, text: &str) -> Option<&RepoSpec> {
        let path = text.trim().to_ascii_lowercase();
        self.repos.iter().find(|repo| {
            repo.id.as_str() == path
                || repo
                    .remote
                    .as_ref()
                    .is_some_and(|remote| remote.path() == path)
        })
    }

    /// Whether the area `dependent` is declared as depending on the area `on`.
    pub fn depends(&self, dependent: &QualifiedArea, on: &QualifiedArea) -> bool {
        self.repo(&dependent.repo)
            .and_then(|repo| repo.area_deps.get(&dependent.name))
            .is_some_and(|targets| targets.contains(on))
    }

    /// The declared area.
    pub fn area(&self, area: &QualifiedArea) -> Option<&Area> {
        self.repo(&area.repo).and_then(|repo| repo.area(&area.name))
    }

    /// Resolves an area as a person names it: `repo:area`, or a bare name that exactly one
    /// repository declares.
    ///
    /// # Errors
    /// [`AreaLookupError`] when the text is malformed, names nothing, or is ambiguous.
    pub fn resolve_area(&self, text: &str) -> Result<QualifiedArea, AreaLookupError> {
        let malformed = || AreaLookupError::Malformed {
            text: text.to_owned(),
        };
        let not_found = || AreaLookupError::NotFound {
            text: text.to_owned(),
        };
        if text.contains(':') {
            let area = QualifiedArea::parse(text).map_err(|_| malformed())?;
            return self.area(&area).map(|_| area.clone()).ok_or_else(not_found);
        }
        let name = AreaName::parse(text).map_err(|_| malformed())?;
        let mut candidates: Vec<QualifiedArea> = self
            .repos
            .iter()
            .filter(|repo| repo.area(&name).is_some())
            .map(|repo| repo.qualify(&name))
            .collect();
        match candidates.len() {
            0 => Err(not_found()),
            1 => Ok(candidates.remove(0)),
            _ => Err(AreaLookupError::Ambiguous {
                text: text.to_owned(),
                candidates,
            }),
        }
    }

    /// The repository a checkout governs, given the repository id its configuration names. A project
    /// that lists no repositories has one, which a checkout naming none (or naming it) governs.
    ///
    /// # Errors
    /// [`CheckoutRefused`] when a project with repositories gets no id, or the id is not listed.
    pub fn checkout_repo(&self, repo: Option<&RepoId>) -> Result<&RepoSpec, CheckoutRefused> {
        match repo {
            Some(id) => self
                .repo(id)
                .ok_or_else(|| CheckoutRefused::UnknownRepo { repo: id.clone() }),
            None if self.lists_repositories => Err(CheckoutRefused::RepoRequired),
            None => self.repos.first().ok_or(CheckoutRefused::RepoRequired),
        }
    }

    /// Whether an area or hot-file rule of the path's repository matches it. A path of a repository
    /// the settings do not list is not governed.
    pub fn is_governed(&self, path: &RepoPath) -> bool {
        self.repo(&path.repo)
            .is_some_and(|repo| repo.is_governed(&path.path))
    }

    /// The settings written back as a document that parses to the same settings.
    pub fn to_document(&self) -> SettingsDocument {
        let areas = |repo: &RepoSpec| -> Vec<AreaDocument> {
            repo.areas
                .iter()
                .map(|area| AreaDocument {
                    name: area.name.to_string(),
                    paths: area.paths.iter().map(ToString::to_string).collect(),
                })
                .collect()
        };
        let hot = |repo: &RepoSpec| -> Vec<HotRuleDocument> {
            repo.hot
                .iter()
                .map(|rule| HotRuleDocument {
                    paths: rule.paths().iter().map(ToString::to_string).collect(),
                    kind: rule.kind().to_string(),
                    area: rule.area().map(|area| area.name.to_string()),
                    note: rule.note().map(ToString::to_string),
                })
                .collect()
        };
        let protected = |repo: &RepoSpec| review::protected_documents(&repo.protected_refs);
        let status = |repo: &RepoSpec| review::pr_status_document(&repo.pr_status);
        let contracts = radar::contract_documents;
        let area_deps = radar::area_deps_document;
        let mut document = SettingsDocument {
            stale_after: Some(format_duration(self.stale_after)),
            heartbeat_every: Some(format_duration(self.heartbeat_every)),
            yield_escalates_after: (self.yield_escalates_after != DEFAULT_YIELD_ESCALATES_AFTER)
                .then(|| format_duration(self.yield_escalates_after)),
            edit_policy: Some(self.edit_policy),
            raised_per_hour: (self.raised_per_hour != DEFAULT_RAISED_PER_HOUR)
                .then_some(self.raised_per_hour),
            slots: slots::slot_documents(&self.slots),
            arbiters: self.arbiters.iter().map(ToString::to_string).collect(),
            ..SettingsDocument::default()
        };
        if self.lists_repositories {
            document.repos = self
                .repos
                .iter()
                .map(|repo| RepoDocument {
                    id: repo.id.to_string(),
                    remote: repo
                        .remote
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    areas: areas(repo),
                    hot: hot(repo),
                    protected_refs: protected(repo),
                    pr_status: status(repo),
                    contracts: contracts(repo),
                    area_deps: area_deps(repo),
                })
                .collect();
        } else if let Some(repo) = self.repos.first() {
            document.areas = areas(repo);
            document.hot = hot(repo);
            document.protected_refs = protected(repo);
            document.pr_status = status(repo);
            document.contracts = contracts(repo);
            document.area_deps = area_deps(repo);
        }
        document
    }
}

fn parse_in_range(
    text: Option<&str>,
    default: Duration,
    (min, max): (Duration, Duration),
    malformed: fn(DurationError) -> SettingsError,
    out_of_range: SettingsError,
) -> Result<Duration, SettingsError> {
    let value = match text {
        Some(text) => parse_duration(text).map_err(malformed)?,
        None => default,
    };
    if value < min || value > max {
        return Err(out_of_range);
    }
    Ok(value)
}

fn parse_repo(
    input: &RepoInput<'_>,
    hot_rules: &HotRuleRegistry,
) -> Result<RepoSpec, SettingsError> {
    let id = RepoId::parse(input.id).map_err(|error| SettingsError::RepoId {
        id: input.id.to_owned(),
        error,
    })?;
    let remote = input
        .remote
        .map(|text| {
            RemoteUrl::parse(text).map_err(|error| SettingsError::Remote {
                repo: id.clone(),
                error,
            })
        })
        .transpose()?;
    let mut areas: Vec<Area> = Vec::with_capacity(input.areas.len());
    let mut names = BTreeSet::new();
    for written in input.areas {
        let area = parse_area(&id, written)?;
        if !names.insert(area.name.clone()) {
            return Err(SettingsError::DuplicateArea {
                area: QualifiedArea {
                    repo: id,
                    name: area.name,
                },
            });
        }
        areas.push(area);
    }
    let protected_refs = review::parse_protected_refs(&id, input.protected_refs)?;
    let pr_status = review::parse_pr_status(&id, input.pr_status)?;
    let contracts = radar::parse_contracts(&id, input.contracts)?;
    let mut repo = RepoSpec {
        id,
        remote,
        areas,
        hot: Vec::with_capacity(input.hot.len()),
        protected_refs,
        pr_status,
        contracts,
        area_deps: BTreeMap::new(),
    };
    repo.area_deps = radar::parse_area_deps(&repo, input.area_deps)?;
    for (index, written) in input.hot.iter().enumerate() {
        let rule = parse_hot_rule(&repo, index + 1, written, hot_rules)?;
        repo.hot.push(rule);
    }
    Ok(repo)
}

fn parse_area(repo: &RepoId, written: &AreaDocument) -> Result<Area, SettingsError> {
    let name = AreaName::parse(&written.name).map_err(|error| SettingsError::AreaName {
        repo: repo.clone(),
        name: written.name.clone(),
        error,
    })?;
    let qualified = || QualifiedArea {
        repo: repo.clone(),
        name: name.clone(),
    };
    if written.paths.is_empty() {
        return Err(SettingsError::AreaWithoutPaths { area: qualified() });
    }
    let paths = written
        .paths
        .iter()
        .map(|glob| {
            PathGlob::parse(glob).map_err(|error| SettingsError::AreaGlob {
                area: qualified(),
                glob: glob.clone(),
                error,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Area { name, paths })
}

fn parse_hot_rule(
    repo: &RepoSpec,
    number: usize,
    written: &HotRuleDocument,
    hot_rules: &HotRuleRegistry,
) -> Result<HotRuleSpec, SettingsError> {
    if written.paths.is_empty() {
        return Err(SettingsError::HotRuleWithoutPaths {
            repo: repo.id.clone(),
            number,
        });
    }
    let paths = written
        .paths
        .iter()
        .map(|glob| {
            PathGlob::parse(glob).map_err(|error| SettingsError::HotRuleGlob {
                repo: repo.id.clone(),
                number,
                glob: glob.clone(),
                error,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rule_error = |error| SettingsError::HotRule {
        repo: repo.id.clone(),
        number,
        error,
    };
    let kind = hot_rules.kind(&written.kind).map_err(rule_error)?;
    let area = written
        .area
        .as_deref()
        .map(|text| {
            AreaName::parse(text)
                .ok()
                .filter(|name| repo.area(name).is_some())
                .map(|name| repo.qualify(&name))
                .ok_or_else(|| SettingsError::HotRuleArea {
                    repo: repo.id.clone(),
                    number,
                    area: text.to_owned(),
                })
        })
        .transpose()?;
    let note = written
        .note
        .as_deref()
        .map(|text| {
            Note::parse(text).map_err(|error| SettingsError::HotRuleNote {
                repo: repo.id.clone(),
                number,
                error,
            })
        })
        .transpose()?;
    let rule = hot_rules
        .build(&HotRuleConfig {
            kind: &kind,
            area: area.as_ref(),
            note: note.as_ref(),
        })
        .map_err(rule_error)?;
    Ok(HotRuleSpec::new(paths, kind, area, note, rule))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{document, settings};

    fn refusal(json: &str) -> SettingsError {
        ProjectSettings::parse(&document(json), &HotRuleRegistry::standard()).unwrap_err()
    }

    #[test]
    fn an_empty_document_gets_the_defaults_and_one_implicit_repository() {
        let parsed = settings("{}");
        assert_eq!(parsed.stale_after(), Duration::hours(24));
        assert_eq!(parsed.heartbeat_every(), Duration::minutes(1));
        assert_eq!(parsed.yield_escalates_after(), Duration::minutes(30));
        assert_eq!(parsed.edit_policy(), EditPolicy::RequireClaim);
        assert!(!parsed.lists_repositories());
        assert_eq!(parsed.repos().len(), 1);
        assert_eq!(parsed.repos()[0].id, RepoId::implicit());
        assert_eq!(parsed.repos()[0].remote, None);
    }

    #[test]
    fn timing_bounds() {
        let cases: &[(&str, Option<SettingsError>)] = &[
            (r#"{"stale_after":"1h"}"#, None),
            (r#"{"stale_after":"30d"}"#, None),
            (
                r#"{"stale_after":"59m"}"#,
                Some(SettingsError::StaleAfterOutOfRange),
            ),
            (
                r#"{"stale_after":"30d1s"}"#,
                Some(SettingsError::StaleAfterOutOfRange),
            ),
            (
                r#"{"stale_after":"a day"}"#,
                Some(SettingsError::StaleAfter(DurationError::Malformed)),
            ),
            (r#"{"heartbeat_every":"1m"}"#, None),
            (r#"{"heartbeat_every":"15m"}"#, None),
            (
                r#"{"heartbeat_every":"59s"}"#,
                Some(SettingsError::HeartbeatEveryOutOfRange),
            ),
            (
                r#"{"heartbeat_every":"16m"}"#,
                Some(SettingsError::HeartbeatEveryOutOfRange),
            ),
            (
                r#"{"heartbeat_every":"1"}"#,
                Some(SettingsError::HeartbeatEvery(DurationError::Malformed)),
            ),
            (r#"{"yield_escalates_after":"5m"}"#, None),
            (r#"{"yield_escalates_after":"24h"}"#, None),
            (
                r#"{"yield_escalates_after":"4m59s"}"#,
                Some(SettingsError::YieldEscalatesAfterOutOfRange),
            ),
            (
                r#"{"yield_escalates_after":"24h1s"}"#,
                Some(SettingsError::YieldEscalatesAfterOutOfRange),
            ),
            (
                r#"{"yield_escalates_after":"soon"}"#,
                Some(SettingsError::YieldEscalatesAfter(DurationError::Malformed)),
            ),
            (r#"{"raised_per_hour":1}"#, None),
            (r#"{"raised_per_hour":1000}"#, None),
            (
                r#"{"raised_per_hour":0}"#,
                Some(SettingsError::RaisedPerHourOutOfRange),
            ),
            (
                r#"{"raised_per_hour":1001}"#,
                Some(SettingsError::RaisedPerHourOutOfRange),
            ),
        ];
        for (json, expected) in cases {
            let got = ProjectSettings::parse(&document(json), &HotRuleRegistry::standard()).err();
            assert_eq!(&got, expected, "{json}");
        }
    }

    #[test]
    fn the_limit_on_raised_messages_defaults_to_ten_and_is_written_only_when_changed() {
        let default = settings("{}");
        assert_eq!(default.raised_per_hour(), DEFAULT_RAISED_PER_HOUR);
        assert_eq!(default.to_document().raised_per_hour, None);
        let set = settings(r#"{"raised_per_hour":25}"#);
        assert_eq!(set.raised_per_hour(), 25);
        assert_eq!(set.to_document().raised_per_hour, Some(25));
    }

    #[test]
    fn arbiters_are_handles_kept_once_in_order() {
        assert!(settings("{}").arbiters().is_empty());
        assert!(settings("{}").to_document().arbiters.is_empty());
        let set = settings(r#"{"arbiters":["bob","alice","bob"]}"#);
        let names: Vec<&str> = set.arbiters().iter().map(Handle::as_str).collect();
        assert_eq!(names, ["alice", "bob"]);
        assert_eq!(set.to_document().arbiters, ["alice", "bob"]);
        assert_eq!(
            ProjectSettings::parse(
                &document(r#"{"arbiters":["Not A Handle"]}"#),
                &HotRuleRegistry::standard()
            ),
            Err(SettingsError::Arbiter {
                text: "Not A Handle".to_owned()
            })
        );
    }

    #[test]
    fn edit_policy_is_read() {
        assert_eq!(
            settings(r#"{"edit_policy":"advise"}"#).edit_policy(),
            EditPolicy::Advise
        );
        assert_eq!(
            settings(r#"{"edit_policy":"require-claim"}"#).edit_policy(),
            EditPolicy::RequireClaim
        );
    }

    #[test]
    fn area_rules() {
        assert_eq!(
            refusal(
                r#"{"areas":[{"name":"billing","paths":["a/**"]},{"name":"billing","paths":["b/**"]}]}"#
            ),
            SettingsError::DuplicateArea {
                area: QualifiedArea::parse("default:billing").unwrap()
            }
        );
        assert_eq!(
            refusal(r#"{"areas":[{"name":"billing","paths":[]}]}"#),
            SettingsError::AreaWithoutPaths {
                area: QualifiedArea::parse("default:billing").unwrap()
            }
        );
        assert_eq!(
            refusal(r#"{"areas":[{"name":"billing"}]}"#),
            SettingsError::AreaWithoutPaths {
                area: QualifiedArea::parse("default:billing").unwrap()
            }
        );
        assert!(matches!(
            refusal(r#"{"areas":[{"name":"Billing","paths":["a/**"]}]}"#),
            SettingsError::AreaName { .. }
        ));
        assert!(matches!(
            refusal(r#"{"areas":[{"name":"billing","paths":["/abs/**"]}]}"#),
            SettingsError::AreaGlob {
                error: PathGlobError::Absolute,
                ..
            }
        ));
    }

    #[test]
    fn hot_rule_rules() {
        let areas = r#""areas":[{"name":"billing","paths":["packages/billing/**"]}]"#;
        let with_hot = |hot: &str| format!("{{{areas},\"hot\":[{hot}]}}");
        assert_eq!(
            refusal(&with_hot(r#"{"paths":[],"kind":"free"}"#)),
            SettingsError::HotRuleWithoutPaths {
                repo: RepoId::implicit(),
                number: 1
            }
        );
        assert!(matches!(
            refusal(&with_hot(r#"{"paths":["x"],"kind":"lock"}"#)),
            SettingsError::HotRule {
                number: 1,
                error: HotRuleRefused::UnknownKind { .. },
                ..
            }
        ));
        assert_eq!(
            refusal(&with_hot(
                r#"{"paths":["x"],"kind":"free"},{"paths":["y"],"kind":"owner","area":"search"}"#
            )),
            SettingsError::HotRuleArea {
                repo: RepoId::implicit(),
                number: 2,
                area: "search".to_owned()
            }
        );
        assert!(matches!(
            refusal(&with_hot(r#"{"paths":["y"],"kind":"owner"}"#)),
            SettingsError::HotRule {
                error: HotRuleRefused::AreaRequired { .. },
                ..
            }
        ));
        assert!(matches!(
            refusal(&with_hot(
                r#"{"paths":["y"],"kind":"free","area":"billing"}"#
            )),
            SettingsError::HotRule {
                error: HotRuleRefused::AreaNotAllowed { .. },
                ..
            }
        ));
        assert!(matches!(
            refusal(&with_hot(r#"{"paths":["y"],"kind":"free","note":"  "}"#)),
            SettingsError::HotRuleNote { .. }
        ));
        assert!(matches!(
            refusal(&with_hot(r#"{"paths":["y/"],"kind":"free"}"#)),
            SettingsError::HotRuleGlob { .. }
        ));
        let parsed = settings(&with_hot(
            r#"{"paths":["y"],"kind":"owner","area":"billing","note":"ask the holder"}"#,
        ));
        let rule = &parsed.repos()[0].hot[0];
        assert_eq!(rule.kind().as_str(), "owner");
        assert_eq!(
            rule.area(),
            Some(&QualifiedArea::parse("default:billing").unwrap())
        );
        assert_eq!(rule.note().map(Note::as_str), Some("ask the holder"));
    }

    #[test]
    fn repository_rules() {
        assert_eq!(
            refusal(
                r#"{"areas":[{"name":"a","paths":["a/**"]}],"repos":[{"id":"api","remote":"example.com/acme/api"}]}"#
            ),
            SettingsError::MixedLayout
        );
        assert_eq!(
            refusal(
                r#"{"hot":[{"paths":["x"],"kind":"free"}],"repos":[{"id":"api","remote":"example.com/acme/api"}]}"#
            ),
            SettingsError::MixedLayout
        );
        assert_eq!(
            refusal(
                r#"{"repos":[{"id":"api","remote":"example.com/acme/api"},{"id":"api","remote":"example.com/acme/web"}]}"#
            ),
            SettingsError::DuplicateRepo {
                repo: RepoId::parse("api").unwrap()
            }
        );
        assert_eq!(
            refusal(
                r#"{"repos":[{"id":"api","remote":"example.com/acme/api"},{"id":"web","remote":"git@example.com:Acme/API.git"}]}"#
            ),
            SettingsError::DuplicateRemote {
                remote: RemoteUrl::parse("example.com/acme/api").unwrap()
            }
        );
        assert!(matches!(
            refusal(r#"{"repos":[{"id":"API","remote":"example.com/acme/api"}]}"#),
            SettingsError::RepoId { .. }
        ));
        assert!(matches!(
            refusal(r#"{"repos":[{"id":"api","remote":"/srv/api"}]}"#),
            SettingsError::Remote { .. }
        ));
    }

    #[test]
    fn unknown_fields_are_refused_by_the_document() {
        let parsed: Result<SettingsDocument, _> = serde_json::from_str(r#"{"stale":"1h"}"#);
        assert!(parsed.is_err());
    }

    fn two_repos() -> ProjectSettings {
        settings(
            r#"{"repos":[
                {"id":"api","remote":"https://example.com/acme/api.git","areas":[
                    {"name":"billing","paths":["packages/billing/**"]},
                    {"name":"search","paths":["packages/search/**"]}]},
                {"id":"web","remote":"git@example.com:acme/web.git","areas":[
                    {"name":"billing","paths":["src/billing/**"]},
                    {"name":"checkout","paths":["src/checkout/**"]}]}
            ]}"#,
        )
    }

    #[test]
    fn areas_resolve_qualified_or_by_a_unique_name() {
        let parsed = two_repos();
        let area = |text: &str| QualifiedArea::parse(text).unwrap();
        assert_eq!(
            parsed.resolve_area("web:checkout"),
            Ok(area("web:checkout"))
        );
        assert_eq!(parsed.resolve_area("checkout"), Ok(area("web:checkout")));
        assert_eq!(parsed.resolve_area("search"), Ok(area("api:search")));
        assert_eq!(
            parsed.resolve_area("billing"),
            Err(AreaLookupError::Ambiguous {
                text: "billing".to_owned(),
                candidates: vec![area("api:billing"), area("web:billing")]
            })
        );
        assert!(matches!(
            parsed.resolve_area("api:checkout"),
            Err(AreaLookupError::NotFound { .. })
        ));
        assert!(matches!(
            parsed.resolve_area("nope"),
            Err(AreaLookupError::NotFound { .. })
        ));
        assert!(matches!(
            parsed.resolve_area("Bad Name"),
            Err(AreaLookupError::Malformed { .. })
        ));
        assert!(matches!(
            parsed.resolve_area("x:Bad"),
            Err(AreaLookupError::Malformed { .. })
        ));
    }

    #[test]
    fn repositories_are_found_by_id_and_by_any_spelling_of_their_remote() {
        let parsed = two_repos();
        assert!(parsed.lists_repositories());
        let by_remote = parsed
            .repo_by_remote(&RemoteUrl::parse("https://EXAMPLE.com/acme/web").unwrap())
            .unwrap();
        assert_eq!(by_remote.id.as_str(), "web");
        assert!(parsed.repo(&RepoId::parse("api").unwrap()).is_some());
        assert!(parsed.repo(&RepoId::parse("search").unwrap()).is_none());
    }

    #[test]
    fn a_checkout_finds_its_repository() {
        let listed = two_repos();
        let api = RepoId::parse("api").unwrap();
        let other = RepoId::parse("search").unwrap();
        assert_eq!(listed.checkout_repo(Some(&api)).unwrap().id, api);
        assert_eq!(
            listed.checkout_repo(None),
            Err(CheckoutRefused::RepoRequired)
        );
        assert_eq!(
            listed.checkout_repo(Some(&other)),
            Err(CheckoutRefused::UnknownRepo {
                repo: other.clone()
            })
        );
        let single = settings("{}");
        assert_eq!(single.checkout_repo(None).unwrap().id, RepoId::implicit());
        assert_eq!(
            single.checkout_repo(Some(&RepoId::implicit())).unwrap().id,
            RepoId::implicit()
        );
        assert!(single.checkout_repo(Some(&api)).is_err());
    }

    #[test]
    fn governed_paths_are_those_an_area_or_hot_rule_matches_in_their_own_repository() {
        let parsed = settings(
            r#"{"repos":[
                {"id":"api","remote":"example.com/acme/api","areas":[{"name":"billing","paths":["packages/billing/**"]}],
                 "hot":[{"paths":["composer.lock"],"kind":"regenerate"}]},
                {"id":"web","remote":"example.com/acme/web"}
            ]}"#,
        );
        let path = |text: &str| RepoPath::parse(text).unwrap();
        assert!(parsed.is_governed(&path("api:packages/billing/a.php")));
        assert!(parsed.is_governed(&path("api:composer.lock")));
        assert!(!parsed.is_governed(&path("api:README.md")));
        assert!(!parsed.is_governed(&path("web:packages/billing/a.php")));
        assert!(!parsed.is_governed(&path("web:composer.lock")));
        assert!(!parsed.is_governed(&path("search:composer.lock")));
    }

    #[test]
    fn settings_round_trip_through_their_document() {
        let documents = [
            "{}",
            r#"{"yield_escalates_after":"45m"}"#,
            r#"{"stale_after":"2d","heartbeat_every":"5m","edit_policy":"advise",
                "areas":[{"name":"billing","paths":["packages/billing/**","docs/billing.md"]}],
                "hot":[{"paths":["**/*.lock"],"kind":"regenerate","note":"run the generator"},
                       {"paths":["config/billing.php"],"kind":"owner","area":"billing"}]}"#,
        ];
        for json in documents {
            let parsed = settings(json);
            let again = ProjectSettings::parse(&parsed.to_document(), &HotRuleRegistry::standard())
                .unwrap();
            assert_eq!(again, parsed, "{json}");
        }
        let listed = two_repos();
        let written = listed.to_document();
        assert_eq!(written.repos[0].remote, "example.com/acme/api");
        assert_eq!(
            ProjectSettings::parse(&written, &HotRuleRegistry::standard()).unwrap(),
            listed
        );
    }

    #[test]
    fn the_wait_before_a_yield_is_escalated_is_read_in_both_layouts_and_written_back() {
        for json in [
            r#"{"yield_escalates_after":"1h","areas":[{"name":"billing","paths":["b/**"]}]}"#,
            r#"{"yield_escalates_after":"1h","repos":[{"id":"api","remote":"example.com/acme/api",
                "areas":[{"name":"billing","paths":["b/**"]}]}]}"#,
        ] {
            let parsed = settings(json);
            assert_eq!(parsed.yield_escalates_after(), Duration::hours(1), "{json}");
            let written = parsed.to_document();
            assert_eq!(written.yield_escalates_after.as_deref(), Some("1h"));
            assert_eq!(
                ProjectSettings::parse(&written, &HotRuleRegistry::standard()).unwrap(),
                parsed
            );
        }
        assert_eq!(settings("{}").to_document().yield_escalates_after, None);
    }

    #[test]
    fn errors_read_as_sentences_naming_the_setting() {
        let error = refusal(r#"{"repos":[{"id":"api","remote":"/srv/api"}]}"#);
        assert!(
            error.to_string().starts_with("repository 'api': "),
            "{error}"
        );
        let error = refusal(r#"{"stale_after":"1m"}"#);
        assert_eq!(
            error.to_string(),
            "stale_after is between 1 hour and 30 days"
        );
    }
}
