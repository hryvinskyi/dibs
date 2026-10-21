//! What a project declares about its slots: their names, groups, limits, the command lines that
//! need them and the refs they freeze.

use std::fmt;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::Duration;

use crate::hot_rule::Note;
use crate::repo::RepoId;
use crate::text::{has_control, string_value};

/// The longest a slot may be held at once.
pub const MAX_HOLD_RANGE: (Duration, Duration) = (Duration::minutes(1), Duration::hours(24));

/// How long an offer stands when the settings say nothing.
pub const DEFAULT_OFFER_FOR: Duration = Duration::minutes(5);

/// How long a waiter keeps their place without a sign of life when the settings say nothing.
pub const DEFAULT_MAX_WAIT: Duration = Duration::hours(2);

/// The longest pattern, in characters.
pub const MAX_PATTERN_CHARS: usize = 500;

/// The most command patterns one slot may have.
pub const MAX_PATTERNS: usize = 20;

/// The most memory one compiled pattern may take.
pub const PATTERN_SIZE_LIMIT: usize = 256 * 1024;

/// A slot's name: 1 to 64 characters, lowercase letters, digits and `:._-`, starting with a letter
/// or digit (`deploy:prod`, `db.staging`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SlotName(String);

/// Why a text is not a slot name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SlotNameError {
    /// The text breaks the name's character rules.
    #[error(
        "a slot name is 1 to 64 characters: lowercase letters, digits and ':', '.', '_' or '-', starting with a letter or digit"
    )]
    Malformed,
}

impl SlotName {
    /// Parses a slot name.
    ///
    /// # Errors
    /// [`SlotNameError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, SlotNameError> {
        if is_slot_text(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SlotNameError::Malformed)
        }
    }
}

string_value!(SlotName, SlotNameError);

/// The name of a group of slots that exclude each other, with the same rules as a slot's name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SlotGroup(String);

/// Why a text is not a slot group's name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SlotGroupError {
    /// The text breaks the name's character rules.
    #[error(
        "a slot group is 1 to 64 characters: lowercase letters, digits and ':', '.', '_' or '-', starting with a letter or digit"
    )]
    Malformed,
}

impl SlotGroup {
    /// Parses a group name.
    ///
    /// # Errors
    /// [`SlotGroupError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, SlotGroupError> {
        if is_slot_text(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SlotGroupError::Malformed)
        }
    }
}

string_value!(SlotGroup, SlotGroupError);

fn is_slot_text(value: &str) -> bool {
    let mut chars = value.chars();
    let first = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first
        && value.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ":._-".contains(c))
}

/// What one hold excludes: a declared group, or a slot alone, which is a group of one. The two never
/// compare equal, so a group and a slot of the same name stay apart.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GroupKey {
    /// The slots of a declared group.
    Group(SlotGroup),
    /// A slot of no group.
    Alone(SlotName),
}

/// Why a hold was taken: 1 to 200 characters with no control characters, trimmed. Every member of
/// the project reads it, like the item of a claim.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Purpose(String);

/// Why a text is not a purpose.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PurposeError {
    /// The trimmed text is empty, longer than 200 characters, or has control characters.
    #[error("a purpose is 1 to 200 characters with no control characters")]
    Malformed,
}

impl Purpose {
    /// Parses a purpose, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`PurposeError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, PurposeError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 200 || has_control(value) {
            return Err(PurposeError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Purpose, PurposeError);

/// One branch of one repository, `<repo>:refs/heads/<name>`: a branch a push goes to, or the base
/// of a pull request, which a slot's frozen refs are judged against.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoRef {
    /// The repository.
    pub repo: RepoId,
    /// The full ref name, `refs/heads/<name>`.
    pub name: String,
}

/// Why a text is not a repository ref.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepoRefError {
    /// The text is not `<repo>:refs/heads/<name>`.
    #[error("a branch is written '<repo>:refs/heads/<branch>'")]
    Malformed,
    /// The text names a tag.
    #[error("a tag is not a branch; a branch is written '<repo>:refs/heads/<branch>'")]
    Tag,
}

/// The longest full ref name, `refs/…` included.
pub(crate) const MAX_REF_NAME: usize = 255;

/// Where the branches of a repository are.
pub(crate) const BRANCHES: &str = "refs/heads/";

