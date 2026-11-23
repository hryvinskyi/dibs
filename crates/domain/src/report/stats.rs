//! The figures a project's pilot is judged by, read from its event log and from the reports its
//! members' machines sent: one pure projection, so the same log and reports always give the same
//! figures.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::event::{Event, EventKind, FeedbackGiven, GrantEnd, Granted, PrStateReported};
use crate::grant::GrantDecision;
use crate::ids::MemberId;
use crate::report::{DurationBuckets, Feedback, FeedbackReason, HookEventName, HookReport};
use crate::report::{ReportDay, Tally};
use crate::review::PrRef;
use crate::session::AgentLabel;
use crate::slot::SlotName;

/// The days a reading covers, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsRange {
    from: ReportDay,
    to: ReportDay,
}

/// Why days cannot be read together.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StatsRangeError {
    /// The last day comes before the first.
    #[error("the range ends before it starts")]
    Backwards,
    /// More days than one reading covers.
    #[error("a range covers at most {max} days", max = StatsRange::MAX_DAYS)]
    TooLong,
}

impl StatsRange {
    /// The most days one reading covers.
    pub const MAX_DAYS: i64 = 92;

    /// The days from `from` to `to`, both included.
    ///
    /// # Errors
    /// [`StatsRangeError`] when `to` comes before `from` or the range is longer than
    /// [`StatsRange::MAX_DAYS`].
    pub fn new(from: ReportDay, to: ReportDay) -> Result<Self, StatsRangeError> {
        if to < from {
            return Err(StatsRangeError::Backwards);
        }
        if (to.date() - from.date()).whole_days() >= Self::MAX_DAYS {
            return Err(StatsRangeError::TooLong);
        }
        Ok(Self { from, to })
    }

    /// The week that ends on `to`: the seven days up to it.
    pub fn week_to(to: ReportDay) -> Self {
        Self {
            from: to.minus_days(6),
            to,
        }
    }

    /// The first day.
    pub const fn from(&self) -> ReportDay {
        self.from
    }

    /// The last day.
    pub const fn to(&self) -> ReportDay {
        self.to
    }

    /// When the range starts: the first day's midnight, in UTC.
    pub fn starts_at(&self) -> OffsetDateTime {
        self.from.start()
    }

    /// When it ends: the midnight after the last day, which is no longer in it.
    pub fn ends_at(&self) -> OffsetDateTime {
        self.to.next().start()
    }

    /// Whether an event at `at` falls in it.
    pub fn holds(&self, at: OffsetDateTime) -> bool {
        self.starts_at() <= at && at < self.ends_at()
    }
}

/// How long something waited, over the waits that ended: how many, the median and the longest.
/// The median of an even count is the mean of the two middle waits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Waits {
    /// How many waits ended.
    pub count: u64,
    /// The median wait; `None` with no wait.
    pub median: Option<Duration>,
    /// The longest wait; `None` with no wait.
    pub max: Option<Duration>,
}

/// One member's reported counts of one day, every machine of theirs added up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDay {
    /// The member.
    pub member: MemberId,
    /// The day.
    pub day: ReportDay,
    /// How often each kind happened.
    pub counts: BTreeMap<Tally, u64>,
}

/// How long one hook event of one agent took, over the runs counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationFigure {
    /// The member whose machines ran it; `None` for every member together.
    pub member: Option<MemberId>,
    /// The agent's label.
    pub agent: AgentLabel,
    /// The hook event.
    pub event: HookEventName,
    /// The runs, by how long they took.
    pub buckets: DurationBuckets,
}

impl DurationFigure {
    /// How many runs were counted.
    pub fn runs(&self) -> u64 {
        self.buckets.total()
    }

    /// The upper bound of the bucket the 95th percentile falls in, in milliseconds; `None` when it
    /// lies beyond the last bound.
    pub fn p95_ms(&self) -> Option<u64> {
        self.buckets.p95_bound_ms()
    }
}

/// One piece of feedback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackEntry {
    /// When it was given.
    pub at: OffsetDateTime,
    /// The member whose agent gave it.
    pub member: MemberId,
    /// What it is about.
    pub about: Feedback,
    /// Why, as its member wrote it.
    pub reason: FeedbackReason,
}

