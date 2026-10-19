//! Hot-file rules: per-glob policies for files many people touch, and the registry that builds a
//! rule from its kind.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::area::QualifiedArea;
use crate::claim::Holder;
use crate::edit::{EditWorld, Verdict};
use crate::paths::{PathGlob, RelPath, RepoPath};
use crate::text::string_value;

/// The kind of a hot-file rule, as registered in a [`HotRuleRegistry`] (`regenerate`, `owner`,
/// `warn-shared`, `free`). Only a registry hands one out, so every kind in settings is a known one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(into = "String")]
pub struct HotRuleKind(String);

impl HotRuleKind {
    /// The kind's name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HotRuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<HotRuleKind> for String {
    fn from(value: HotRuleKind) -> String {
        value.0
    }
}

/// A note a hot-file rule shows with its verdict: 1 to 500 characters after trimming, with no
/// control characters other than line breaks.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Note(String);

/// Why a text is not a note.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NoteError {
    /// The trimmed text is empty, longer than 500 characters, or has control characters.
    #[error("a note is 1 to 500 characters with no control characters other than line breaks")]
    Malformed,
}

impl Note {
    /// Parses a note, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`NoteError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, NoteError> {
        let value = value.trim();
        let controls = value.chars().any(|c| c.is_control() && c != '\n');
        if value.is_empty() || value.chars().count() > 500 || controls {
            return Err(NoteError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Note, NoteError);

/// What a hot-file rule decided for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotDecision {
    /// This is the path's verdict.
    Decide(Verdict),
    /// This is the path's verdict, and the path is recorded as shared under the editing member's
    /// claim so later edits by others can name it.
    Share(Verdict),
    /// The path is decided as a path of this area.
    AsArea(QualifiedArea),
}

/// What a hot-file rule sees when it decides a path.
pub struct HotContext<'a> {
    path: &'a RepoPath,
    kind: &'a HotRuleKind,
    note: Option<&'a Note>,
    world: &'a EditWorld<'a>,
}

impl<'a> HotContext<'a> {
    pub(crate) fn new(spec: &'a HotRuleSpec, path: &'a RepoPath, world: &'a EditWorld<'a>) -> Self {
        Self {
            path,
            kind: &spec.kind,
            note: spec.note.as_ref(),
            world,
        }
    }

    /// The path being decided.
    pub fn path(&self) -> &RepoPath {
        self.path
    }

    /// The kind of the rule that matched.
    pub fn kind(&self) -> &HotRuleKind {
        self.kind
    }

    /// The note of the rule that matched.
    pub fn note(&self) -> Option<&Note> {
        self.note
    }

    /// Other members' live claims that recorded this same path as shared.
    pub fn others_sharing(&self) -> Vec<Holder> {
        self.world.others_sharing(self.path)
    }
}

/// The behaviour of one kind of hot-file rule.
pub trait HotRule: Send + Sync + fmt::Debug {
    /// Decides one path the rule's globs matched.
    fn decide(&self, ctx: &HotContext<'_>) -> HotDecision;
}

/// The settings a rule is built from.
#[derive(Debug, Clone, Copy)]
pub struct HotRuleConfig<'a> {
    /// The kind being built.
    pub kind: &'a HotRuleKind,
    /// The area the rule names, already checked to exist in the rule's repository.
    pub area: Option<&'a QualifiedArea>,
    /// The rule's note.
    pub note: Option<&'a Note>,
}

/// Why a rule cannot be built.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HotRuleRefused {
    /// No rule of this kind is registered.
    #[error("'{kind}' is not a hot-file rule kind; the known kinds are {known}")]
    UnknownKind {
        /// The kind asked for.
        kind: String,
        /// The registered kinds, comma-separated.
        known: String,
    },
    /// The kind needs an area and the rule names none.
    #[error("a '{kind}' rule names the area whose holder may edit its files")]
    AreaRequired {
        /// The kind.
        kind: HotRuleKind,
    },
    /// The kind takes no area and the rule names one.
    #[error("a '{kind}' rule takes no area")]
    AreaNotAllowed {
        /// The kind.
        kind: HotRuleKind,
    },
}