/// Where the tags of a repository are.
pub(crate) const TAGS: &str = "refs/tags/";

/// Whether `part`, a ref name after its `refs/<kind>/`, has nothing git refuses inside a name: no
/// control character, space or `~^:?*[\`, no leading `/`, no `..`, `//` or `@{`. Whether it may be
/// empty, or end with `/` or `.lock`, is the caller's to say.
pub(crate) fn ref_part_is_clean(part: &str) -> bool {
    let forbidden = |c: char| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c);
    !part.chars().any(forbidden)
        && !part.starts_with('/')
        && !part.contains("..")
        && !part.contains("//")
        && !part.contains("@{")
}

/// Whether `name`, a ref's name after `refs/<kind>/`, or with `pattern` the prefix before a
/// pattern's `*`, follows git's rules for a ref name: [`ref_part_is_clean`], no part between
/// slashes that is empty, starts with `.` or ends with `.lock`, and no `.` or `/` at the end. A
/// prefix may be empty, and its last part is unfinished: it may end with `/`, `.` or `.lock`, or be
/// empty, since what the `*` adds completes it.
pub(crate) fn ref_name_is_clean(name: &str, pattern: bool) -> bool {
    if !ref_part_is_clean(name) {
        return false;
    }
    let mut parts: Vec<&str> = name.split('/').collect();
    let last = parts.pop().unwrap_or_default();
    let whole =
        |part: &&str| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock");
    if !parts.iter().all(whole) || last.starts_with('.') {
        return false;
    }
    pattern || (!last.is_empty() && !last.ends_with('.') && !last.ends_with(".lock"))
}

/// How a ref's name after `refs/<kind>/` is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Written<'a> {
    /// One ref, named exactly.
    Exact(&'a str),
    /// Every ref whose name starts with this prefix, written with a `*` after it.
    Prefix(&'a str),
}

/// Why a name after `refs/<kind>/` is neither one name nor a prefix followed by one `*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarFault {
    /// Nothing is written.
    Empty,
    /// `**` is written.
    Double,
    /// A `*` is written before the end.
    NotAtEnd,
}

/// How `rest`, a ref's name after `refs/<kind>/`, is written: one name, or a prefix followed by a
/// single `*` at the end. Whether the name or the prefix is one git accepts is the caller's to say.
pub(crate) fn written(rest: &str) -> Result<Written<'_>, StarFault> {
    if rest.is_empty() {
        return Err(StarFault::Empty);
    }
    if rest.contains("**") {
        return Err(StarFault::Double);
    }
    let shape = rest
        .strip_suffix('*')
        .map_or(Written::Exact(rest), Written::Prefix);
    let (Written::Exact(literal) | Written::Prefix(literal)) = shape;
    if literal.contains('*') {
        return Err(StarFault::NotAtEnd);
    }
    Ok(shape)
}

impl RepoRef {
    /// Parses `<repo>:refs/heads/<name>`.
    ///
    /// # Errors
    /// [`RepoRefError::Tag`] for a tag ref, [`RepoRefError::Malformed`] when the repository id or
    /// the ref breaks its rules otherwise.
    pub fn parse(value: &str) -> Result<Self, RepoRefError> {
        let (repo, name) = value.split_once(':').ok_or(RepoRefError::Malformed)?;
        let repo = RepoId::parse(repo).map_err(|_| RepoRefError::Malformed)?;
        Self::named(repo, name)
    }

    /// The branch ref `name` of `repo`.
    ///
    /// # Errors
    /// [`RepoRefError::Tag`] when `name` is a `refs/tags/` ref, [`RepoRefError::Malformed`] when it
    /// is no `refs/heads/` ref git would accept.
    pub fn named(repo: RepoId, name: &str) -> Result<Self, RepoRefError> {
        if name.starts_with(TAGS) {
            return Err(RepoRefError::Tag);
        }
        Self::of(repo, name).ok_or(RepoRefError::Malformed)
    }

    /// The branch ref `name` of `repo`, when `name` is a `refs/heads/` ref git would accept.
    pub fn of(repo: RepoId, name: &str) -> Option<Self> {
        let branch = name.strip_prefix(BRANCHES)?;
        let valid = !branch.is_empty()
            && name.len() <= MAX_REF_NAME
            && ref_part_is_clean(branch)
            && !branch.ends_with('/')
            && !branch.ends_with(".lock");
        valid.then(|| Self {
            repo,
            name: name.to_owned(),
        })
    }
}

impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.repo, self.name)
    }
}

impl TryFrom<String> for RepoRef {
    type Error = RepoRefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<RepoRef> for String {
    fn from(value: RepoRef) -> String {
        value.to_string()
    }
}

/// Branches of one repository: `<repo>:refs/heads/<branch>`, that branch exactly, or
/// `<repo>:refs/heads/<prefix>*`, every branch whose name starts with the prefix
/// (`api:refs/heads/release/*` covers `release/2026.10.3`). The prefix is never empty, so no
/// pattern covers every branch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BranchPattern {
    repo: RepoId,
    name: String,
}

/// Why a text names no branch and no branches by a prefix.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BranchPatternError {
    /// The text is not `<repo>:refs/heads/…`.
    #[error(
        "a frozen ref is written '<repo>:refs/heads/<branch>' or '<repo>:refs/heads/<prefix>*'"
    )]
    Malformed,
    /// The text names a tag, which no slot freezes.
    #[error(
        "a slot freezes branches, not tags; a frozen ref is written '<repo>:refs/heads/<branch>' or '<repo>:refs/heads/<prefix>*'"
    )]
    Tag,
    /// Nothing after `refs/heads/`.
    #[error("a branch ref names a branch, or a prefix followed by '*', after 'refs/heads/'")]
    Empty,
    /// `refs/heads/*`: a pattern with nothing before its `*`.
    #[error(
        "a branch pattern names a prefix before its '*', as in 'refs/heads/release/*'; 'refs/heads/*' would cover every branch"
    )]
    EmptyPrefix,
    /// `**` in a branch pattern.
    #[error("a branch pattern has one '*', at its end; '**' is not accepted")]
    DoubleStar,
    /// A `*` before the end of a branch pattern.
    #[error("a branch pattern has its '*' at the end only, as in 'refs/heads/release/*'")]
    StarNotAtEnd,
    /// A branch name, or the prefix of a pattern, git would not accept.
    #[error(
        "the branch name is not one git accepts: no space, control character or any of ~^:?[\\, no '..', '@{{' or '//', no part that starts with '.' or ends with '.lock', and no '.' or '/' at its end"
    )]
    Name,
}

impl BranchPattern {
    /// Parses `<repo>:refs/heads/<branch>` or `<repo>:refs/heads/<prefix>*`.
    ///
    /// # Errors
    /// [`BranchPatternError`] as [`BranchPattern::named`] says, and
    /// [`BranchPatternError::Malformed`] for a text with no valid repository id before its `:`.
    pub fn parse(value: &str) -> Result<Self, BranchPatternError> {
        let (repo, name) = value.split_once(':').ok_or(BranchPatternError::Malformed)?;
        let repo = RepoId::parse(repo).map_err(|_| BranchPatternError::Malformed)?;
        Self::named(repo, name)
    }

    /// The branches `name` (`refs/heads/main`, `refs/heads/release/*`) names in `repo`. A branch
    /// named exactly follows the rules of [`RepoRef::of`]; the prefix of a pattern is not empty and
    /// follows git's rules for the start of a ref name.
    ///
    /// # Errors
    /// [`BranchPatternError::Tag`] for a `refs/tags/` ref, [`BranchPatternError::Malformed`] for
    /// another ref that is no `refs/heads/` ref, and the other variants for a branch ref whose
    /// `*` or name breaks the rules above.
    pub fn named(repo: RepoId, name: &str) -> Result<Self, BranchPatternError> {
        if name.starts_with(TAGS) {
            return Err(BranchPatternError::Tag);
        }
        let rest = name
            .strip_prefix(BRANCHES)
            .ok_or(BranchPatternError::Malformed)?;
        let clean = match written(rest) {
            Err(StarFault::Empty) => return Err(BranchPatternError::Empty),
            Err(StarFault::Double) => return Err(BranchPatternError::DoubleStar),
            Err(StarFault::NotAtEnd) => return Err(BranchPatternError::StarNotAtEnd),
            Ok(Written::Prefix("")) => return Err(BranchPatternError::EmptyPrefix),
            Ok(Written::Prefix(prefix)) => {
                name.len() <= MAX_REF_NAME && ref_name_is_clean(prefix, true)
            }
            Ok(Written::Exact(_)) => RepoRef::of(repo.clone(), name).is_some(),
        };
        if !clean {
            return Err(BranchPatternError::Name);
        }
        Ok(Self {
            repo,
            name: name.to_owned(),
        })
    }