/// The figures of a pilot over a range of days.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PilotStats {
    /// Claims made.
    pub claims: u64,
    /// Stale claims taken over.
    pub takeovers: u64,
    /// Slot holds whose run stopped sending heartbeats.
    pub stalls: u64,
    /// Disputes sent to the arbiters.
    pub escalations: u64,
    /// The disputes among them that dibs raised on its own, nobody having asked.
    pub escalations_by_dibs: u64,
    /// Waits in slot queues: from joining a queue to taking the slot asked for.
    pub slot_waits: Waits,
    /// Grants given, by who decided; a decision that never happened is not a key.
    pub grants: BTreeMap<GrantDecision, u64>,
    /// Wide claims' announcements acknowledged, by who decided; a decision that never happened is
    /// not a key.
    pub acknowledgements: BTreeMap<GrantDecision, u64>,
    /// Grants their grantor or an admin revoked; a grant that ended with a claim is not counted.
    pub revoked: u64,
    /// How long requests waited for the grant that answered them, from the request to the grant, by
    /// who decided. Only a grant that records when it was asked, and whose oldest answered request
    /// was asked inside the range, is counted; a decision with no such grant is not a key.
    pub grant_waits: BTreeMap<GrantDecision, Waits>,
    /// The orders stored for colliding claims, by the code of the rule that decided, or `tied`; a
    /// code that never decided is not a key.
    pub orders: BTreeMap<&'static str, u64>,
    /// The orders among them that replaced one stored before for the same collision.
    pub orders_changed: u64,
    /// The reported counts of every member and day added up.
    pub reported: BTreeMap<Tally, u64>,
    /// The reported counts per member and day, ordered by member and day.
    pub member_days: Vec<MemberDay>,
    /// Hook durations per agent and event, every member together, ordered by agent and event.
    pub durations: Vec<DurationFigure>,
    /// Hook durations per member, agent and event, ordered by them.
    pub member_durations: Vec<DurationFigure>,
    /// Refusals agents marked as wrong.
    pub false_refusals: u64,
    /// Conflicts agents resolved by hand while rebasing.
    pub conflicts: u64,
    /// The feedback given, newest first, at most [`PilotStats::MAX_FEEDBACK`] of it.
    pub feedback: Vec<FeedbackEntry>,
    /// How often an integration reported a pull request that does not merge cleanly.
    pub conflicting_reports: u64,
    /// The pull requests reported so.
    pub conflicting_prs: BTreeSet<PrRef>,
    /// How long conflicts of pull requests lasted: from a report that one does not merge cleanly to
    /// the next report that it does. A conflict reported before the range, or not over by its end,
    /// is not counted.
    pub conflicts_resolved: Waits,
}

/// A slot wait under way: who waits for which slot.
type WaitKey = (MemberId, SlotName);

impl PilotStats {
    /// The most pieces of feedback one reading lists.
    pub const MAX_FEEDBACK: usize = 500;

    /// The names of the event kinds the figures read; the others change nothing.
    pub const EVENT_NAMES: [&'static str; 16] = [
        "claimed",
        "taken_over",
        "slot_stalled",
        "escalated",
        "slot_queued",
        "slot_taken",
        "slot_left",
        "slot_offer_lapsed",
        "slot_force_released",
        "feedback_given",
        "pr_state_reported",
        "slot_stall_taken_over",
        "granted",
        "wide_granted_by_ack",
        "grant_revoked",
        "overlap_ordered",
    ];

    /// The figures of `events` that fall in `range`, oldest first, and of the `reports` of its
    /// days, as [`StatsReading`] reads them.
    pub fn of(range: &StatsRange, events: &[Event], reports: &[HookReport]) -> Self {
        let mut reading = StatsReading::new(*range);
        reading.events(events);
        reading.finish(reports)
    }
}

