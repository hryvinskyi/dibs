//! Work items: how they are keyed, titled and created, and the facts an importer reports about
//! them.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::area::{AreaName, QualifiedArea};
use crate::ids::ItemId;
use crate::member::Role;
use crate::ref_scheme::{RefSchemeRegistry, shipped_canonical};
use crate::repo::{RemoteUrl, RepoId};
use crate::settings::{AreaLookupError, ProjectSettings};
use crate::text::{has_control, is_slug, string_value};

/// A reference to a work item in an outside tracker, `scheme:value` (`gh:acme/web#12`).
///
/// The scheme is 1 to 16 characters: a lowercase letter, then lowercase letters, digits, `+`, `.`
/// or `-`. The value is 1 to 200 characters with no control characters and no whitespace at either
/// end. A value of a scheme this crate ships is kept in that scheme's canonical form (a `gh` value
/// in lower case), so two spellings of one reference are one reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExternalRef {
    scheme: String,
    value: String,
}

/// Why a text is not an external reference.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExternalRefError {
    /// The text has no `:` between scheme and value.
    #[error("an external reference is written 'scheme:value'")]
    MissingScheme,
    /// The scheme breaks its character rules.
    #[error(
        "a reference scheme is 1 to 16 characters: a lowercase letter, then lowercase letters, digits, '+', '.' or '-'"
    )]
    InvalidScheme,
    /// The value is empty, too long, has control characters or whitespace at an end.
    #[error(
        "a reference value is 1 to 200 characters, with no control characters and no spaces at either end"
    )]
    InvalidValue,
    /// The value does not have the shape its scheme gives its values.
    #[error("a '{scheme}' reference is written {scheme}:{shape}")]
    Refused {
        /// The scheme.
        scheme: String,
        /// The shape its values have.
        shape: &'static str,
    },
}

impl ExternalRef {
    /// Parses `scheme:value`, splitting at the first `:`, and writes the value of a shipped scheme
    /// in its canonical form.
    ///
    /// # Errors
    /// [`ExternalRefError`] naming the part that is invalid.
    pub fn parse(text: &str) -> Result<Self, ExternalRefError> {
        let (scheme, value) = text
            .split_once(':')
            .ok_or(ExternalRefError::MissingScheme)?;
        let mut scheme_chars = scheme.chars();
        let scheme_ok = scheme_chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && scheme.len() <= 16
            && scheme_chars
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+.-".contains(c));
        if !scheme_ok {
            return Err(ExternalRefError::InvalidScheme);
        }
        let value_ok = !value.is_empty()
            && value.chars().count() <= 200
            && !has_control(value)
            && value.trim() == value;
        if !value_ok {
            return Err(ExternalRefError::InvalidValue);
        }
        let value =
            shipped_canonical(scheme, value).map_err(|refused| ExternalRefError::Refused {
                scheme: scheme.to_owned(),
                shape: refused.shape,
            })?;
        Ok(Self {
            scheme: scheme.to_owned(),
            value,
        })
    }

    /// The scheme (`gh`).
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The value after the scheme (`acme/web#12`).
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for ExternalRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.scheme, self.value)
    }
}

impl TryFrom<String> for ExternalRef {
    type Error = ExternalRefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ExternalRef> for String {
    fn from(value: ExternalRef) -> String {
        value.to_string()
    }
}

/// How a work item is named by the people and tools that refer to it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemKey {
    /// By its reference in an outside tracker.
    External(ExternalRef),
    /// By its local id.
    Local(ItemId),
}

impl ItemKey {
    /// Parses a key as people type it: digits are a local id, anything else an external reference.
    ///
    /// # Errors
    /// [`ExternalRefError`] when the text is neither.
    pub fn parse(text: &str) -> Result<Self, ExternalRefError> {
        if !text.is_empty()
            && text.chars().all(|c| c.is_ascii_digit())
            && let Ok(number) = text.parse::<u64>()
        {
            return Ok(Self::Local(ItemId(number)));
        }
        ExternalRef::parse(text).map(Self::External)
    }
}

impl fmt::Display for ItemKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::External(reference) => reference.fmt(f),
            Self::Local(id) => id.fmt(f),
        }
    }
}

