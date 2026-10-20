//! What members' machines report about the guard, and what agents say about it: counts and kinds
//! only. A report names the member, the machine and the day, how often each kind of refusal and
//! failure happened, and how long hooks took; never a path, a command line or a work item. Feedback
//! names the kind of problem and the reason its member wrote, and for a conflict the area it lay
//! in, never its path.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Date, Duration, Month, OffsetDateTime, Time};

use crate::area::QualifiedArea;
use crate::ids::MemberId;
use crate::repo::RepoId;
use crate::session::{AgentLabel, HostName};
use crate::text::string_value;

pub mod stats;

/// What a machine counts: a refusal of the guard by its verdict, a refusal of a push, an answer
/// given without the server, a session started with the guard switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tally {
    /// An edit refused because another member holds the area.
    HeldBy,
    /// An edit refused because it needs a claim first.
    NeedsClaim,
    /// A command refused because it needs a slot.
    NeedsSlot,
    /// A command refused because a slot another member holds freezes what it pushes to.
    Frozen,
    /// A command refused because it pushes to a protected branch the member may not push to.
    NeedsPermission,
    /// A hook answered without a decision because the server could not be reached.
    Unreachable,
    /// A push refused before it left because it goes to a protected branch.
    PushNeedsPermission,
    /// A push refused before it left because a slot another member holds freezes its ref.
    PushFrozen,
    /// A session that started with the guard switched off.
    SwitchedOff,
}

impl Tally {
    /// Every kind, in order.
    pub const ALL: [Self; 9] = [
        Self::HeldBy,
        Self::NeedsClaim,
        Self::NeedsSlot,
        Self::Frozen,
        Self::NeedsPermission,
        Self::Unreachable,
        Self::PushNeedsPermission,
        Self::PushFrozen,
        Self::SwitchedOff,
    ];

    /// The kind's name, as it is written on the wire.
    pub const fn name(self) -> &'static str {
        match self {
            Self::HeldBy => "held_by",
            Self::NeedsClaim => "needs_claim",
            Self::NeedsSlot => "needs_slot",
            Self::Frozen => "frozen",
            Self::NeedsPermission => "needs_permission",
            Self::Unreachable => "unreachable",
            Self::PushNeedsPermission => "push_needs_permission",
            Self::PushFrozen => "push_frozen",
            Self::SwitchedOff => "switched_off",
        }
    }

    /// The kind of this name; `None` for a name this dibs does not know.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tally| tally.name() == name)
    }
}

impl fmt::Display for Tally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A day of the calendar in UTC, written `2026-03-09`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReportDay(Date);

/// Why a text is not a day.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReportDayError {
    /// The text is not `YYYY-MM-DD`, or names no day of the calendar.
    #[error("a day is written YYYY-MM-DD, such as 2026-03-09")]
    Malformed,
}

impl ReportDay {
    /// The day `at` falls on, in UTC.
    pub fn of(at: OffsetDateTime) -> Self {
        Self(at.to_offset(time::UtcOffset::UTC).date())
    }

    /// Parses `YYYY-MM-DD`.
    ///
    /// # Errors
    /// [`ReportDayError::Malformed`] for any other text, or a day the calendar does not have.
    pub fn parse(value: &str) -> Result<Self, ReportDayError> {
        let value = value.trim();
        let mut parts = value.split('-');
        let (Some(year), Some(month), Some(day), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ReportDayError::Malformed);
        };
        let digits =
            |text: &str, len: usize| text.len() == len && text.chars().all(|c| c.is_ascii_digit());
        if !(digits(year, 4) && digits(month, 2) && digits(day, 2)) {
            return Err(ReportDayError::Malformed);
        }
        let year: i32 = year.parse().map_err(|_| ReportDayError::Malformed)?;
        let month: u8 = month.parse().map_err(|_| ReportDayError::Malformed)?;
        let day: u8 = day.parse().map_err(|_| ReportDayError::Malformed)?;
        let month = Month::try_from(month).map_err(|_| ReportDayError::Malformed)?;
        Date::from_calendar_date(year, month, day)
            .map(Self)
            .map_err(|_| ReportDayError::Malformed)
    }

    /// The date.
    pub const fn date(self) -> Date {
        self.0
    }

    /// When the day starts: its midnight in UTC.
    pub fn start(self) -> OffsetDateTime {
        self.0.with_time(Time::MIDNIGHT).assume_utc()
    }

    /// The day after; the same day at the end of the calendar.
    #[must_use]
    pub fn next(self) -> Self {
        self.0.next_day().map_or(self, Self)
    }

    /// The day `days` before this one; the first day of the calendar when that lies before it.
    #[must_use]
    pub fn minus_days(self, days: i64) -> Self {
        self.0
            .checked_sub(Duration::days(days))
            .map_or(Self(Date::MIN), Self)
    }
}