/// The figures of a range read a page of events at a time, oldest first, so a long range is never
/// held whole: what stays between pages is the figures, the waits and conflicts under way, the
/// waits that ended and the feedback kept. A slot wait or a conflict counts when it ends in the
/// range; one that began before it is not counted.
#[derive(Debug)]
pub struct StatsReading {
    range: StatsRange,
    stats: PilotStats,
    waiting: BTreeMap<WaitKey, OffsetDateTime>,
    waits: Vec<Duration>,
    grant_waits: BTreeMap<GrantDecision, Vec<Duration>>,
    conflicting: BTreeMap<PrRef, OffsetDateTime>,
    resolved: Vec<Duration>,
    feedback: VecDeque<FeedbackEntry>,
}

impl StatsReading {
    /// A reading of `range` that has read nothing yet.
    pub fn new(range: StatsRange) -> Self {
        Self {
            range,
            stats: PilotStats::default(),
            waiting: BTreeMap::new(),
            waits: Vec::new(),
            grant_waits: BTreeMap::new(),
            conflicting: BTreeMap::new(),
            resolved: Vec::new(),
            feedback: VecDeque::new(),
        }
    }

    /// Reads the next events of the log, oldest first; those outside the range change nothing.
    pub fn events(&mut self, events: &[Event]) {
        let range = self.range;
        for event in events.iter().filter(|event| range.holds(event.at)) {
            self.read(event);
        }
    }

    /// The figures, with the `reports` of the range's days.
    pub fn finish(mut self, reports: &[HookReport]) -> PilotStats {
        self.stats.slot_waits = waits_of(self.waits);
        self.stats.grant_waits = self
            .grant_waits
            .into_iter()
            .map(|(decided, waits)| (decided, waits_of(waits)))
            .collect();
        self.stats.conflicts_resolved = waits_of(self.resolved);
        self.stats.feedback = self.feedback.into_iter().rev().collect();
        self.stats.read_reports(&self.range, reports);
        self.stats
    }

    fn read(&mut self, event: &Event) {
        let stats = &mut self.stats;
        let waiting = &mut self.waiting;
        let waits = &mut self.waits;
        match &event.kind {
            EventKind::Claimed(_) => stats.claims += 1,
            EventKind::TakenOver(_) => stats.takeovers += 1,
            EventKind::SlotStalled(_) => stats.stalls += 1,
            EventKind::Escalated(escalated) => {
                stats.escalations += 1;
                if escalated.by_dibs {
                    stats.escalations_by_dibs += 1;
                }
            }
            EventKind::SlotQueued(queued) => {
                waiting
                    .entry((queued.member, queued.slot.clone()))
                    .or_insert(event.at);
            }
            EventKind::SlotTaken(taken) => {
                if let Some(since) = waiting.remove(&(taken.member, taken.slot.clone())) {
                    waits.push(event.at - since);
                }
            }
            EventKind::SlotStallTakenOver(taken) => {
                if let Some(since) = waiting.remove(&(taken.member, taken.slot.clone())) {
                    waits.push(event.at - since);
                }
            }
            EventKind::SlotLeft(left) | EventKind::SlotOfferLapsed(left) => {
                waiting.remove(&(left.member, left.slot.clone()));
            }
            EventKind::SlotForceReleased(forced) => {
                waiting.remove(&(forced.member, forced.slot.clone()));
            }
            EventKind::Granted(granted) => self.granted(event.at, granted),
            EventKind::WideGrantedByAck(acknowledged) => {
                *stats
                    .acknowledgements
                    .entry(acknowledged.decided)
                    .or_default() += 1;
            }
            EventKind::GrantRevoked(ended) => {
                if ended.cause == GrantEnd::Revoked {
                    stats.revoked += 1;
                }
            }
            EventKind::OverlapOrdered(ordered) => {
                *stats.orders.entry(ordered.order.code()).or_default() += 1;
                if ordered.changed {
                    stats.orders_changed += 1;
                }
            }
            EventKind::FeedbackGiven(given) => self.feedback_given(event.at, given),
            EventKind::PrStateReported(reported) => self.pr_reported(event.at, reported),
            _ => {}
        }
    }