    /// The repository.
    pub fn repo(&self) -> &RepoId {
        &self.repo
    }

    /// The branch or the pattern as written: `refs/heads/main`, `refs/heads/release/*`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What precedes the `*` of a pattern, the full ref (`refs/heads/release/`); `None` for a branch
    /// named exactly.
    pub fn prefix(&self) -> Option<&str> {
        self.name.strip_suffix('*')
    }

    /// Whether the full ref `name` of its repository is one it covers: the same branch, or for a
    /// pattern, a branch that starts with what precedes its `*`.
    pub fn covers(&self, name: &str) -> bool {
        match self.prefix() {
            Some(prefix) => name.starts_with(prefix),
            None => self.name == name,
        }
    }

    /// Whether `branch`, of any repository, is one it covers.
    pub fn covers_branch(&self, branch: &RepoRef) -> bool {
        self.repo == branch.repo && self.covers(&branch.name)
    }
}

impl fmt::Display for BranchPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.repo, self.name)
    }
}

impl TryFrom<String> for BranchPattern {
    type Error = BranchPatternError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<BranchPattern> for String {
    fn from(value: BranchPattern) -> String {
        value.to_string()
    }
}

/// A command line pattern: a simple command matching `matches` needs the slot, unless it also
/// matches `unless`. Both match in time linear in the input, so no pattern can be made slow.
#[derive(Debug, Clone)]
pub struct CommandPattern {
    matches: Regex,
    unless: Option<Regex>,
}

/// Why a pattern is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PatternError {
    /// The pattern is empty or longer than [`MAX_PATTERN_CHARS`].
    #[error("a command pattern is 1 to 500 characters")]
    Length,
    /// The pattern is not a regular expression, or compiles larger than [`PATTERN_SIZE_LIMIT`].
    #[error("'{pattern}' is not a regular expression dibs accepts: {reason}")]
    Invalid {
        /// The pattern as written.
        pattern: String,
        /// What is wrong with it, in the regular expression library's words.
        reason: String,
    },
}

impl CommandPattern {
    /// Compiles a pattern and its exclusion.
    ///
    /// # Errors
    /// [`PatternError`] when either is empty, too long, invalid or too large.
    pub fn new(matches: &str, unless: Option<&str>) -> Result<Self, PatternError> {
        Ok(Self {
            matches: compile(matches)?,
            unless: unless.map(compile).transpose()?,
        })
    }

    /// The pattern as written.
    pub fn matches_source(&self) -> &str {
        self.matches.as_str()
    }

    /// The exclusion as written.
    pub fn unless_source(&self) -> Option<&str> {
        self.unless.as_ref().map(Regex::as_str)
    }

    /// Whether one simple command needs the slot.
    pub fn is_match(&self, command: &str) -> bool {
        self.matches.is_match(command)
            && !self
                .unless
                .as_ref()
                .is_some_and(|unless| unless.is_match(command))
    }
}

impl PartialEq for CommandPattern {
    fn eq(&self, other: &Self) -> bool {
        self.matches_source() == other.matches_source()
            && self.unless_source() == other.unless_source()
    }
}

impl Eq for CommandPattern {}

fn compile(pattern: &str) -> Result<Regex, PatternError> {
    if pattern.is_empty() || pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err(PatternError::Length);
    }
    RegexBuilder::new(pattern)
        .size_limit(PATTERN_SIZE_LIMIT)
        .dfa_size_limit(PATTERN_SIZE_LIMIT)
        .build()
        .map_err(|error| PatternError::Invalid {
            pattern: pattern.to_owned(),
            reason: error.to_string(),
        })
}