impl fmt::Display for ReportDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}",
            self.0.year(),
            u8::from(self.0.month()),
            self.0.day()
        )
    }
}

impl TryFrom<String> for ReportDay {
    type Error = ReportDayError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ReportDay> for String {
    fn from(value: ReportDay) -> Self {
        value.to_string()
    }
}

/// The name of a hook event as the guard reports it (`before_tool`): 1 to 32 characters of
/// lowercase ASCII letters, digits and `_`, starting with a letter. It is stored and grouped by,
/// never interpreted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HookEventName(String);

/// Why a text is not the name of a hook event.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HookEventNameError {
    /// The text breaks the rules of [`HookEventName`].
    #[error(
        "a hook event is 1 to 32 characters: lowercase letters, digits and '_', starting with a letter"
    )]
    Malformed,
}

impl HookEventName {
    /// Parses the name exactly as given.
    ///
    /// # Errors
    /// [`HookEventNameError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, HookEventNameError> {
        let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
        let starts_well = value.starts_with(|c: char| c.is_ascii_lowercase());
        if starts_well && value.len() <= 32 && value.chars().all(allowed) {
            Ok(Self(value.to_owned()))
        } else {
            Err(HookEventNameError::Malformed)
        }
    }
}

string_value!(HookEventName, HookEventNameError);

/// The upper bounds of the duration buckets, in milliseconds; a run longer than the last falls in
/// one more bucket after them.
pub const BUCKET_BOUNDS_MS: [u64; 6] = [10, 30, 100, 300, 1_000, 2_000];

/// The number of buckets: one per bound and one for longer runs.
pub const BUCKETS: usize = BUCKET_BOUNDS_MS.len() + 1;

/// How many hook runs took how long: a count per bucket of [`BUCKET_BOUNDS_MS`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurationBuckets([u64; BUCKETS]);

impl DurationBuckets {
    /// The buckets of these counts.
    pub const fn new(counts: [u64; BUCKETS]) -> Self {
        Self(counts)
    }

    /// Counts one run of this length.
    pub fn record(&mut self, took: Duration) {
        let millis = u64::try_from(took.whole_milliseconds()).unwrap_or(0);
        let index = BUCKET_BOUNDS_MS
            .iter()
            .position(|bound| millis <= *bound)
            .unwrap_or(BUCKET_BOUNDS_MS.len());
        if let Some(count) = self.0.get_mut(index) {
            *count = count.saturating_add(1);
        }
    }

    /// Adds another histogram's counts to these.
    pub fn add(&mut self, other: &Self) {
        for (count, more) in self.0.iter_mut().zip(other.0) {
            *count = count.saturating_add(more);
        }
    }

    /// The counts, one per bucket.
    pub const fn counts(&self) -> [u64; BUCKETS] {
        self.0
    }

    /// How many runs were counted.
    pub fn total(&self) -> u64 {
        self.0
            .iter()
            .fold(0, |sum, count| sum.saturating_add(*count))
    }

    /// The upper bound, in milliseconds, of the bucket the 95th percentile falls in; `None` for
    /// no runs, or when it falls past the last bound.
    pub fn p95_bound_ms(&self) -> Option<u64> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        let needed = total.saturating_mul(95).div_ceil(100);
        let mut seen = 0_u64;
        for (index, count) in self.0.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= needed {
                return BUCKET_BOUNDS_MS.get(index).copied();
            }
        }
        None
    }
}

/// How long one kind of hook of one kind of agent took on one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookDurations {
    /// The agent's label, as its sessions report it.
    pub agent: AgentLabel,
    /// The hook event.
    pub event: HookEventName,
    /// The runs, by how long they took.
    pub buckets: DurationBuckets,
}

/// The local id a refusal is shown with (`r-7f3a51`): `r-` and 4 to 8 lowercase hexadecimal digits;
/// a machine makes them of six. It names the refusal on the machine that showed it, so feedback
/// about it can say which one; the server keeps it as given and checks nothing about it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RefusalId(String);

/// Why a text is not a refusal's id.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RefusalIdError {
    /// The text is not `r-` and 4 to 8 lowercase hexadecimal digits.
    #[error("a refusal is named as its reason shows it, such as r-7f3a")]
    Malformed,
}

