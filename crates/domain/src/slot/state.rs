//! Where the project's slots stand: who holds which, who waits for which group, and to whom a group
//! is on offer.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::ids::{ItemId, MemberId, RunId, SessionId, WaiterId};
use crate::session::HostName;
use crate::slot::spec::{GroupKey, Purpose, SlotCatalog, SlotName};
use crate::text::string_value;

/// How long a hold kept by a run's heartbeats goes without one before it is stalled.
pub const STALL_AFTER: Duration = Duration::minutes(2);

/// The name of a forge workflow (`deploy.yml`): 1 to 100 characters with no whitespace or control
/// characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WorkflowName(String);

/// Why a text is not a workflow name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowNameError {
    /// The text breaks the rules above.
    #[error("a workflow is named in 1 to 100 characters with no spaces or control characters")]
    Malformed,
}

impl WorkflowName {
    /// Parses a workflow name.
    ///
    /// # Errors
    /// [`WorkflowNameError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, WorkflowNameError> {
        let ok = !value.is_empty()
            && value.chars().count() <= 100
            && !value.chars().any(|c| c.is_whitespace() || c.is_control());
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(WorkflowNameError::Malformed)
        }
    }
}

string_value!(WorkflowName, WorkflowNameError);

/// A commit id: 7 to 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CommitSha(String);

/// Why a text is not a commit id.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommitShaError {
    /// The text breaks the rules above.
    #[error("a commit is named by 7 to 64 lowercase hexadecimal digits")]
    Malformed,
}

impl CommitSha {
    /// Parses a commit id.
    ///
    /// # Errors
    /// [`CommitShaError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, CommitShaError> {
        let ok = (7..=64).contains(&value.len())
            && value
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(CommitShaError::Malformed)
        }
    }
}

string_value!(CommitSha, CommitShaError);

/// A short word naming how something ended: a signal (`SIGTERM`, `ctrl-c`) or a workflow run's
/// conclusion (`success`): 1 to 32 ASCII letters, digits, `-` and `_`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EndWord(String);

/// Why a text is not such a word.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EndWordError {
    /// The text breaks the rules above.
    #[error("1 to 32 letters, digits, '-' or '_'")]
    Malformed,
}

impl EndWord {
    /// Parses the word.
    ///
    /// # Errors
    /// [`EndWordError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, EndWordError> {
        let ok = !value.is_empty()
            && value.len() <= 32
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if ok {
            Ok(Self(value.to_owned()))
        } else {
            Err(EndWordError::Malformed)
        }
    }
}

string_value!(EndWord, EndWordError);

/// How the action done under a hold ended, when the release says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunExit {
    /// The command exited with this status.
    Code(i32),
    /// The command ended on this signal.
    Signal(EndWord),
    /// The workflow run finished with this conclusion.
    Conclusion(EndWord),
}

/// How a hold stays alive, fixed when it is taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Liveness {
    /// Kept by the member's heartbeats; ends at its expiry and never stalls.
    Session,
    /// Kept by the heartbeats of one run of a command; stalled after [`STALL_AFTER`] without one.
    Run {
        /// The run.
        run: RunId,
    },
    /// Kept until the forge reports that this workflow's run for this commit finished, or until
    /// its expiry; it never stalls.
    UntilRun {
        /// The workflow.
        workflow: WorkflowName,
        /// The commit it runs for.
        sha: CommitSha,
    },
}

impl Liveness {
    /// The stable name of the mode: `session`, `run` or `until_run`.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Run { .. } => "run",
            Self::UntilRun { .. } => "until_run",
        }
    }

    /// The liveness a renewal keeps: a plain take never weakens a hold kept by a run or a workflow
    /// to one kept by heartbeats alone; any other request is taken as asked.
    pub(crate) fn renewed(&self, asked: &Self) -> Self {
        match asked {
            Self::Session => self.clone(),
            Self::Run { .. } | Self::UntilRun { .. } => asked.clone(),
        }
    }
}

/// A member's time-boxed right to do a slot's action. The hold is the member's; `session` and
/// `host` record where it was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotHold {
    /// The slot.
    pub slot: SlotName,
    /// The member holding it.
    pub member: MemberId,
    /// The session that took or last renewed it.
    pub session: SessionId,
    /// That session's host.
    pub host: HostName,
    /// Why it is held.
    pub purpose: Purpose,
    /// The work item it is held for, when one was named.
    pub item: Option<ItemId>,
    /// How it stays alive.
    pub liveness: Liveness,
    /// When it was taken.
    pub taken_at: OffsetDateTime,
    /// When it ends unless renewed.
    pub expires_at: OffsetDateTime,
    /// The last sign of life of what keeps it.
    pub last_seen: OffsetDateTime,
    /// When it was found stalled, while it is.
    pub stalled_at: Option<OffsetDateTime>,
}

impl SlotHold {
    /// Whether it is stalled: a run that holds it stopped sending heartbeats.
    pub fn is_stalled(&self) -> bool {
        self.stalled_at.is_some()
    }

    /// Whether it is kept by this run.
    pub fn is_run(&self, run: RunId) -> bool {
        self.liveness == Liveness::Run { run }
    }
}

/// A member waiting for a group's hold to end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotWaiter {
    /// The place.
    pub id: WaiterId,
    /// The slot asked for.
    pub slot: SlotName,
    /// The member.
    pub member: MemberId,
    /// Why they want it.
    pub purpose: Purpose,
    /// When they joined the queue.
    pub since: OffsetDateTime,
    /// Their last sign of life.
    pub last_seen: OffsetDateTime,
}