/// Builds the rule of one kind from its settings.
pub type HotRuleConstructor = fn(&HotRuleConfig<'_>) -> Result<Arc<dyn HotRule>, HotRuleRefused>;

/// The hot-file rule kinds a deployment knows, each with the constructor that builds it.
#[derive(Debug, Clone, Default)]
pub struct HotRuleRegistry {
    constructors: BTreeMap<String, HotRuleConstructor>,
}

impl HotRuleRegistry {
    /// A registry with no kind.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The registry of the kinds this crate ships: `regenerate`, `owner`, `warn-shared` and `free`.
    pub fn standard() -> Self {
        Self::empty()
            .with("regenerate", Regenerate::build)
            .with("owner", Owner::build)
            .with("warn-shared", WarnShared::build)
            .with("free", Free::build)
    }

    /// The registry with `constructor` registered as kind `name`, replacing an earlier one.
    #[must_use]
    pub fn with(mut self, name: &str, constructor: HotRuleConstructor) -> Self {
        self.constructors.insert(name.to_owned(), constructor);
        self
    }

    /// The registered kind of this name.
    ///
    /// # Errors
    /// [`HotRuleRefused::UnknownKind`] when no kind of that name is registered.
    pub fn kind(&self, name: &str) -> Result<HotRuleKind, HotRuleRefused> {
        if self.constructors.contains_key(name) {
            Ok(HotRuleKind(name.to_owned()))
        } else {
            Err(HotRuleRefused::UnknownKind {
                kind: name.to_owned(),
                known: self
                    .constructors
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            })
        }
    }

    /// Builds the rule `config` describes.
    ///
    /// # Errors
    /// [`HotRuleRefused`] when the kind is unknown or refuses the configuration.
    pub fn build(&self, config: &HotRuleConfig<'_>) -> Result<Arc<dyn HotRule>, HotRuleRefused> {
        let constructor = self.constructors.get(config.kind.as_str()).ok_or_else(|| {
            HotRuleRefused::UnknownKind {
                kind: config.kind.to_string(),
                known: self
                    .constructors
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            }
        })?;
        constructor(config)
    }
}

/// A hot-file rule as declared in a repository's settings, with the rule its kind built.
#[derive(Clone)]
pub struct HotRuleSpec {
    paths: Vec<PathGlob>,
    kind: HotRuleKind,
    area: Option<QualifiedArea>,
    note: Option<Note>,
    rule: Arc<dyn HotRule>,
}

impl HotRuleSpec {
    pub(crate) fn new(
        paths: Vec<PathGlob>,
        kind: HotRuleKind,
        area: Option<QualifiedArea>,
        note: Option<Note>,
        rule: Arc<dyn HotRule>,
    ) -> Self {
        Self {
            paths,
            kind,
            area,
            note,
            rule,
        }
    }

    /// The globs the rule applies to.
    pub fn paths(&self) -> &[PathGlob] {
        &self.paths
    }

    /// The rule's kind.
    pub fn kind(&self) -> &HotRuleKind {
        &self.kind
    }

    /// The area the rule names, for kinds that take one.
    pub fn area(&self) -> Option<&QualifiedArea> {
        self.area.as_ref()
    }

    /// The rule's note.
    pub fn note(&self) -> Option<&Note> {
        self.note.as_ref()
    }

    /// Whether one of the rule's globs matches the path.
    pub fn matches(&self, path: &RelPath) -> bool {
        self.paths.iter().any(|glob| glob.is_match(path))
    }

    pub(crate) fn decide(&self, ctx: &HotContext<'_>) -> HotDecision {
        self.rule.decide(ctx)
    }
}

impl fmt::Debug for HotRuleSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HotRuleSpec")
            .field("paths", &self.paths)
            .field("kind", &self.kind)
            .field("area", &self.area)
            .field("note", &self.note)
            .finish_non_exhaustive()
    }
}

impl PartialEq for HotRuleSpec {
    fn eq(&self, other: &Self) -> bool {
        self.paths == other.paths
            && self.kind == other.kind
            && self.area == other.area
            && self.note == other.note
    }
}

impl Eq for HotRuleSpec {}

fn refuse_area(config: &HotRuleConfig<'_>) -> Result<(), HotRuleRefused> {
    match config.area {
        Some(_) => Err(HotRuleRefused::AreaNotAllowed {
            kind: config.kind.clone(),
        }),
        None => Ok(()),
    }
}

/// `regenerate`: a generated file anyone may change and nobody merges by hand. Always allowed, with
/// the rule's note.
#[derive(Debug, Clone, Copy)]
pub struct Regenerate;

impl Regenerate {
    fn build(config: &HotRuleConfig<'_>) -> Result<Arc<dyn HotRule>, HotRuleRefused> {
        refuse_area(config)?;
        Ok(Arc::new(Self))
    }
}

impl HotRule for Regenerate {
    fn decide(&self, ctx: &HotContext<'_>) -> HotDecision {
        HotDecision::Decide(Verdict::AllowedWithNote {
            rule: ctx.kind().clone(),
            note: ctx.note().cloned(),
            touched_by: Vec::new(),
        })
    }
}

/// `owner(<area>)`: only the holder of the named area edits these files; the path is decided as a
/// path of that area.
#[derive(Debug, Clone)]
pub struct Owner {
    area: QualifiedArea,
}

impl Owner {
    fn build(config: &HotRuleConfig<'_>) -> Result<Arc<dyn HotRule>, HotRuleRefused> {
        let area = config.area.ok_or_else(|| HotRuleRefused::AreaRequired {
            kind: config.kind.clone(),
        })?;
        Ok(Arc::new(Self { area: area.clone() }))
    }
}