impl RefusalId {
    /// Parses an id, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`RefusalIdError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, RefusalIdError> {
        let value = value.trim();
        let digits = value.strip_prefix("r-").ok_or(RefusalIdError::Malformed)?;
        let hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
        if (4..=8).contains(&digits.len()) && digits.chars().all(hex) {
            Ok(Self(value.to_owned()))
        } else {
            Err(RefusalIdError::Malformed)
        }
    }

    /// The id the low 24 of these bits spell: `r-` and six hexadecimal digits.
    pub fn from_bits(bits: u32) -> Self {
        Self(format!("r-{:06x}", bits & 0x00ff_ffff))
    }
}

string_value!(RefusalId, RefusalIdError);

/// What one machine counted on one day: refusals and failures by kind, and how long hooks took.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayCounts {
    /// How often each kind happened; a kind that did not is left out.
    pub counts: BTreeMap<Tally, u64>,
    /// How long hooks took, one entry per agent and event, ordered by them.
    pub durations: Vec<HookDurations>,
}

/// Why counts cannot be reported.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DayCountsError {
    /// More agent and event pairs than a day's report holds.
    #[error("a day's report holds at most {max} hook durations", max = DayCounts::MAX_DURATIONS)]
    TooManyDurations,
    /// One agent and event pair given twice.
    #[error("a day's report names each agent and hook event once")]
    RepeatedDurations,
}

impl DayCounts {
    /// The most agent and event pairs one day's report holds.
    pub const MAX_DURATIONS: usize = 64;

    /// The counts in their normal form: kinds that did not happen and hooks that never ran left
    /// out, the durations ordered by agent and event.
    ///
    /// # Errors
    /// [`DayCountsError`] for more pairs than [`DayCounts::MAX_DURATIONS`], or one given twice.
    pub fn new(
        counts: BTreeMap<Tally, u64>,
        durations: Vec<HookDurations>,
    ) -> Result<Self, DayCountsError> {
        if durations.len() > Self::MAX_DURATIONS {
            return Err(DayCountsError::TooManyDurations);
        }
        let mut keyed: BTreeMap<(AgentLabel, HookEventName), DurationBuckets> = BTreeMap::new();
        for entry in durations {
            if entry.buckets.total() == 0 {
                continue;
            }
            if keyed
                .insert((entry.agent, entry.event), entry.buckets)
                .is_some()
            {
                return Err(DayCountsError::RepeatedDurations);
            }
        }
        Ok(Self {
            counts: counts.into_iter().filter(|(_, n)| *n > 0).collect(),
            durations: keyed
                .into_iter()
                .map(|((agent, event), buckets)| HookDurations {
                    agent,
                    event,
                    buckets,
                })
                .collect(),
        })
    }

    /// Whether nothing was counted.
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty() && self.durations.is_empty()
    }
}

/// What a member's machine reports for one day, as it reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayReport {
    /// The day, in UTC.
    pub day: ReportDay,
    /// What was counted.
    #[serde(flatten)]
    pub counts: DayCounts,
}

impl DayReport {
    /// How many days before the day of a heartbeat a report it carries may be about.
    pub const KEPT_DAYS_BACK: i64 = 7;

    /// Whether a heartbeat sent at `now` may report this day: one of the week before the day
    /// `now` falls on, that day, or the day after, which another time zone may already be in.
    pub fn is_recent(&self, now: OffsetDateTime) -> bool {
        let today = ReportDay::of(now);
        today.minus_days(Self::KEPT_DAYS_BACK) <= self.day && self.day <= today.next()
    }
}

/// The id a member's dibs gave the machine it runs on, once, at random: 32 lowercase hexadecimal
/// digits (128 bits). Reports are kept per machine by it, so a machine that is renamed keeps one
/// report a day and two machines of one name keep two. It is never shown to anyone.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MachineId(String);

/// Why a text is not a machine's id.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MachineIdError {
    /// The text is not 32 lowercase hexadecimal digits.
    #[error("a machine's id is 32 lowercase hexadecimal digits")]
    Malformed,
}

impl MachineId {
    /// Parses an id exactly as given.
    ///
    /// # Errors
    /// [`MachineIdError::Malformed`] when the text breaks the rule above.
    pub fn parse(value: &str) -> Result<Self, MachineIdError> {
        let hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
        if value.len() == 32 && value.chars().all(hex) {
            Ok(Self(value.to_owned()))
        } else {
            Err(MachineIdError::Malformed)
        }
    }