/// A group's hold offered to the member who was first in its queue, until a deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotOffer {
    /// The slot they asked for.
    pub slot: SlotName,
    /// The member.
    pub member: MemberId,
    /// Why they want it.
    pub purpose: Purpose,
    /// When the offer lapses.
    pub until: OffsetDateTime,
}

/// Every hold, waiter and offer of one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotBoard {
    /// The holds, at most one per slot.
    pub holds: Vec<SlotHold>,
    /// The waiters, at most one per member and group.
    pub waiters: Vec<SlotWaiter>,
    /// The offers, at most one per group.
    pub offers: Vec<SlotOffer>,
}

impl SlotBoard {
    /// Whether nothing is held, awaited or offered.
    pub fn is_empty(&self) -> bool {
        self.holds.is_empty() && self.waiters.is_empty() && self.offers.is_empty()
    }

    /// The hold of this slot.
    pub fn hold(&self, slot: &SlotName) -> Option<&SlotHold> {
        self.holds.iter().find(|hold| hold.slot == *slot)
    }

    /// The holds of a group.
    pub fn group_holds<'a>(
        &'a self,
        catalog: &'a SlotCatalog,
        key: &'a GroupKey,
    ) -> impl Iterator<Item = &'a SlotHold> + 'a {
        self.holds
            .iter()
            .filter(move |hold| catalog.in_group(&hold.slot, key))
    }

    /// The offer of a group.
    pub fn group_offer(&self, catalog: &SlotCatalog, key: &GroupKey) -> Option<&SlotOffer> {
        self.offers
            .iter()
            .find(|offer| catalog.in_group(&offer.slot, key))
    }

    /// The waiters of a group, first in line first.
    pub fn group_queue(&self, catalog: &SlotCatalog, key: &GroupKey) -> Vec<&SlotWaiter> {
        let mut queue: Vec<&SlotWaiter> = self
            .waiters
            .iter()
            .filter(|waiter| catalog.in_group(&waiter.slot, key))
            .collect();
        queue.sort_by_key(|waiter| (waiter.since, waiter.id));
        queue
    }

    /// Where a slot stands, its group's hold and queue included; `None` for an undeclared slot.
    pub fn standing(&self, catalog: &SlotCatalog, slot: &SlotName) -> Option<SlotStanding> {
        let spec = catalog.get(slot)?;
        let key = spec.group_key();
        Some(SlotStanding {
            slot: slot.clone(),
            hold: self.group_holds(catalog, &key).next().cloned(),
            queue: self
                .group_queue(catalog, &key)
                .into_iter()
                .cloned()
                .collect(),
            offer: self.group_offer(catalog, &key).cloned(),
        })
    }

    /// Records a sign of life of the member: the holds their heartbeats keep and their places in
    /// queues are seen now. A hold kept by a run or a workflow is left alone.
    pub fn refresh_member(&mut self, member: MemberId, now: OffsetDateTime) -> bool {
        let mut changed = false;
        for hold in &mut self.holds {
            if hold.member == member && hold.liveness == Liveness::Session {
                hold.last_seen = now;
                changed = true;
            }
        }
        for waiter in &mut self.waiters {
            if waiter.member == member {
                waiter.last_seen = now;
                changed = true;
            }
        }
        changed
    }
}

/// Where one slot stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotStanding {
    /// The slot.
    pub slot: SlotName,
    /// The hold of its group: of this slot, or of another slot of the group.
    pub hold: Option<SlotHold>,
    /// Its group's queue, first in line first.
    pub queue: Vec<SlotWaiter>,
    /// Its group's offer.
    pub offer: Option<SlotOffer>,
}

impl SlotStanding {
    /// The 1-based place of the member in the queue.
    pub fn position_of(&self, member: MemberId) -> Option<usize> {
        self.queue
            .iter()
            .position(|waiter| waiter.member == member)
            .map(|index| index + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_values_follow_their_rules() {
        assert!(WorkflowName::parse("deploy.yml").is_ok());
        assert!(WorkflowName::parse("deploy yml").is_err());
        assert!(WorkflowName::parse("").is_err());
        assert!(CommitSha::parse("abc1234").is_ok());
        assert!(CommitSha::parse("ABC1234").is_err());
        assert!(CommitSha::parse("abc12").is_err());
        assert!(EndWord::parse("SIGTERM").is_ok());
        assert!(EndWord::parse("ctrl-break").is_ok());
        assert!(EndWord::parse("a b").is_err());
    }

    #[test]
    fn exits_and_liveness_serialise_in_their_names() {
        let exit = serde_json::to_value(RunExit::Code(3)).unwrap();
        assert_eq!(exit, serde_json::json!({"code": 3}));
        let signal = RunExit::Signal(EndWord::parse("SIGTERM").unwrap());
        assert_eq!(
            serde_json::to_value(&signal).unwrap(),
            serde_json::json!({"signal": "SIGTERM"})
        );
        let run = Liveness::Run { run: RunId(7) };
        assert_eq!(
            serde_json::to_value(&run).unwrap(),
            serde_json::json!({"mode": "run", "run": 7})
        );
        assert_eq!(Liveness::Session.name(), "session");
    }

    #[test]
    fn a_plain_renewal_keeps_a_stronger_liveness() {
        let run = Liveness::Run { run: RunId(1) };
        assert_eq!(run.renewed(&Liveness::Session), run);
        assert_eq!(Liveness::Session.renewed(&run), run);
        let other = Liveness::Run { run: RunId(2) };
        assert_eq!(run.renewed(&other), other);
    }
}