/// A work item's title: 1 to 200 characters after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Title(String);

/// Why a text is not a title.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TitleError {
    /// The trimmed text is empty or longer than 200 characters, or has control characters.
    #[error("a title is 1 to 200 characters with no control characters")]
    Malformed,
}

impl Title {
    /// Parses a title, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`TitleError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, TitleError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 200 || has_control(value) {
            return Err(TitleError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(Title, TitleError);

/// Where a work item stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// Nobody holds it.
    Open,
    /// A member holds it.
    Claimed,
    /// Completed.
    Done,
    /// Given up.
    Dropped,
}

/// How a claim may be made on an item in its present state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOpening {
    /// The item is open or claimed: the claim is decided as any other.
    AsItStands,
    /// The item was completed, but the tracker it is imported from still has it open: the work
    /// came back, and the claim brings the item back with it.
    Reopens,
}

/// Why an item cannot be claimed: it is done for good, or it was given up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the item is done or dropped, and nothing has it open")]
pub struct ItemShut {
    /// Where the item stands.
    pub status: ItemStatus,
}

/// Whether an item may be claimed, given where it stands, whether an importer lists it, and
/// whether its tracker closed it.
///
/// A completed item stays completed unless the tracker it is imported from still has it open:
/// work that went on to be checked and came back is claimed again on the same item. A local item
/// has no tracker to say so, and a dropped item was given up, so neither comes back.
///
/// # Errors
/// [`ItemShut`] when the item is done or dropped and no tracker has it open.
pub const fn claim_opening(
    status: ItemStatus,
    imported: bool,
    closed_in_tracker: bool,
) -> Result<ClaimOpening, ItemShut> {
    match status {
        ItemStatus::Open | ItemStatus::Claimed => Ok(ClaimOpening::AsItStands),
        ItemStatus::Done if imported && !closed_in_tracker => Ok(ClaimOpening::Reopens),
        ItemStatus::Done | ItemStatus::Dropped => Err(ItemShut { status }),
    }
}

/// How soon an item should be picked up: 0 to 4, lower is sooner; 2 when nothing says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Priority(u8);

/// Why a number is not a priority.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PriorityError {
    /// The number is above 4.
    #[error("a priority is a whole number from 0 (soonest) to 4")]
    OutOfRange,
}

impl Priority {
    /// The soonest priority.
    pub const HIGHEST: u8 = 0;
    /// The latest priority.
    pub const LOWEST: u8 = 4;

    /// The priority of this number.
    ///
    /// # Errors
    /// [`PriorityError::OutOfRange`] above 4.
    pub fn new(value: u8) -> Result<Self, PriorityError> {
        if value <= Self::LOWEST {
            Ok(Self(value))
        } else {
            Err(PriorityError::OutOfRange)
        }
    }

    /// The number.
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for Priority {
    fn default() -> Self {
        Self(2)
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<u8> for Priority {
    type Error = PriorityError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Priority> for u8 {
    fn from(value: Priority) -> u8 {
        value.0
    }
}

/// The state an outside tracker shows an item in (its board's column, `Todo`), carried as it is
/// and never interpreted: 1 to 40 characters after trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourceStatus(String);

/// Why a text is not a tracker's state.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SourceStatusError {
    /// The trimmed text is empty or longer than 40 characters, or has control characters.
    #[error("a tracker state is 1 to 40 characters with no control characters")]
    Malformed,
}

impl SourceStatus {
    /// Parses a tracker's state, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`SourceStatusError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, SourceStatusError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 40 || has_control(value) {
            return Err(SourceStatusError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(SourceStatus, SourceStatusError);

/// The name of the outside source an importer reads items from (`board`): 1 to 64 characters,
/// lowercase letters, digits, `.`, `_` and `-`, starting with a letter or digit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SourceId(String);

/// Why a text is not a source's name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SourceIdError {
    /// The text breaks the name's character rules.
    #[error(
        "a source name is 1 to 64 characters: lowercase letters, digits, '.', '_' and '-', starting with a letter or digit"
    )]
    Malformed,
}

impl SourceId {
    /// Parses a source's name.
    ///
    /// # Errors
    /// [`SourceIdError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, SourceIdError> {
        if is_slug(value, 64, &['.', '_', '-']) {
            Ok(Self(value.to_owned()))
        } else {
            Err(SourceIdError::Malformed)
        }
    }
}

string_value!(SourceId, SourceIdError);

/// The facts about an item an importer supplies: its title, the areas it will probably touch, how
/// soon it should be done, the tracker's own state of it, and whether the tracker closed it.
///
/// `closed` is the tracker's word, not a claim's end: an item closed upstream keeps any claim on
/// it, and leaves the ready list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemMeta {
    /// The title.
    pub title: Title,
    /// The areas the work will probably touch. Advisory: no claim declares them.
    pub areas_hint: BTreeSet<QualifiedArea>,
    /// How soon.
    pub priority: Priority,
    /// The tracker's state, as it shows it.
    pub source_status: Option<SourceStatus>,
    /// Whether the tracker closed the item.
    pub closed: bool,
}