/// One declared slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSpec {
    /// Its name.
    pub name: SlotName,
    /// The longest one hold lasts before it must be renewed.
    pub max_hold: Duration,
    /// How long an offer to the first waiter stands.
    pub offer_for: Duration,
    /// How long a waiter keeps their place without a sign of life.
    pub max_wait: Duration,
    /// The simple commands that need the slot, first match wins.
    pub commands: Vec<CommandPattern>,
    /// Whether its commands run only inside `dibs slot run`.
    pub run_only: bool,
    /// The group whose slots it excludes, when it has one.
    pub group: Option<SlotGroup>,
    /// The branches nobody but the holder may push to while it is held.
    pub freezes: Vec<BranchPattern>,
    /// A note shown with it.
    pub note: Option<Note>,
}

impl SlotSpec {
    /// What a hold of this slot excludes.
    pub fn group_key(&self) -> GroupKey {
        self.group.as_ref().map_or_else(
            || GroupKey::Alone(self.name.clone()),
            |group| GroupKey::Group(group.clone()),
        )
    }

    /// Whether a hold of this slot freezes `branch`: one of its frozen refs covers it.
    pub fn freezes_branch(&self, branch: &RepoRef) -> bool {
        self.freeze_of(branch).is_some()
    }

    /// The first of its frozen refs that covers `branch`.
    pub fn freeze_of(&self, branch: &RepoRef) -> Option<&BranchPattern> {
        self.freezes
            .iter()
            .find(|frozen| frozen.covers_branch(branch))
    }

    /// Whether one simple command needs this slot.
    pub fn matches(&self, command: &str) -> bool {
        self.commands
            .iter()
            .any(|pattern| pattern.is_match(command))
    }
}

/// Every slot a project declares, in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotCatalog {
    slots: Vec<SlotSpec>,
}

impl SlotCatalog {
    /// A catalog of these slots; their names are unique.
    pub(crate) fn new(slots: Vec<SlotSpec>) -> Self {
        Self { slots }
    }

    /// The slots, in declaration order.
    pub fn slots(&self) -> &[SlotSpec] {
        &self.slots
    }

    /// The slot of this name.
    pub fn get(&self, name: &SlotName) -> Option<&SlotSpec> {
        self.slots.iter().find(|spec| spec.name == *name)
    }

    /// What a hold of the named slot excludes, when the slot is declared.
    pub fn group_of(&self, name: &SlotName) -> Option<GroupKey> {
        self.get(name).map(SlotSpec::group_key)
    }

    /// Whether the named slot is declared and its hold excludes `key`.
    pub fn in_group(&self, name: &SlotName, key: &GroupKey) -> bool {
        self.group_of(name).as_ref() == Some(key)
    }

