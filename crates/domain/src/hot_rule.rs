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

/// The command that regenerates the files of a `regenerate` rule, shown with its verdict and never
/// run by dibs: one line of 1 to 200 characters after trimming, with no control characters, no
/// backtick and no character that breaks a line or that a reader is not shown (a line or paragraph
/// separator, or any default-ignorable code point: a bidirectional control, a zero-width
/// character, a variation selector).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Recipe(String);

/// Why a text is not a recipe.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RecipeError {
    /// The trimmed text is empty, longer than 200 characters, or has a character the type refuses:
    /// a control character (a line break included), a backtick, which would end the code span dibs
    /// shows it in, or one that breaks the line or hides or reorders what is shown.
    #[error(
        "a hot rule's recipe is one line of 1 to 200 characters with no control characters, backticks, line separators or invisible characters"
    )]
    Malformed,
}

impl Recipe {
    /// The longest recipe, in characters.
    pub const MAX_CHARS: usize = 200;

    /// Parses a recipe, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`RecipeError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, RecipeError> {
        let value = value.trim();
        let refused = value
            .chars()
            .any(|c| c.is_control() || c == '`' || hides_or_breaks(c));
        if value.is_empty() || value.chars().count() > Self::MAX_CHARS || refused {
            return Err(RecipeError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Recipe, RecipeError);

/// Whether `c` breaks a line without being a control character, or is not shown to a reader at
/// all: the line and paragraph separators, the interlinear annotation marks, and every code point
/// Unicode makes default-ignorable (the bidirectional marks, embeddings, overrides and isolates,
/// the zero-width and invisible formatting characters, the fillers, the variation selectors and
/// the tag characters).
const fn hides_or_breaks(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{fff0}'..='\u{fffb}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e0fff}'
    )
}

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
    recipe: Option<&'a Recipe>,
    world: &'a EditWorld<'a>,
}