impl HotRule for Owner {
    fn decide(&self, _ctx: &HotContext<'_>) -> HotDecision {
        HotDecision::AsArea(self.area.clone())
    }
}

/// `warn-shared`: anyone may edit; the verdict names the other members' live claims that edited the
/// same file, and the edit is recorded so later editors are told.
#[derive(Debug, Clone, Copy)]
pub struct WarnShared;

impl WarnShared {
    fn build(config: &HotRuleConfig<'_>) -> Result<Arc<dyn HotRule>, HotRuleRefused> {
        refuse_area(config)?;
        Ok(Arc::new(Self))
    }
}

impl HotRule for WarnShared {
    fn decide(&self, ctx: &HotContext<'_>) -> HotDecision {
        let others = ctx.others_sharing();
        if others.is_empty() {
            return HotDecision::Share(Verdict::Allowed);
        }
        HotDecision::Share(Verdict::AllowedWithNote {
            rule: ctx.kind().clone(),
            note: ctx.note().cloned(),
            touched_by: others,
        })
    }
}

/// `free`: anyone may edit, with nothing said.
#[derive(Debug, Clone, Copy)]
pub struct Free;

impl Free {
    fn build(config: &HotRuleConfig<'_>) -> Result<Arc<dyn HotRule>, HotRuleRefused> {
        refuse_area(config)?;
        Ok(Arc::new(Self))
    }
}

impl HotRule for Free {
    fn decide(&self, _ctx: &HotContext<'_>) -> HotDecision {
        HotDecision::Decide(Verdict::Allowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_table() {
        assert_eq!(
            Note::parse("  run make gen  ").unwrap().as_str(),
            "run make gen"
        );
        assert!(Note::parse("line one\nline two").is_ok());
        assert_eq!(Note::parse(""), Err(NoteError::Malformed));
        assert_eq!(Note::parse("   "), Err(NoteError::Malformed));
        assert_eq!(Note::parse("tab\there"), Err(NoteError::Malformed));
        assert!(Note::parse(&"n".repeat(500)).is_ok());
        assert_eq!(Note::parse(&"n".repeat(501)), Err(NoteError::Malformed));
    }

    #[test]
    fn the_standard_registry_knows_four_kinds_and_refuses_others() {
        let registry = HotRuleRegistry::standard();
        for name in ["regenerate", "owner", "warn-shared", "free"] {
            assert_eq!(registry.kind(name).unwrap().as_str(), name);
        }
        let refused = registry.kind("lock").unwrap_err();
        assert_eq!(
            refused,
            HotRuleRefused::UnknownKind {
                kind: "lock".to_owned(),
                known: "free, owner, regenerate, warn-shared".to_owned(),
            }
        );
        assert!(HotRuleRegistry::empty().kind("free").is_err());
    }

    #[test]
    fn constructors_check_the_area_their_kind_needs() {
        let registry = HotRuleRegistry::standard();
        let area = QualifiedArea::parse("api:billing").unwrap();
        let owner = registry.kind("owner").unwrap();
        let free = registry.kind("free").unwrap();
        let build = |kind: &HotRuleKind, area: Option<&QualifiedArea>| {
            registry.build(&HotRuleConfig {
                kind,
                area,
                note: None,
            })
        };
        assert!(build(&owner, Some(&area)).is_ok());
        assert_eq!(
            build(&owner, None).unwrap_err(),
            HotRuleRefused::AreaRequired {
                kind: owner.clone()
            }
        );
        assert!(build(&free, None).is_ok());
        assert_eq!(
            build(&free, Some(&area)).unwrap_err(),
            HotRuleRefused::AreaNotAllowed { kind: free.clone() }
        );
        for name in ["regenerate", "warn-shared"] {
            let kind = registry.kind(name).unwrap();
            assert!(build(&kind, Some(&area)).is_err(), "{name}");
        }
    }

    #[derive(Debug)]
    struct AlwaysNeedsClaim;

    impl HotRule for AlwaysNeedsClaim {
        fn decide(&self, _ctx: &HotContext<'_>) -> HotDecision {
            HotDecision::Decide(Verdict::NeedsClaim)
        }
    }

    #[test]
    fn a_registry_accepts_a_new_kind_without_touching_the_others() {
        fn build(_config: &HotRuleConfig<'_>) -> Result<Arc<dyn HotRule>, HotRuleRefused> {
            Ok(Arc::new(AlwaysNeedsClaim))
        }
        let registry = HotRuleRegistry::standard().with("strict", build);
        let kind = registry.kind("strict").unwrap();
        assert!(
            registry
                .build(&HotRuleConfig {
                    kind: &kind,
                    area: None,
                    note: None
                })
                .is_ok()
        );
    }
}