    fn feedback_given(&mut self, at: OffsetDateTime, given: &FeedbackGiven) {
        match given.about {
            Feedback::FalseRefusal { .. } => self.stats.false_refusals += 1,
            Feedback::Conflict { .. } => self.stats.conflicts += 1,
        }
        self.feedback.push_back(FeedbackEntry {
            at,
            member: given.member,
            about: given.about.clone(),
            reason: given.reason.clone(),
        });
        if self.feedback.len() > PilotStats::MAX_FEEDBACK {
            self.feedback.pop_front();
        }
    }

    /// A grant counts under its decision; it is timed from the oldest request it answered only when
    /// that request was asked inside the range, as a slot wait or a conflict is.
    fn granted(&mut self, at: OffsetDateTime, granted: &Granted) {
        *self.stats.grants.entry(granted.decided).or_default() += 1;
        let began_in_range = granted
            .asked_at
            .filter(|asked_at| *asked_at >= self.range.starts_at());
        if let Some(asked_at) = began_in_range {
            self.grant_waits
                .entry(granted.decided)
                .or_default()
                .push(at - asked_at);
        }
    }

    /// A report that a pull request does not merge cleanly starts its conflict, unless one is under
    /// way already; the next report that it does ends it.
    fn pr_reported(&mut self, at: OffsetDateTime, reported: &PrStateReported) {
        if reported.mergeable {
            if let Some(since) = self.conflicting.remove(&reported.pr) {
                self.resolved.push(at - since);
            }
            return;
        }
        self.stats.conflicting_reports += 1;
        self.stats.conflicting_prs.insert(reported.pr.clone());
        self.conflicting.entry(reported.pr.clone()).or_insert(at);
    }
}

impl PilotStats {
    fn read_reports(&mut self, range: &StatsRange, reports: &[HookReport]) {
        let mut days: BTreeMap<(MemberId, ReportDay), BTreeMap<Tally, u64>> = BTreeMap::new();
        let mut all: BTreeMap<(AgentLabel, HookEventName), DurationBuckets> = BTreeMap::new();
        let mut each: BTreeMap<(MemberId, AgentLabel, HookEventName), DurationBuckets> =
            BTreeMap::new();
        let in_range = |report: &&HookReport| {
            range.from() <= report.report.day && report.report.day <= range.to()
        };
        for report in reports.iter().filter(in_range) {
            let day = days.entry((report.member, report.report.day)).or_default();
            for (tally, count) in &report.report.counts.counts {
                add(day.entry(*tally).or_default(), *count);
                add(self.reported.entry(*tally).or_default(), *count);
            }
            for entry in &report.report.counts.durations {
                all.entry((entry.agent.clone(), entry.event.clone()))
                    .or_default()
                    .add(&entry.buckets);
                each.entry((report.member, entry.agent.clone(), entry.event.clone()))
                    .or_default()
                    .add(&entry.buckets);
            }
        }
        self.member_days = days
            .into_iter()
            .filter(|(_, counts)| !counts.is_empty())
            .map(|((member, day), counts)| MemberDay {
                member,
                day,
                counts,
            })
            .collect();
        self.durations = all
            .into_iter()
            .map(|((agent, event), buckets)| DurationFigure {
                member: None,
                agent,
                event,
                buckets,
            })
            .collect();
        self.member_durations = each
            .into_iter()
            .map(|((member, agent, event), buckets)| DurationFigure {
                member: Some(member),
                agent,
                event,
                buckets,
            })
            .collect();
    }
}

fn add(total: &mut u64, more: u64) {
    *total = total.saturating_add(more);
}

/// The count, median and longest of the waits.
fn waits_of(mut waits: Vec<Duration>) -> Waits {
    waits.sort();
    let count = u64::try_from(waits.len()).unwrap_or(u64::MAX);
    let median = match waits.len() {
        0 => None,
        n if n % 2 == 1 => waits.get(n / 2).copied(),
        n => match (waits.get(n / 2 - 1), waits.get(n / 2)) {
            (Some(lower), Some(upper)) => Some((*lower + *upper) / 2),
            _ => None,
        },
    };
    Waits {
        count,
        median,
        max: waits.last().copied(),
    }
}

#[cfg(test)]
mod tests;