impl ItemMeta {
    /// The facts of a new item that only has a title.
    pub fn titled(title: Title) -> Self {
        Self {
            title,
            areas_hint: BTreeSet::new(),
            priority: Priority::default(),
            source_status: None,
            closed: false,
        }
    }
}

/// Why an area of a hint was left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HintDropped {
    /// The text is no area name.
    Malformed,
    /// No repository of the project declares it.
    Undeclared,
    /// Several repositories declare an area of that name, and the item's own repository is not
    /// one of them.
    Ambiguous,
}

/// An area of a hint that was left out, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedHint {
    /// The text as given.
    pub text: String,
    /// Why.
    pub reason: HintDropped,
}

/// The areas of a hint the project declares, and the ones left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HintResolution {
    /// The areas kept, qualified by their repository.
    pub areas: BTreeSet<QualifiedArea>,
    /// The texts left out, in the order given.
    pub dropped: Vec<DroppedHint>,
}

/// Resolves the areas an importer hints for an item of `repository`. A bare name is the item's own
/// repository's area when that repository declares it, else the one area of that name the project
/// declares. An area the project does not declare is left out with a note, never refused: a hint
/// only advises.
pub fn resolve_hint(
    texts: &[String],
    repository: &ItemRepository,
    settings: &ProjectSettings,
) -> HintResolution {
    let own = match repository {
        ItemRepository::Listed(repo) => settings.repo(repo),
        ItemRepository::Unlisted(_) | ItemRepository::Unnamed if !settings.lists_repositories() => {
            settings.repos().first()
        }
        ItemRepository::Unlisted(_) | ItemRepository::Unnamed => None,
    };
    let mut resolution = HintResolution::default();
    for text in texts {
        let in_own = AreaName::parse(text).ok().and_then(|name| {
            own.filter(|repo| repo.area(&name).is_some())
                .map(|repo| repo.qualify(&name))
        });
        let resolved = in_own.map_or_else(|| settings.resolve_area(text), Ok);
        match resolved {
            Ok(area) => {
                resolution.areas.insert(area);
            }
            Err(error) => resolution.dropped.push(DroppedHint {
                text: text.clone(),
                reason: match error {
                    AreaLookupError::Malformed { .. } => HintDropped::Malformed,
                    AreaLookupError::NotFound { .. } => HintDropped::Undeclared,
                    AreaLookupError::Ambiguous { .. } => HintDropped::Ambiguous,
                },
            }),
        }
    }
    resolution
}

/// The repository a new work item belongs to, as far as its reference says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemRepository {
    /// One of the project's repositories.
    Listed(RepoId),
    /// A repository the project does not list. Only the importer creates such items; they may be
    /// claimed, but no area exists for them.
    Unlisted(RemoteUrl),
    /// The reference names no repository (a local item, or a scheme that names none).
    Unnamed,
}

/// Why a work item may not be created.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CreateRefused {
    /// A person named a repository the project does not list.
    #[error("the reference names the repository {remote}, which this project does not list")]
    UnlistedRepository {
        /// The repository the reference named.
        remote: RemoteUrl,
    },
}

