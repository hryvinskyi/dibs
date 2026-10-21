//! The slots of a settings document, validated into a catalog and written back.

use thiserror::Error;
use time::Duration;

use super::RepoSpec;
use super::document::{CommandDocument, SlotDocument};
use super::duration::{DurationError, format_duration, parse_duration};
use crate::hot_rule::{Note, NoteError};
use crate::slot::{
    BranchPattern, BranchPatternError, CommandPattern, DEFAULT_MAX_WAIT, DEFAULT_OFFER_FOR,
    MAX_HOLD_RANGE, MAX_PATTERNS, PatternError, SlotCatalog, SlotGroup, SlotGroupError, SlotName,
    SlotNameError, SlotSpec,
};

const OFFER_FOR_RANGE: (Duration, Duration) = (Duration::minutes(1), Duration::hours(1));
const MAX_WAIT_RANGE: (Duration, Duration) = (Duration::minutes(1), Duration::hours(24));

/// Why one slot of a settings document is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SlotError {
    /// The name breaks its rules.
    #[error(transparent)]
    Name(#[from] SlotNameError),
    /// Another slot has the same name.
    #[error("the name is used by another slot")]
    Duplicate,
    /// `max_hold` is missing.
    #[error("max_hold is required")]
    MaxHoldMissing,
    /// A duration is not one.
    #[error("{field}: {error}")]
    Duration {
        /// The setting.
        field: &'static str,
        /// What is wrong with it.
        #[source]
        error: DurationError,
    },
    /// A duration is outside its range.
    #[error("{field} is between {min} and {max}")]
    OutOfRange {
        /// The setting.
        field: &'static str,
        /// The shortest allowed.
        min: String,
        /// The longest allowed.
        max: String,
    },
    /// More command patterns than allowed.
    #[error("a slot has at most {MAX_PATTERNS} command patterns")]
    TooManyPatterns,
    /// A pattern is refused.
    #[error(transparent)]
    Pattern(#[from] PatternError),
    /// The group's name breaks its rules.
    #[error(transparent)]
    Group(#[from] SlotGroupError),
    /// A frozen ref is malformed.
    #[error("freezes '{text}': {error}")]
    Freeze {
        /// The ref as written.
        text: String,
        /// What is wrong with it.
        #[source]
        error: BranchPatternError,
    },
    /// A frozen ref names a repository the settings do not declare.
    #[error("freezes '{text}' names a repository the project does not declare")]
    FreezeRepo {
        /// The ref as written.
        text: String,
    },
    /// The note breaks its rules.
    #[error(transparent)]
    Note(#[from] NoteError),
}

/// Validates the slots of a document against the repositories already validated.
pub(super) fn parse_slots(
    documents: &[SlotDocument],
    repos: &[RepoSpec],
    lists_repositories: bool,
) -> Result<SlotCatalog, (String, SlotError)> {
    let mut slots: Vec<SlotSpec> = Vec::with_capacity(documents.len());
    for document in documents {
        let named = |error: SlotError| (document.name.clone(), error);
        let spec = parse_slot(document, repos, lists_repositories).map_err(named)?;
        if slots.iter().any(|known| known.name == spec.name) {
            return Err(named(SlotError::Duplicate));
        }
        slots.push(spec);
    }
    Ok(SlotCatalog::new(slots))
}

fn parse_slot(
    document: &SlotDocument,
    repos: &[RepoSpec],
    lists_repositories: bool,
) -> Result<SlotSpec, SlotError> {
    let name = SlotName::parse(&document.name)?;
    let max_hold = document
        .max_hold
        .as_deref()
        .ok_or(SlotError::MaxHoldMissing)?;
    let max_hold = in_range("max_hold", Some(max_hold), MAX_HOLD_RANGE.0, MAX_HOLD_RANGE)?;
    let offer_for = in_range(
        "offer_for",
        document.offer_for.as_deref(),
        DEFAULT_OFFER_FOR,
        OFFER_FOR_RANGE,
    )?;
    let max_wait = in_range(
        "max_wait",
        document.max_wait.as_deref(),
        DEFAULT_MAX_WAIT,
        MAX_WAIT_RANGE,
    )?;
    if document.commands.len() > MAX_PATTERNS {
        return Err(SlotError::TooManyPatterns);
    }
    let commands = document
        .commands
        .iter()
        .map(|command| CommandPattern::new(&command.matches, command.unless.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    let group = document
        .group
        .as_deref()
        .map(SlotGroup::parse)
        .transpose()?;
    let freezes = document
        .freezes
        .iter()
        .map(|text| freeze(text, repos, lists_repositories))
        .collect::<Result<Vec<_>, _>>()?;
    let note = document.note.as_deref().map(Note::parse).transpose()?;
    Ok(SlotSpec {
        name,
        max_hold,
        offer_for,
        max_wait,
        commands,
        run_only: document.run_only,
        group,
        freezes,
        note,
    })
}

fn in_range(
    field: &'static str,
    text: Option<&str>,
    default: Duration,
    (min, max): (Duration, Duration),
) -> Result<Duration, SlotError> {
    let value = match text {
        Some(text) => parse_duration(text).map_err(|error| SlotError::Duration { field, error })?,
        None => default,
    };
    if value < min || value > max {
        return Err(SlotError::OutOfRange {
            field,
            min: format_duration(min),
            max: format_duration(max),
        });
    }
    Ok(value)
}

/// A frozen ref: `repo:refs/heads/<name>` or `repo:refs/heads/<prefix>*`, or, in a project with
/// one repository, the bare ref; a tag is refused.
fn freeze(
    text: &str,
    repos: &[RepoSpec],
    lists_repositories: bool,
) -> Result<BranchPattern, SlotError> {
    let malformed = |error| SlotError::Freeze {
        text: text.to_owned(),
        error,
    };
    let parsed = match (text.split_once(':'), repos.first()) {
        (None, Some(sole)) if !lists_repositories => {
            BranchPattern::named(sole.id.clone(), text).map_err(malformed)?
        }
        _ => BranchPattern::parse(text).map_err(malformed)?,
    };
    if repos.iter().any(|repo| repo.id == *parsed.repo()) {
        Ok(parsed)
    } else {
        Err(SlotError::FreezeRepo {
            text: text.to_owned(),
        })
    }
}

/// The catalog written back as documents that parse to the same slots.
pub(super) fn slot_documents(catalog: &SlotCatalog) -> Vec<SlotDocument> {
    catalog
        .slots()
        .iter()
        .map(|spec| SlotDocument {
            name: spec.name.to_string(),
            max_hold: Some(format_duration(spec.max_hold)),
            offer_for: Some(format_duration(spec.offer_for)),
            max_wait: Some(format_duration(spec.max_wait)),
            commands: spec
                .commands
                .iter()
                .map(|pattern| CommandDocument {
                    matches: pattern.matches_source().to_owned(),
                    unless: pattern.unless_source().map(str::to_owned),
                })
                .collect(),
            run_only: spec.run_only,
            group: spec.group.as_ref().map(ToString::to_string),
            freezes: spec.freezes.iter().map(ToString::to_string).collect(),
            note: spec.note.as_ref().map(ToString::to_string),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hot_rule::HotRuleRegistry;
    use crate::settings::{ProjectSettings, SettingsDocument, SettingsError};
    use crate::slot::RepoRef;
    use crate::testing::{document, two_repo_json};

    fn parse(json: &str) -> Result<ProjectSettings, SettingsError> {
        ProjectSettings::parse(&document(json), &HotRuleRegistry::standard())
    }

    fn refusal(json: &str) -> SlotError {
        match parse(json) {
            Err(SettingsError::Slot { error, .. }) => error,
            other => panic!("not a slot refusal: {other:?}"),
        }
    }

    #[test]
    fn a_document_without_slots_has_none() {
        assert!(parse("{}").unwrap().slots().slots().is_empty());
        assert!(SettingsDocument::default().slots.is_empty());
    }

    #[test]
    fn a_slot_takes_its_defaults_and_round_trips() {
        let settings = parse(
            r#"{"slots":[{"name":"deploy:prod","max_hold":"30m","group":"prod",
                "commands":[{"matches":"^deploy\\.sh","unless":"--dry-run"}],
                "freezes":["refs/heads/main"],"note":"ask in the channel"}]}"#,
        )
        .unwrap();
        let spec = &settings.slots().slots()[0];
        assert_eq!(spec.max_hold, Duration::minutes(30));
        assert_eq!(spec.offer_for, DEFAULT_OFFER_FOR);
        assert_eq!(spec.max_wait, DEFAULT_MAX_WAIT);
        assert!(!spec.run_only);
        assert_eq!(spec.freezes[0].to_string(), "default:refs/heads/main");
        let again =
            ProjectSettings::parse(&settings.to_document(), &HotRuleRegistry::standard()).unwrap();
        assert_eq!(again, settings);
    }

    type Expected = fn(&SlotError) -> bool;

    #[test]
    fn invalid_slots_are_refused_naming_the_slot() {
        let cases: &[(&str, Expected)] = &[
            (r#"{"slots":[{"name":"Deploy","max_hold":"1h"}]}"#, |e| {
                matches!(e, SlotError::Name(_))
            }),
            (r#"{"slots":[{"name":"x"}]}"#, |e| {
                matches!(e, SlotError::MaxHoldMissing)
            }),
            (r#"{"slots":[{"name":"x","max_hold":"25h"}]}"#, |e| {
                matches!(
                    e,
                    SlotError::OutOfRange {
                        field: "max_hold",
                        ..
                    }
                )
            }),
            (r#"{"slots":[{"name":"x","max_hold":"30s"}]}"#, |e| {
                matches!(
                    e,
                    SlotError::OutOfRange {
                        field: "max_hold",
                        ..
                    }
                )
            }),
            (
                r#"{"slots":[{"name":"x","max_hold":"1h","offer_for":"2h"}]}"#,
                |e| {
                    matches!(
                        e,
                        SlotError::OutOfRange {
                            field: "offer_for",
                            ..
                        }
                    )
                },
            ),
            (
                r#"{"slots":[{"name":"x","max_hold":"1h","max_wait":"soon"}]}"#,
                |e| {
                    matches!(
                        e,
                        SlotError::Duration {
                            field: "max_wait",
                            ..
                        }
                    )
                },
            ),
            (
                r#"{"slots":[{"name":"x","max_hold":"1h"},{"name":"x","max_hold":"1h"}]}"#,
                |e| matches!(e, SlotError::Duplicate),
            ),
            (
                r#"{"slots":[{"name":"x","max_hold":"1h","commands":[{"matches":"("}]}]}"#,
                |e| matches!(e, SlotError::Pattern(PatternError::Invalid { .. })),
            ),
            (
                r#"{"slots":[{"name":"x","max_hold":"1h","group":"A"}]}"#,
                |e| matches!(e, SlotError::Group(_)),
            ),
            (
                r#"{"slots":[{"name":"x","max_hold":"1h","freezes":["main"]}]}"#,
                |e| matches!(e, SlotError::Freeze { .. }),
            ),
            (
                r#"{"slots":[{"name":"x","max_hold":"1h","freezes":["api:refs/heads/main"]}]}"#,
                |e| matches!(e, SlotError::FreezeRepo { .. }),
            ),
        ];
        for (json, expected) in cases {
            let error = refusal(json);
            assert!(expected(&error), "{json}: {error:?}");
        }
        let many: Vec<String> = (0..21)
            .map(|n| format!(r#"{{"matches":"^cmd{n}"}}"#))
            .collect();
        let json = format!(
            r#"{{"slots":[{{"name":"x","max_hold":"1h","commands":[{}]}}]}}"#,
            many.join(",")
        );
        assert_eq!(refusal(&json), SlotError::TooManyPatterns);
        let Err(error) = parse(r#"{"slots":[{"name":"x"}]}"#) else {
            panic!("accepted");
        };
        assert_eq!(error.to_string(), "slot 'x': max_hold is required");
    }

    #[test]
    fn a_project_with_repositories_names_the_repository_of_each_frozen_ref() {
        let with = |freezes: &str| {
            let base = two_repo_json("advise");
            let slots = format!(
                r#","slots":[{{"name":"deploy","max_hold":"1h","freezes":[{freezes}]}}]}}"#
            );
            format!("{}{slots}", base.trim_end().trim_end_matches('}'))
        };
        assert!(parse(&with(r#""api:refs/heads/main""#)).is_ok());
        assert!(matches!(
            refusal(&with(r#""refs/heads/main""#)),
            SlotError::Freeze { .. }
        ));
        assert!(matches!(
            refusal(&with(r#""docs:refs/heads/main""#)),
            SlotError::FreezeRepo { .. }
        ));
        assert_eq!(
            refusal(&with(r#""api:refs/tags/v*""#)),
            SlotError::Freeze {
                text: "api:refs/tags/v*".to_owned(),
                error: BranchPatternError::Tag,
            }
        );
    }

    #[test]
    fn a_slot_freezes_branches_by_a_prefix_that_ends_in_a_star_and_round_trips() {
        let with = |freezes: &str| {
            let base = two_repo_json("advise");
            let slots = format!(
                r#","slots":[{{"name":"deploy","max_hold":"1h","freezes":[{freezes}]}}]}}"#
            );
            format!("{}{slots}", base.trim_end().trim_end_matches('}'))
        };
        let settings = parse(&with(r#""api:refs/heads/release/*","api:refs/heads/main""#)).unwrap();
        let spec = &settings.slots().slots()[0];
        assert_eq!(spec.freezes[0].to_string(), "api:refs/heads/release/*");
        let branch = |text: &str| RepoRef::parse(text).unwrap();
        assert!(spec.freezes_branch(&branch("api:refs/heads/release/2026.10.3")));
        assert!(spec.freezes_branch(&branch("api:refs/heads/main")));
        assert!(!spec.freezes_branch(&branch("api:refs/heads/release")));
        assert!(!spec.freezes_branch(&branch("api:refs/heads/bob/42-rounding")));
        assert!(
            !spec.freezes_branch(&branch("web:refs/heads/release/2026.10.3")),
            "a pattern covers the branches of its own repository"
        );
        let document = settings.to_document();
        assert_eq!(
            document.slots[0].freezes,
            ["api:refs/heads/release/*", "api:refs/heads/main"]
        );
        let again = ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap();
        assert_eq!(again, settings);

        for (text, error) in [
            ("api:refs/heads/*", BranchPatternError::EmptyPrefix),
            ("api:refs/heads/release/**", BranchPatternError::DoubleStar),
            (
                "api:refs/heads/release/*/x",
                BranchPatternError::StarNotAtEnd,
            ),
            ("api:refs/heads/.release/*", BranchPatternError::Name),
            ("api:refs/tags/v*", BranchPatternError::Tag),
        ] {
            assert_eq!(
                refusal(&with(&format!(r#""{text}""#))),
                SlotError::Freeze {
                    text: text.to_owned(),
                    error,
                },
                "{text}"
            );
        }
        let single = parse(
            r#"{"slots":[{"name":"cut","max_hold":"1h","freezes":["refs/heads/release/*"]}]}"#,
        )
        .unwrap();
        assert_eq!(
            single.slots().slots()[0].freezes[0].to_string(),
            "default:refs/heads/release/*"
        );
        let Err(error) =
            parse(r#"{"slots":[{"name":"cut","max_hold":"1h","freezes":["refs/heads/*"]}]}"#)
        else {
            panic!("a pattern with no prefix was accepted");
        };
        let said = error.to_string();
        assert!(
            said.starts_with("slot 'cut': freezes 'refs/heads/*':")
                && said.contains("would cover every branch"),
            "{said}"
        );
    }

    #[test]
    fn a_slot_freezes_branches_and_refuses_a_tag_saying_so() {
        let Err(error) =
            parse(r#"{"slots":[{"name":"release","max_hold":"1h","freezes":["refs/tags/v1.0"]}]}"#)
        else {
            panic!("a tag was accepted as a frozen ref");
        };
        let said = error.to_string();
        assert!(
            said.starts_with("slot 'release': freezes 'refs/tags/v1.0':")
                && said.contains("freezes branches, not tags"),
            "{said}"
        );
    }
}