    /// The id these 128 bits spell.
    pub fn from_bits(bits: u128) -> Self {
        Self(format!("{bits:032x}"))
    }
}

string_value!(MachineId, MachineIdError);

/// The days one heartbeat reports, of one machine: at most [`HeartbeatReports::MAX`], each day
/// once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatReports {
    machine: MachineId,
    days: Vec<DayReport>,
}

/// Why reports cannot travel with one heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HeartbeatReportsError {
    /// More days than one heartbeat reports.
    #[error("a heartbeat reports at most {max} days", max = HeartbeatReports::MAX)]
    TooMany,
    /// One day reported twice.
    #[error("a heartbeat reports each day once")]
    RepeatedDay,
}

impl HeartbeatReports {
    /// The most days one heartbeat reports: the week a machine keeps.
    pub const MAX: usize = 7;

    /// The reports of `machine`, in the order given.
    ///
    /// # Errors
    /// [`HeartbeatReportsError`] for more than [`HeartbeatReports::MAX`] days, or one day twice.
    pub fn new(machine: MachineId, days: Vec<DayReport>) -> Result<Self, HeartbeatReportsError> {
        if days.len() > Self::MAX {
            return Err(HeartbeatReportsError::TooMany);
        }
        let distinct: std::collections::BTreeSet<ReportDay> =
            days.iter().map(|report| report.day).collect();
        if distinct.len() != days.len() {
            return Err(HeartbeatReportsError::RepeatedDay);
        }
        Ok(Self { machine, days })
    }

    /// The machine.
    pub fn machine(&self) -> &MachineId {
        &self.machine
    }

    /// The days' reports.
    pub fn days(&self) -> &[DayReport] {
        &self.days
    }
}

/// A day's report as the service keeps it: one per member, machine (by its id, whatever its name)
/// and day, a resent one replacing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookReport {
    /// The member.
    pub member: MemberId,
    /// Their machine.
    pub machine: MachineId,
    /// The machine's name, as the session that sent the report last reported it.
    pub host: HostName,
    /// The report.
    pub report: DayReport,
}

impl HookReport {
    /// Whether this report changes what is kept: nothing kept yet for its member, machine and
    /// day, or different counts.
    pub fn changes(&self, kept: Option<&DayCounts>) -> bool {
        kept != Some(&self.report.counts)
    }
}

/// Why an agent's feedback is given, as its member wrote it: one line of 1 to 280 characters after
/// trimming, line breaks and tabs folded to spaces, no other control characters. It is kept as
/// written, so it should name no path, command line or secret.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FeedbackReason(String);

/// Why a text is not a reason for feedback.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FeedbackReasonError {
    /// The trimmed text is empty, longer than 280 characters, or has control characters.
    #[error("a reason is one line of 1 to 280 characters")]
    Malformed,
}

impl FeedbackReason {
    /// The most characters a reason holds.
    pub const MAX_CHARS: usize = 280;

    /// Parses a reason: line breaks and tabs become spaces, then whitespace at both ends is
    /// trimmed.
    ///
    /// # Errors
    /// [`FeedbackReasonError::Malformed`] when the text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, FeedbackReasonError> {
        let folded: String = value
            .chars()
            .map(|c| {
                if matches!(c, '\n' | '\r' | '\t') {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        let line = folded.trim();
        if line.is_empty()
            || line.chars().count() > Self::MAX_CHARS
            || line.chars().any(char::is_control)
        {
            return Err(FeedbackReasonError::Malformed);
        }
        Ok(Self(line.to_owned()))
    }
}

string_value!(FeedbackReason, FeedbackReasonError);

/// What feedback is about. Serialised as `{"kind": "false_refusal", "refusal": "r-7f3a",
/// "tally": "held_by"}` or `{"kind": "conflict", "repo": "default", "area": "default:billing"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Feedback {
    /// A refusal of the guard the agent found wrong.
    FalseRefusal {
        /// The refusal, by the id it was shown with.
        refusal: RefusalId,
        /// Its kind, when the machine that showed it still remembers it.
        tally: Option<Tally>,
    },
    /// A conflict while rebasing that the agent resolved by hand.
    Conflict {
        /// The repository.
        repo: RepoId,
        /// The area the conflicting file lies in, when one does; the file itself is not kept.
        area: Option<QualifiedArea>,
    },
}

#[cfg(test)]
mod tests;