/// Decides whether `by` may create an item with this reference, and which repository it belongs to.
///
/// A person (admin or member) is refused an item in a repository the project does not list, so a
/// typo in a reference never opens a second item beside the real one. The importer is not refused:
/// a blocker it found in another repository must exist to block anything. A project whose settings
/// list no repositories has no remote to compare with, so there every item is accepted as naming
/// none.
///
/// # Errors
/// [`CreateRefused::UnlistedRepository`] for a person naming an unlisted repository.
pub fn decide_create_item(
    reference: Option<&ExternalRef>,
    by: Role,
    settings: &ProjectSettings,
    schemes: &RefSchemeRegistry,
) -> Result<ItemRepository, CreateRefused> {
    let named = reference.and_then(|r| schemes.repository_of(r));
    let Some(remote) = named.filter(|_| settings.lists_repositories()) else {
        return Ok(ItemRepository::Unnamed);
    };
    if let Some(repo) = settings.repo_by_remote(&remote) {
        return Ok(ItemRepository::Listed(repo.id.clone()));
    }
    if by.is_person() {
        Err(CreateRefused::UnlistedRepository { remote })
    } else {
        Ok(ItemRepository::Unlisted(remote))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_ref_table() {
        let long_value = format!("x:{}", "v".repeat(200));
        let too_long_value = format!("x:{}", "v".repeat(201));
        type Expected<'a> = Result<(&'a str, &'a str), ExternalRefError>;
        let cases: &[(&str, Expected<'_>)] = &[
            ("gh:acme/web#12", Ok(("gh", "acme/web#12"))),
            ("jira:ACME-1", Ok(("jira", "ACME-1"))),
            ("x+y.z-1:a b", Ok(("x+y.z-1", "a b"))),
            (
                "url:https://example.com/a",
                Ok(("url", "https://example.com/a")),
            ),
            (
                long_value.as_str(),
                Ok(("x", long_value.trim_start_matches("x:"))),
            ),
            ("acme-web-12", Err(ExternalRefError::MissingScheme)),
            (":value", Err(ExternalRefError::InvalidScheme)),
            ("1gh:x", Err(ExternalRefError::InvalidScheme)),
            ("GH:x", Err(ExternalRefError::InvalidScheme)),
            ("abcdefghijklmnopq:x", Err(ExternalRefError::InvalidScheme)),
            ("gh_x:y", Err(ExternalRefError::InvalidScheme)),
            ("gh:", Err(ExternalRefError::InvalidValue)),
            ("gh: x", Err(ExternalRefError::InvalidValue)),
            ("gh:x ", Err(ExternalRefError::InvalidValue)),
            ("gh:a\nb", Err(ExternalRefError::InvalidValue)),
            (too_long_value.as_str(), Err(ExternalRefError::InvalidValue)),
            (
                "gh:not-an-issue",
                Err(ExternalRefError::Refused {
                    scheme: "gh".to_owned(),
                    shape: "owner/repo#number",
                }),
            ),
        ];
        for (text, expected) in cases {
            let got = ExternalRef::parse(text);
            match expected {
                Ok((scheme, value)) => {
                    let got = got.unwrap();
                    assert_eq!((got.scheme(), got.value()), (*scheme, *value), "{text:?}");
                    assert_eq!(got.to_string(), *text);
                }
                Err(error) => assert_eq!(got.as_ref(), Err(error), "{text:?}"),
            }
        }
    }

    #[test]
    fn a_shipped_scheme_keeps_one_form_and_others_keep_theirs() {
        let typed = ExternalRef::parse("gh:Acme/Widgets#42").unwrap();
        assert_eq!(typed.value(), "acme/widgets#42");
        assert_eq!(typed, ExternalRef::parse("gh:acme/widgets#42").unwrap());
        let read: ExternalRef = serde_json::from_str("\"gh:ACME/Widgets#42\"").unwrap();
        assert_eq!(read, typed);
        assert_eq!(ExternalRef::parse("jira:ACME-1").unwrap().value(), "ACME-1");
        let refused = ExternalRef::parse("gh:acme").unwrap_err();
        assert_eq!(
            refused.to_string(),
            "a 'gh' reference is written gh:owner/repo#number"
        );
    }

    #[test]
    fn a_priority_is_zero_to_four_and_two_by_default() {
        assert_eq!(Priority::default().get(), 2);
        for value in 0..=4 {
            assert_eq!(Priority::new(value).unwrap().get(), value);
        }
        assert_eq!(Priority::new(5), Err(PriorityError::OutOfRange));
        assert!(serde_json::from_str::<Priority>("7").is_err());
        assert_eq!(
            serde_json::to_string(&Priority::new(1).unwrap()).unwrap(),
            "1"
        );
    }

    #[test]
    fn a_tracker_state_and_a_source_name_are_bounded() {
        assert_eq!(SourceStatus::parse("  Todo ").unwrap().as_str(), "Todo");
        assert!(SourceStatus::parse(&"s".repeat(40)).is_ok());
        assert!(SourceStatus::parse(&"s".repeat(41)).is_err());
        assert!(SourceStatus::parse("").is_err());
        assert!(SourceStatus::parse("In\nReview").is_err());
        assert!(SourceId::parse("board").is_ok());
        assert!(SourceId::parse("Board").is_err());
        assert!(SourceId::parse("").is_err());
    }

    fn hint(
        texts: &[&str],
        repository: &ItemRepository,
        settings: &ProjectSettings,
    ) -> HintResolution {
        let texts: Vec<String> = texts.iter().map(|t| (*t).to_owned()).collect();
        resolve_hint(&texts, repository, settings)
    }

    #[test]
    fn a_hint_keeps_the_declared_areas_and_drops_the_rest_with_a_note() {
        let settings = crate::testing::two_repo_settings();
        let web = ItemRepository::Listed(RepoId::parse("web").unwrap());
        let resolution = hint(
            &[
                "billing",
                "search",
                "api:billing-docs",
                "nowhere",
                "Not An Area",
                "web:search",
            ],
            &web,
            &settings,
        );
        let kept: Vec<String> = resolution.areas.iter().map(ToString::to_string).collect();
        assert_eq!(kept, ["api:billing-docs", "api:search", "web:billing"]);
        assert_eq!(
            resolution.dropped,
            [
                DroppedHint {
                    text: "nowhere".to_owned(),
                    reason: HintDropped::Undeclared
                },
                DroppedHint {
                    text: "Not An Area".to_owned(),
                    reason: HintDropped::Malformed
                },
                DroppedHint {
                    text: "web:search".to_owned(),
                    reason: HintDropped::Undeclared
                },
            ]
        );
    }

    #[test]
    fn a_bare_name_two_repositories_declare_is_dropped_unless_the_item_says_which() {
        let settings = crate::testing::two_repo_settings();
        let unnamed = hint(&["billing"], &ItemRepository::Unnamed, &settings);
        assert!(unnamed.areas.is_empty());
        assert_eq!(unnamed.dropped[0].reason, HintDropped::Ambiguous);
        let api = hint(
            &["billing"],
            &ItemRepository::Listed(RepoId::parse("api").unwrap()),
            &settings,
        );
        assert_eq!(
            api.areas
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["api:billing"]
        );
    }

    #[test]
    fn a_single_repository_project_qualifies_a_bare_name_with_its_repository() {
        let settings = crate::testing::settings(
            r#"{"areas":[{"name":"billing","paths":["packages/billing/**"]}]}"#,
        );
        let resolution = hint(&["billing"], &ItemRepository::Unnamed, &settings);
        assert_eq!(
            resolution
                .areas
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["default:billing"]
        );
        assert!(resolution.dropped.is_empty());
    }

    #[test]
    fn item_key_reads_digits_as_a_local_id() {
        assert_eq!(ItemKey::parse("42").unwrap(), ItemKey::Local(ItemId(42)));
        assert_eq!(
            ItemKey::parse("gh:acme/web#12").unwrap(),
            ItemKey::External(ExternalRef::parse("gh:acme/web#12").unwrap())
        );
        assert_eq!(ItemKey::parse("abc"), Err(ExternalRefError::MissingScheme));
        assert_eq!(ItemKey::parse(""), Err(ExternalRefError::MissingScheme));
        assert_eq!(ItemKey::Local(ItemId(7)).to_string(), "7");
    }

    #[test]
    fn title_table() {
        let cases: &[(&str, Option<&str>)] = &[
            ("Fix the invoice rounding", Some("Fix the invoice rounding")),
            ("  padded  ", Some("padded")),
            ("", None),
            ("   ", None),
            ("tab\tinside", None),
            (&"t".repeat(201), None),
        ];
        for (text, expected) in cases {
            assert_eq!(
                Title::parse(text).ok().as_ref().map(Title::as_str),
                *expected,
                "{text:?}"
            );
        }
        assert!(Title::parse(&"t".repeat(200)).is_ok());
    }

    fn github_repos() -> ProjectSettings {
        crate::testing::settings(
            r#"{"repos":[
                {"id":"api","remote":"https://github.com/acme/api.git"},
                {"id":"web","remote":"git@github.com:acme/web.git"}]}"#,
        )
    }

    fn create(text: Option<&str>, by: Role) -> Result<ItemRepository, CreateRefused> {
        let reference = text.map(|t| ExternalRef::parse(t).unwrap());
        decide_create_item(
            reference.as_ref(),
            by,
            &github_repos(),
            &RefSchemeRegistry::standard(),
        )
    }

    #[test]
    fn an_open_or_claimed_item_is_claimed_as_it_stands() {
        for status in [ItemStatus::Open, ItemStatus::Claimed] {
            assert_eq!(
                claim_opening(status, true, true),
                Ok(ClaimOpening::AsItStands)
            );
        }
    }

    #[test]
    fn a_completed_item_its_tracker_still_has_open_is_claimed_again() {
        assert_eq!(
            claim_opening(ItemStatus::Done, true, false),
            Ok(ClaimOpening::Reopens)
        );
    }

    #[test]
    fn a_completed_item_stays_shut_once_its_tracker_closed_it_or_when_it_has_none() {
        for (imported, closed) in [(true, true), (false, false), (false, true)] {
            assert_eq!(
                claim_opening(ItemStatus::Done, imported, closed),
                Err(ItemShut {
                    status: ItemStatus::Done
                }),
                "imported {imported}, closed {closed}"
            );
        }
    }

    #[test]
    fn a_dropped_item_stays_shut_whatever_its_tracker_says() {
        assert_eq!(
            claim_opening(ItemStatus::Dropped, true, false),
            Err(ItemShut {
                status: ItemStatus::Dropped
            })
        );
    }

    #[test]
    fn an_item_in_a_listed_repository_belongs_to_it() {
        for role in [Role::Admin, Role::Member, Role::Importer] {
            assert_eq!(
                create(Some("gh:Acme/Web#12"), role),
                Ok(ItemRepository::Listed(RepoId::parse("web").unwrap()))
            );
        }
    }

    #[test]
    fn a_person_creating_an_item_in_an_unlisted_repository_is_refused() {
        let remote = RemoteUrl::parse("github.com/acme/wbe").unwrap();
        for role in [Role::Admin, Role::Member] {
            assert_eq!(
                create(Some("gh:acme/wbe#12"), role),
                Err(CreateRefused::UnlistedRepository {
                    remote: remote.clone()
                }),
                "{role:?}"
            );
        }
    }

    #[test]
    fn the_importer_creates_an_item_in_an_unlisted_repository() {
        assert_eq!(
            create(Some("gh:acme/search#3"), Role::Importer),
            Ok(ItemRepository::Unlisted(
                RemoteUrl::parse("github.com/acme/search").unwrap()
            ))
        );
    }

    #[test]
    fn a_reference_naming_no_repository_is_not_checked() {
        for text in [None, Some("jira:ACME-12")] {
            assert_eq!(
                create(text, Role::Member),
                Ok(ItemRepository::Unnamed),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_project_without_listed_repositories_accepts_any_reference() {
        let single = crate::testing::settings("{}");
        let reference = ExternalRef::parse("gh:acme/api#1").unwrap();
        assert_eq!(
            decide_create_item(
                Some(&reference),
                Role::Member,
                &single,
                &RefSchemeRegistry::standard()
            ),
            Ok(ItemRepository::Unnamed)
        );
    }
}
