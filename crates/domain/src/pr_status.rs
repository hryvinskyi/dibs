//! The status a forge shows on a pull request: whether a held slot freezes the branch it would be
//! merged into. It is computed here from the slots and the settings, so whatever writes it to the
//! forge holds no rule of its own.
//!
//! A status is visible to everyone who can read the repository, so how much it says is the
//! project's choice: without the holder described, it names neither the slot, its holder nor their
//! purpose.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, UtcOffset};

use crate::handle::Handle;
use crate::ids::MemberId;
use crate::slot::{Purpose, RepoRef, SlotBoard, SlotCatalog, SlotName};
use crate::text::{has_control, string_value};

/// The name a forge files the status under (`dibs`): 1 to 100 characters after trimming, with no
/// control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StatusContext(String);

/// Why a text is not a status context.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StatusContextError {
    /// The trimmed text is empty, longer than 100 characters, or has control characters.
    #[error("a status context is 1 to 100 characters with no control characters")]
    Malformed,
}

impl StatusContext {
    /// The context of a project that names none.
    pub const DEFAULT: &'static str = "dibs";

    /// Parses a context, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`StatusContextError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, StatusContextError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > 100 || has_control(value) {
            return Err(StatusContextError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(StatusContext, StatusContextError);

impl Default for StatusContext {
    fn default() -> Self {
        Self(Self::DEFAULT.to_owned())
    }
}

/// Whether, and how, a repository's pull requests get a status.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrStatusSettings {
    /// Whether they get one at all; off unless the project turns it on.
    pub enabled: bool,
    /// The name it is filed under.
    pub context: StatusContext,
    /// Whether its description names the slot, its holder and their purpose.
    pub describe_holder: bool,
}

/// The state of a pull request's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    /// Nothing freezes its base branch.
    Success,
    /// A held slot freezes its base branch.
    Failure,
}

/// The hold that freezes a base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freeze {
    /// The slot held.
    pub slot: SlotName,
    /// Its holder.
    pub holder: MemberId,
    /// Why they hold it.
    pub purpose: Purpose,
    /// When the hold ends unless renewed.
    pub until: OffsetDateTime,
}

/// A pull request's status: failure while a hold freezes its base branch, success otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatus {
    /// Its state.
    pub state: PrState,
    /// The hold freezing the base branch, when one does.
    pub freeze: Option<Freeze>,
}

/// The most characters of a status's description.
pub const MAX_DESCRIPTION_CHARS: usize = 140;

/// The status of a pull request into `base`: failure while any member holds a slot that freezes
/// `base`, by its name or by a pattern that covers it — the first such hold in the board's order —
/// success otherwise.
pub fn pr_status(base: &RepoRef, catalog: &SlotCatalog, board: &SlotBoard) -> PrStatus {
    let freeze = board.holds.iter().find_map(|hold| {
        let spec = catalog.get(&hold.slot)?;
        spec.freezes_branch(base).then(|| Freeze {
            slot: hold.slot.clone(),
            holder: hold.member,
            purpose: hold.purpose.clone(),
            until: hold.expires_at,
        })
    });
    PrStatus {
        state: if freeze.is_some() {
            PrState::Failure
        } else {
            PrState::Success
        },
        freeze,
    }
}

impl PrStatus {
    /// The description shown with the status, at most [`MAX_DESCRIPTION_CHARS`]: with
    /// `describe_holder`, the slot, its holder (by `holder`'s handle) and their purpose; without,
    /// only that a freezing action is in progress and until when, in UTC.
    pub fn description(&self, describe_holder: bool, holder: Option<&Handle>) -> String {
        let Some(freeze) = &self.freeze else {
            return "Nothing freezes the base branch; it may be merged.".to_owned();
        };
        let until = freeze.until.to_offset(UtcOffset::UTC);
        let until = format!("{:02}:{:02} UTC", until.hour(), until.minute());
        let text = if describe_holder {
            let who = holder.map_or("a member", Handle::as_str);
            format!(
                "{who} holds {} until {until}: {}",
                freeze.slot, freeze.purpose
            )
        } else {
            format!("A freezing action is in progress until {until}.")
        };
        text.chars().take(MAX_DESCRIPTION_CHARS).collect()
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::ids::SessionId;
    use crate::session::HostName;
    use crate::slot::{Liveness, SlotHold};
    use crate::testing::{ALICE, at, settings};

    fn catalog() -> SlotCatalog {
        settings(
            r#"{"repos":[{"id":"api","remote":"example.com/acme/api"}],
                "slots":[{"name":"db:migrate","max_hold":"1h","freezes":["api:refs/heads/main"]},
                         {"name":"deploy:prod","max_hold":"30m"}]}"#,
        )
        .slots()
        .clone()
    }