impl<'a> HotContext<'a> {
    pub(crate) fn new(spec: &'a HotRuleSpec, path: &'a RepoPath, world: &'a EditWorld<'a>) -> Self {
        Self {
            path,
            kind: &spec.kind,
            note: spec.note.as_ref(),
            recipe: spec.recipe.as_ref(),
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

    /// The recipe of the rule that matched: the command that regenerates the path.
    pub fn recipe(&self) -> Option<&Recipe> {
        self.recipe
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

    /// Whether a file under the rule is merged by hand when two branches change it. Only such a
    /// file makes two branches that change it collide, or leaves a branch behind when a merge into
    /// its base changes it. A kind is, unless it says otherwise.
    fn merged_by_hand(&self) -> bool {
        true
    }

    /// The area every file under the rule is a path of, for a kind that decides each file it
    /// matches as a path of that area ([`HotDecision::AsArea`]), so that what is said about the
    /// area's paths — whether a branch touches the area — says it of these files too. A kind
    /// names none unless it says otherwise.
    fn as_area(&self) -> Option<&QualifiedArea> {
        None
    }

    /// Whether a rule of the kind may name a recipe: the command that regenerates its files. A
    /// rule whose kind takes none is refused when it names one. A kind takes none unless it says
    /// otherwise.
    fn takes_recipe(&self) -> bool {
        false
    }
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
    /// The kind takes no recipe and the rule names one.
    #[error("a '{kind}' rule takes no recipe")]
    RecipeNotAllowed {
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

/// A hot-file rule as declared in a repository's settings, with the rule its kind built. The note
/// and the recipe are kept here only, and reach the rule through its [`HotContext`].
#[derive(Clone)]
pub struct HotRuleSpec {
    paths: Vec<PathGlob>,
    kind: HotRuleKind,
    area: Option<QualifiedArea>,
    note: Option<Note>,
    recipe: Option<Recipe>,
    rule: Arc<dyn HotRule>,
}

impl HotRuleSpec {
    pub(crate) fn new(
        paths: Vec<PathGlob>,
        kind: HotRuleKind,
        area: Option<QualifiedArea>,
        note: Option<Note>,
        recipe: Option<Recipe>,
        rule: Arc<dyn HotRule>,
    ) -> Self {
        Self {
            paths,
            kind,
            area,
            note,
            recipe,
            rule,
        }
    }

    /// The same rule without its recipe.
    #[must_use]
    pub(crate) fn without_recipe(self) -> Self {
        Self {
            recipe: None,
            ..self
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

    /// The rule's recipe: the command that regenerates its files, for a kind that takes one.
    pub fn recipe(&self) -> Option<&Recipe> {
        self.recipe.as_ref()
    }

    /// Whether one of the rule's globs matches the path.
    pub fn matches(&self, path: &RelPath) -> bool {
        self.paths.iter().any(|glob| glob.is_match(path))
    }

    pub(crate) fn decide(&self, ctx: &HotContext<'_>) -> HotDecision {
        self.rule.decide(ctx)
    }

    /// Whether a file under the rule is merged by hand, as its kind answers: only such a file
    /// makes branches collide or leaves a branch behind a merge into its base.
    pub fn merged_by_hand(&self) -> bool {
        self.rule.merged_by_hand()
    }

    /// The area every file under the rule is a path of, as its kind answers ([`HotRule::as_area`]).
    pub fn as_area(&self) -> Option<&QualifiedArea> {
        self.rule.as_area()
    }
}

impl fmt::Debug for HotRuleSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HotRuleSpec")
            .field("paths", &self.paths)
            .field("kind", &self.kind)
            .field("area", &self.area)
            .field("note", &self.note)
            .field("recipe", &self.recipe)
            .finish_non_exhaustive()
    }
}

impl PartialEq for HotRuleSpec {
    fn eq(&self, other: &Self) -> bool {
        self.paths == other.paths
            && self.kind == other.kind
            && self.area == other.area
            && self.note == other.note
            && self.recipe == other.recipe
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
/// the rule's note and the recipe that regenerates it.
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
            recipe: ctx.recipe().cloned(),
            touched_by: Vec::new(),
        })
    }

    /// No: the file is made again on the new base.
    fn merged_by_hand(&self) -> bool {
        false
    }

    /// Yes: the command that makes the file again.
    fn takes_recipe(&self) -> bool {
        true
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

    fn as_area(&self) -> Option<&QualifiedArea> {
        Some(&self.area)
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
            recipe: None,
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
    fn recipe_table() {
        assert_eq!(
            Recipe::parse("  cargo xtask integrations  ")
                .unwrap()
                .as_str(),
            "cargo xtask integrations"
        );
        assert_eq!(
            Recipe::parse("make gen").unwrap().to_string(),
            "make gen",
            "it shows as written"
        );
        assert!(Recipe::parse(&"r".repeat(200)).is_ok());
        for refused in [
            String::new(),
            "   ".to_owned(),
            "r".repeat(201),
            "make gen\nmake check".to_owned(),
            "make gen\rmake check".to_owned(),
            "tab\there".to_owned(),
            "make `gen`".to_owned(),
            "make gen\u{2028}rm -rf build".to_owned(),
            "make gen\u{2029}x".to_owned(),
            "make \u{202e}neg".to_owned(),
            "make \u{2066}gen\u{2069}".to_owned(),
            "make\u{200b}gen".to_owned(),
            "\u{feff}make gen".to_owned(),
            "make\u{180e}gen".to_owned(),
            "make \u{206a}gen".to_owned(),
            "make \u{fff9}gen\u{fffb}".to_owned(),
            "make gen\u{e0041}".to_owned(),
            "make\u{034f} gen".to_owned(),
            "make gen\u{fe0f}".to_owned(),
            "make\u{180b}gen".to_owned(),
            "make\u{3164}gen".to_owned(),
            "make\u{115f}gen".to_owned(),
            "make\u{17b4}gen".to_owned(),
            "make\u{ffa0}gen".to_owned(),
            "make\u{fff0}gen".to_owned(),
            "make\u{1bca0}gen".to_owned(),
            "make\u{1d173}gen".to_owned(),
            "make gen\u{e0100}".to_owned(),
        ] {
            assert_eq!(
                Recipe::parse(&refused),
                Err(RecipeError::Malformed),
                "{refused:?}"
            );
        }
        let error = RecipeError::Malformed.to_string();
        assert!(error.contains("recipe"), "{error}");
        assert!(error.contains("200"), "{error}");
    }

    #[test]
    fn a_recipe_reads_and_writes_as_a_plain_string() {
        let recipe: Recipe = serde_json::from_str(r#""npm run gen""#).unwrap();
        assert_eq!(recipe.as_str(), "npm run gen");
        assert_eq!(serde_json::to_string(&recipe).unwrap(), r#""npm run gen""#);
        assert!(serde_json::from_str::<Recipe>(r#""one\ntwo""#).is_err());
    }

    #[test]
    fn only_a_regenerate_rule_takes_a_recipe() {
        let registry = HotRuleRegistry::standard();
        let area = QualifiedArea::parse("api:billing").unwrap();
        for (name, area, takes) in [
            ("regenerate", None, true),
            ("owner", Some(&area), false),
            ("warn-shared", None, false),
            ("free", None, false),
        ] {
            let kind = registry.kind(name).unwrap();
            let rule = registry
                .build(&HotRuleConfig {
                    kind: &kind,
                    area,
                    note: None,
                })
                .unwrap();
            assert_eq!(rule.takes_recipe(), takes, "{name}");
        }
        assert!(
            !AlwaysNeedsClaim.takes_recipe(),
            "a kind that does not say otherwise takes none"
        );
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

    #[test]
    fn only_a_regenerated_file_is_not_merged_by_hand() {
        let registry = HotRuleRegistry::standard();
        let area = QualifiedArea::parse("api:billing").unwrap();
        for (name, area, by_hand) in [
            ("regenerate", None, false),
            ("owner", Some(&area), true),
            ("warn-shared", None, true),
            ("free", None, true),
        ] {
            let kind = registry.kind(name).unwrap();
            let rule = registry
                .build(&HotRuleConfig {
                    kind: &kind,
                    area,
                    note: None,
                })
                .unwrap();
            assert_eq!(rule.merged_by_hand(), by_hand, "{name}");
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
        let rule = registry
            .build(&HotRuleConfig {
                kind: &kind,
                area: None,
                note: None,
            })
            .unwrap();
        assert!(
            rule.merged_by_hand(),
            "a kind that does not say otherwise is merged by hand"
        );
    }
}
