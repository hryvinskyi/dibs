//! The figures of a project's pilot over a range of days, for its admins.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dibs_domain::handle::Handle;
use dibs_domain::ids::MemberId;
use dibs_domain::report::stats::{PilotStats, StatsRange, StatsReading};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::Names;
use crate::ports::Store;
use crate::work::Work;

/// The most events one reading goes through; past them it says it read only part of the range.
pub const MAX_STATS_EVENTS: usize = 100_000;

/// How many events are read from the store at a time.
const STATS_PAGE: usize = 5_000;

/// The figures, with the handle of every member they name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsView {
    /// The days read.
    pub range: StatsRange,
    /// The figures.
    pub stats: PilotStats,
    /// Whether the range held more events than one reading goes through, so the figures read from
    /// the log cover only its first [`MAX_STATS_EVENTS`].
    pub truncated: bool,
    /// The handle of every member the figures name, by id.
    pub members: BTreeMap<MemberId, Handle>,
}

/// Reads the figures of a range of days: claims, takeovers, stalled slot holds, escalations and slot
/// waits from the event log, refusals, failures and hook durations from the reports members'
/// machines sent, the feedback agents gave and the pull requests integrations reported as not
/// merging cleanly. It writes nothing, and reads in a transaction for reading, page by page, so it
/// never holds up a heartbeat or an edit check.
pub struct ReadStats {
    store: Arc<dyn Store>,
}

impl ReadStats {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The figures of `range`.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for anyone but an admin; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, range: StatsRange) -> Result<StatsView, AppError> {
        actor.require_admin()?;
        let mut work = Work::begin_reading_for(&*self.store, actor).await?;
        let mut reading = StatsReading::new(range);
        let mut read = 0;
        let mut after = None;
        let mut truncated = false;
        loop {
            let page = work
                .events_of_kinds(
                    actor.project,
                    &PilotStats::EVENT_NAMES,
                    (range.starts_at(), range.ends_at()),
                    after,
                    STATS_PAGE,
                )
                .await?;
            let room = MAX_STATS_EVENTS - read;
            if page.len() > room {
                reading.events(page.get(..room).unwrap_or_default());
                truncated = true;
                break;
            }
            reading.events(&page);
            read += page.len();
            after = page.last().map(|event| event.id);
            if page.len() < STATS_PAGE {
                break;
            }
            if read == MAX_STATS_EVENTS {
                let more = work
                    .events_of_kinds(
                        actor.project,
                        &PilotStats::EVENT_NAMES,
                        (range.starts_at(), range.ends_at()),
                        after,
                        1,
                    )
                    .await?;
                truncated = !more.is_empty();
                break;
            }
        }
        let reports = work
            .hook_reports(actor.project, range.from(), range.to())
            .await?;
        let names = Names::load(&mut work).await?;
        let stats = reading.finish(&reports);
        let named: BTreeSet<MemberId> = stats
            .member_days
            .iter()
            .map(|day| day.member)
            .chain(stats.member_durations.iter().filter_map(|d| d.member))
            .chain(stats.feedback.iter().map(|entry| entry.member))
            .collect();
        let members = named
            .into_iter()
            .map(|member| Ok((member, names.handle(member)?)))
            .collect::<Result<_, AppError>>()?;
        Ok(StatsView {
            range,
            stats,
            truncated,
            members,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dibs_domain::report::{DayCounts, DayReport, HeartbeatReports, ReportDay, Tally};
    use time::Duration;

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        ClaimItem, ClaimItemInput, FeedbackAbout, FeedbackInput, GiveFeedback, Heartbeat,
        ReportPrState,
    };

    fn report(day: ReportDay, pairs: &[(Tally, u64)]) -> HeartbeatReports {
        HeartbeatReports::new(
            dibs_domain::report::MachineId::from_bits(1),
            vec![DayReport {
                day,
                counts: DayCounts::new(pairs.iter().copied().collect(), Vec::new()).unwrap(),
            }],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn an_admin_reads_the_figures_of_the_log_and_the_reports_of_the_range() {
        let env = Env::new().await;
        env.advance(Duration::days(10));
        let today = ReportDay::of(env.now());
        let item = env.item("gh:acme/widgets#1").await;
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    item,
                    session: env.bob.session,
                    areas: Vec::new(),
                    wide: None,
                    kind: None,
                },
            )
            .await
            .unwrap();
        Heartbeat::new(&env.deps)
            .execute_reporting(
                &env.bob.actor,
                env.bob.session,
                None,
                Some(&report(
                    today,
                    &[(Tally::HeldBy, 2), (Tally::SwitchedOff, 1)],
                )),
            )
            .await
            .unwrap();
        GiveFeedback::new(&env.deps)
            .execute(
                &env.bob.actor,
                FeedbackInput {
                    about: FeedbackAbout::FalseRefusal {
                        refusal: dibs_domain::report::RefusalId::parse("r-00ff").unwrap(),
                        tally: None,
                    },
                    reason: dibs_domain::report::FeedbackReason::parse("it was free").unwrap(),
                },
            )
            .await
            .unwrap();
        ReportPrState::new(&env.deps)
            .execute(
                &env.importer,
                dibs_domain::review::PrRef::parse("acme/widgets#4").unwrap(),
                false,
            )
            .await
            .unwrap();
        let read = ReadStats::new(&env.deps);
        let view = read
            .execute(&env.alice.actor, StatsRange::week_to(today))
            .await
            .unwrap();
        assert_eq!(view.stats.claims, 1);
        assert_eq!(
            view.stats.reported,
            BTreeMap::from([(Tally::HeldBy, 2), (Tally::SwitchedOff, 1)])
        );
        assert_eq!(view.stats.false_refusals, 1);
        assert_eq!(view.stats.conflicting_reports, 1);
        assert!(!view.truncated);
        assert_eq!(
            view.members.get(&env.bob.actor.member).map(Handle::as_str),
            Some("bob")
        );
        let earlier = read
            .execute(&env.alice.actor, StatsRange::week_to(today.minus_days(8)))
            .await
            .unwrap();
        assert_eq!(earlier.stats, PilotStats::default());
        for refused in [&env.bob.actor, &env.importer] {
            let error = read
                .execute(refused, StatsRange::week_to(today))
                .await
                .unwrap_err();
            assert_eq!(error.code(), "forbidden");
        }
    }

    #[tokio::test]
    async fn a_range_of_more_events_than_one_reading_goes_through_says_so() {
        use dibs_domain::event::{Claimed, EventKind};

        let env = Env::new().await;
        env.advance(Duration::days(10));
        let now = env.now();
        let mut tx = env.deps.store.begin().await.unwrap();
        for n in 0..=u64::try_from(MAX_STATS_EVENTS).unwrap() {
            tx.append_event(
                env.project,
                now,
                &EventKind::Claimed(Claimed {
                    item: dibs_domain::ids::ItemId(n + 1),
                    member: env.bob.actor.member,
                    session: env.bob.session,
                    host: dibs_domain::session::HostName::parse("desktop").unwrap(),
                    areas: std::collections::BTreeSet::new(),
                }),
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        let view = ReadStats::new(&env.deps)
            .execute(&env.alice.actor, StatsRange::week_to(ReportDay::of(now)))
            .await
            .unwrap();
        assert!(view.truncated);
        assert_eq!(view.stats.claims, u64::try_from(MAX_STATS_EVENTS).unwrap());
    }
}