    fn hold(slot: &str, purpose: &str) -> SlotHold {
        SlotHold {
            slot: SlotName::parse(slot).unwrap(),
            member: ALICE,
            session: SessionId(1),
            host: HostName::parse("laptop").unwrap(),
            purpose: Purpose::parse(purpose).unwrap(),
            item: None,
            liveness: Liveness::Session,
            taken_at: at(0),
            expires_at: at(0) + Duration::hours(14) + Duration::minutes(40),
            last_seen: at(0),
            stalled_at: None,
        }
    }

    fn main() -> RepoRef {
        RepoRef::parse("api:refs/heads/main").unwrap()
    }

    #[test]
    fn a_held_slot_that_freezes_the_base_fails_the_status_and_its_release_passes_it() {
        let catalog = catalog();
        let held = SlotBoard {
            holds: vec![hold("db:migrate", "rename the invoices table")],
            ..SlotBoard::default()
        };
        let failing = pr_status(&main(), &catalog, &held);
        assert_eq!(failing.state, PrState::Failure);
        assert_eq!(
            failing.freeze.as_ref().map(|freeze| freeze.holder),
            Some(ALICE)
        );
        let released = pr_status(&main(), &catalog, &SlotBoard::default());
        assert_eq!(released.state, PrState::Success);
        assert_eq!(released.freeze, None);
    }

    #[test]
    fn only_a_slot_that_freezes_that_base_counts() {
        let catalog = catalog();
        let other = SlotBoard {
            holds: vec![hold("deploy:prod", "ship 2.4")],
            ..SlotBoard::default()
        };
        assert_eq!(pr_status(&main(), &catalog, &other).state, PrState::Success);
        let migrating = SlotBoard {
            holds: vec![hold("db:migrate", "rename")],
            ..SlotBoard::default()
        };
        let release = RepoRef::parse("api:refs/heads/release").unwrap();
        assert_eq!(
            pr_status(&release, &catalog, &migrating).state,
            PrState::Success
        );
    }

    #[test]
    fn a_held_slot_that_freezes_branches_by_a_pattern_fails_every_base_it_covers() {
        let catalog = settings(
            r#"{"repos":[{"id":"api","remote":"example.com/acme/api"},
                         {"id":"web","remote":"example.com/acme/web"}],
                "slots":[{"name":"release:cut","max_hold":"1h",
                          "freezes":["api:refs/heads/release/*"]}]}"#,
        )
        .slots()
        .clone();
        let held = SlotBoard {
            holds: vec![hold("release:cut", "cut 2026.10.3")],
            ..SlotBoard::default()
        };
        let base = |text: &str| RepoRef::parse(text).unwrap();
        for covered in [
            "api:refs/heads/release/2026.10.3",
            "api:refs/heads/release/2026.11.1",
        ] {
            let status = pr_status(&base(covered), &catalog, &held);
            assert_eq!(status.state, PrState::Failure, "{covered}");
            assert_eq!(
                status.freeze.map(|freeze| freeze.slot.to_string()),
                Some("release:cut".to_owned())
            );
        }
        for free in [
            "api:refs/heads/main",
            "api:refs/heads/release",
            "web:refs/heads/release/2026.10.3",
        ] {
            assert_eq!(
                pr_status(&base(free), &catalog, &held).state,
                PrState::Success,
                "{free}"
            );
        }
        assert_eq!(
            pr_status(
                &base("api:refs/heads/release/2026.10.3"),
                &catalog,
                &SlotBoard::default()
            )
            .state,
            PrState::Success
        );
    }

    #[test]
    fn the_description_names_the_holder_only_when_the_project_says_so() {
        let catalog = catalog();
        let held = SlotBoard {
            holds: vec![hold("db:migrate", "rename the invoices table")],
            ..SlotBoard::default()
        };
        let status = pr_status(&main(), &catalog, &held);
        let alice = Handle::parse("alice").unwrap();
        assert_eq!(
            status.description(false, Some(&alice)),
            "A freezing action is in progress until 14:40 UTC."
        );
        assert_eq!(
            status.description(true, Some(&alice)),
            "alice holds db:migrate until 14:40 UTC: rename the invoices table"
        );
        let long = SlotBoard {
            holds: vec![hold("db:migrate", &"p".repeat(200))],
            ..SlotBoard::default()
        };
        let description = pr_status(&main(), &catalog, &long).description(true, Some(&alice));
        assert_eq!(description.chars().count(), MAX_DESCRIPTION_CHARS);
        let free = pr_status(&main(), &catalog, &SlotBoard::default());
        assert!(!free.description(true, None).contains("dibs"));
    }

    #[test]
    fn a_context_is_one_to_a_hundred_characters() {
        assert_eq!(StatusContext::default().as_str(), "dibs");
        assert_eq!(
            StatusContext::parse(" ci/coordination ").unwrap().as_str(),
            "ci/coordination"
        );
        for bad in ["", "  ", "a\nb"] {
            assert!(StatusContext::parse(bad).is_err(), "{bad:?}");
        }
        assert!(StatusContext::parse(&"c".repeat(101)).is_err());
    }
}