    /// Every group, each once, in the order its first slot is declared.
    pub fn groups(&self) -> Vec<GroupKey> {
        let mut keys: Vec<GroupKey> = Vec::new();
        for spec in &self.slots {
            let key = spec.group_key();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_names_follow_their_rules() {
        for good in ["deploy:prod", "db.staging", "a", "release_2", "0-x"] {
            assert!(SlotName::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "Deploy", ":x", "-x", "a b", "a/b", &"a".repeat(65)] {
            assert!(SlotName::parse(bad).is_err(), "{bad}");
        }
        assert!(SlotGroup::parse("staging").is_ok());
        assert!(SlotGroup::parse("Staging").is_err());
    }

    #[test]
    fn a_group_and_a_lone_slot_of_one_name_differ() {
        let group = GroupKey::Group(SlotGroup::parse("x").unwrap());
        let alone = GroupKey::Alone(SlotName::parse("x").unwrap());
        assert_ne!(group, alone);
    }

    #[test]
    fn purposes_are_trimmed_and_bounded() {
        assert_eq!(Purpose::parse("  ship it ").unwrap().as_str(), "ship it");
        assert!(Purpose::parse(" ").is_err());
        assert!(Purpose::parse(&"x".repeat(201)).is_err());
        assert!(Purpose::parse("a\nb").is_err());
    }

    #[test]
    fn repo_refs_are_branch_refs_of_a_repository() {
        let main = RepoRef::parse("api:refs/heads/main").unwrap();
        assert_eq!(main.repo.as_str(), "api");
        assert_eq!(main.name, "refs/heads/main");
        assert_eq!(main.to_string(), "api:refs/heads/main");
        assert!(RepoRef::parse("api:refs/heads/release/2.x").is_ok());
        for bad in [
            "refs/heads/main",
            "api:main",
            "api:refs/tags/v1",
            "API:refs/heads/main",
            "api:refs/heads/",
            "api:refs/heads/a..b",
            "api:refs/heads/a b",
            "api:refs/heads/x.lock",
        ] {
            assert!(RepoRef::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            RepoRef::parse("api:refs/tags/v1"),
            Err(RepoRefError::Tag),
            "a tag is refused as one"
        );
        assert_eq!(RepoRef::parse("api:main"), Err(RepoRefError::Malformed));
    }

    #[test]
    fn a_branch_pattern_is_a_branch_or_a_prefix_of_branches_of_one_repository() {
        let releases = BranchPattern::parse("api:refs/heads/release/*").unwrap();
        assert_eq!(releases.repo().as_str(), "api");
        assert_eq!(releases.name(), "refs/heads/release/*");
        assert_eq!(releases.prefix(), Some("refs/heads/release/"));
        assert_eq!(releases.to_string(), "api:refs/heads/release/*");
        let branch = |text: &str| RepoRef::parse(text).unwrap();
        assert!(releases.covers_branch(&branch("api:refs/heads/release/2026.10.3")));
        assert!(!releases.covers_branch(&branch("api:refs/heads/release")));
        assert!(!releases.covers_branch(&branch("web:refs/heads/release/2026.10.3")));
        let main = BranchPattern::parse("api:refs/heads/main").unwrap();
        assert_eq!(main.prefix(), None);
        assert!(main.covers_branch(&branch("api:refs/heads/main")));
        assert!(!main.covers_branch(&branch("api:refs/heads/main2")));

        let json = serde_json::to_string(&releases).unwrap();
        assert_eq!(json, r#""api:refs/heads/release/*""#);
        assert_eq!(
            serde_json::from_str::<BranchPattern>(&json).unwrap(),
            releases
        );
        assert!(serde_json::from_str::<BranchPattern>(r#""api:refs/heads/*""#).is_err());

        for (text, error) in [
            ("refs/heads/release/*", BranchPatternError::Malformed),
            ("API:refs/heads/release/*", BranchPatternError::Malformed),
            ("api:release/*", BranchPatternError::Malformed),
            ("api:refs/tags/v*", BranchPatternError::Tag),
            ("api:refs/heads/", BranchPatternError::Empty),
            ("api:refs/heads/*", BranchPatternError::EmptyPrefix),
            ("api:refs/heads/release**", BranchPatternError::DoubleStar),
            ("api:refs/heads/*release", BranchPatternError::StarNotAtEnd),
            ("api:refs/heads/a..b/*", BranchPatternError::Name),
            ("api:refs/heads/x.lock", BranchPatternError::Name),
        ] {
            assert_eq!(BranchPattern::parse(text), Err(error), "{text}");
        }
    }

    #[test]
    fn a_pattern_matches_unless_its_exclusion_does() {
        let push =
            CommandPattern::new(r"^git (-C \S+ )?push", Some(r"^git -C \.\./other")).unwrap();
        assert!(push.is_match("git push origin main"));
        assert!(push.is_match("git -C api push"));
        assert!(!push.is_match("git -C ../other push"));
        assert!(!push.is_match("echo git push"));
        assert_eq!(
            push,
            CommandPattern::new(r"^git (-C \S+ )?push", Some(r"^git -C \.\./other")).unwrap()
        );
        assert_ne!(
            push,
            CommandPattern::new(r"^git (-C \S+ )?push", None).unwrap()
        );
    }

    #[test]
    fn an_invalid_or_oversized_pattern_is_refused() {
        assert_eq!(
            CommandPattern::new("", None).unwrap_err(),
            PatternError::Length
        );
        assert_eq!(
            CommandPattern::new(&"a".repeat(501), None).unwrap_err(),
            PatternError::Length
        );
        assert!(matches!(
            CommandPattern::new("(unclosed", None),
            Err(PatternError::Invalid { .. })
        ));
        assert!(matches!(
            CommandPattern::new("x", Some("[")),
            Err(PatternError::Invalid { .. })
        ));
        assert!(matches!(
            CommandPattern::new(r"\w{1000}\w{1000}\w{1000}", None),
            Err(PatternError::Invalid { .. })
        ));
        assert!(
            CommandPattern::new("(?=look)ahead", None).is_err(),
            "no look-around"
        );
    }
}
