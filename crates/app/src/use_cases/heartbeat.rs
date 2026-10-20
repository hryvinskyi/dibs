//! A session's sign of life.

use std::sync::Arc;

use dibs_domain::event::{BranchRecorded, EventKind, HookReported};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::radar::heartbeat_branches;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::report::{HeartbeatReports, HookReport, ReportDay};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::own_session;
use crate::use_cases::radar::{OnBranch, Radar};
use crate::work::Work;

/// What a heartbeat kept alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatOutcome {
    /// The claims whose `last_seen` moved to now.
    pub refreshed: Vec<ItemId>,
    /// The days whose report changed what was kept for the session's member and machine.
    pub reported: Vec<ReportDay>,
    /// The days whose report was kept or was already: every day reported but those outside the
    /// week a heartbeat may report.
    pub accepted: Vec<ReportDay>,
}

/// Refreshes the session and the caller's live claims taken from a session on the same machine. A
/// claim taken on another machine is kept alive by that machine's heartbeat, or by an edit check
/// here; a claim that already went stale is renewed only by claiming it again. The caller's places
/// in slot queues, and the slot holds their heartbeats keep, are seen now too, from any machine.
///
/// A heartbeat from a checkout on a branch records that branch on the claims it kept alive that its
/// own session took or last renewed: one session works in one checkout, while another session of
/// the same machine may work in another checkout on another branch. A `BranchRecorded` event marks
/// each change, and the claim's footprint of the repository, which measured the branch before,
/// goes with what it made. A repository the project does not list records nothing.
///
/// The reports it carries are kept one per member, machine (by the id its dibs gave it, whatever it
/// is called) and day: a report that differs from the one kept replaces it and appends a
/// `HookReported` event, one that does not changes nothing. A day more than a week before the
/// heartbeat, or after the day it is sent, is not kept.
pub struct Heartbeat {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl Heartbeat {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Records a heartbeat that reports nothing.
    ///
    /// # Errors
    /// As [`Heartbeat::execute_reporting`].
    pub async fn execute(
        &self,
        actor: &Actor,
        session: SessionId,
        branch: Option<&OnBranch>,
    ) -> Result<HeartbeatOutcome, AppError> {
        self.execute_reporting(actor, session, branch, None).await
    }

    /// Records the heartbeat and keeps the reports it carries.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] when the session is not the caller's; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute_reporting(
        &self,
        actor: &Actor,
        session: SessionId,
        branch: Option<&OnBranch>,
        reports: Option<&HeartbeatReports>,
    ) -> Result<HeartbeatOutcome, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let mut session = own_session(&mut work, actor, session).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let stale_after = settings.stale_after();
        session.last_seen = now;
        work.save_session(actor.project, &session).await?;
        let mut kept = Vec::new();
        for mut claim in work.claims(actor.project).await? {
            let ours = claim.member == actor.member && claim.host == session.host;
            if ours && claim.is_live(now, stale_after) {
                claim.last_seen = now;
                work.save_claim(actor.project, &claim).await?;
                kept.push(claim);
            }
        }
        if let Some(on) = branch.filter(|on| settings.repo(&on.repo).is_some()) {
            let radar = Radar {
                ids: &*self.ids,
                schemes: &self.ref_schemes,
                settings: &settings,
                now,
            };
            for claim in heartbeat_branches(&kept, session.id, &on.repo, &on.branch) {
                work.save_claim(actor.project, &claim).await?;
                work.record(
                    now,
                    EventKind::BranchRecorded(BranchRecorded {
                        item: claim.item,
                        member: actor.member,
                        repo: on.repo.clone(),
                        branch: on.branch.clone(),
                    }),
                )
                .await?;
                radar
                    .drop_footprint(&mut work, claim.item, &on.repo)
                    .await?;
            }
        }
        let mut board = work.slot_board(actor.project).await?;
        if board.refresh_member(actor.member, now) {
            work.put_slot_board(actor.project, &board).await?;
        }
        let mut reported = Vec::new();
        let mut accepted = Vec::new();
        let (machine, days) = match reports {
            Some(reports) => (Some(reports.machine()), reports.days()),
            None => (None, &[][..]),
        };
        for (machine, day) in days
            .iter()
            .filter(|day| day.is_recent(now))
            .filter_map(|day| Some((machine?, day)))
        {
            accepted.push(day.day);
            let report = HookReport {
                member: actor.member,
                machine: machine.clone(),
                host: session.host.clone(),
                report: day.clone(),
            };
            let kept = work
                .hook_report(actor.project, actor.member, machine, day.day)
                .await?;
            if !report.changes(kept.as_ref()) {
                continue;
            }
            work.save_hook_report(actor.project, &report).await?;
            work.record(
                now,
                EventKind::HookReported(HookReported {
                    member: actor.member,
                    host: session.host.clone(),
                    day: day.day,
                    counts: day.counts.clone(),
                }),
            )
            .await?;
            reported.push(day.day);
        }
        work.commit(&*self.notifier).await?;
        Ok(HeartbeatOutcome {
            refreshed: kept.iter().map(|claim| claim.item).collect(),
            reported,
            accepted,
        })
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::use_cases::testing::{Env, session};
    use crate::use_cases::{ClaimItem, ClaimItemInput};

    async fn claim(env: &Env, item: ItemId, session: SessionId) {
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    wide: None,
                    item,
                    session,
                    areas: Vec::new(),
                    kind: None,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_heartbeat_refreshes_the_live_claims_of_its_machine_only() {
        let env = Env::new().await;
        let here = env.item("gh:acme/widgets#1").await;
        let elsewhere = env.item("gh:acme/widgets#2").await;
        let gone_stale = env.item("gh:acme/widgets#3").await;
        let other_machine = session(&env.deps, &env.bob.actor, "laptop-2", "laptop-2/aa").await;
        claim(&env, gone_stale, env.bob.session).await;
        env.advance(Duration::hours(25));
        claim(&env, here, env.bob.session).await;
        claim(&env, elsewhere, other_machine).await;
        let events = env.events().await.len();
        env.advance(Duration::minutes(10));
        let outcome = Heartbeat::new(&env.deps)
            .execute(&env.bob.actor, env.bob.session, None)
            .await
            .unwrap();
        assert_eq!(outcome.refreshed, [here]);
        let mut tx = env.deps.store.begin().await.unwrap();
        let now = env.fakes.clock.as_ref();
        let refreshed = tx.claim(env.project, here).await.unwrap().unwrap();
        assert_eq!(refreshed.last_seen, crate::ports::Clock::now(now));
        let untouched = tx.claim(env.project, elsewhere).await.unwrap().unwrap();
        assert!(untouched.last_seen < refreshed.last_seen);
        drop(tx);
        assert_eq!(env.events().await.len(), events);
    }

    #[tokio::test]
    async fn a_heartbeat_from_a_checkout_on_a_branch_records_it_on_the_claim_once() {
        use dibs_domain::radar::BranchName;
        use dibs_domain::repo::RepoId;

        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, item, env.bob.session).await;
        let on = |repo: &str, branch: &str| OnBranch {
            repo: RepoId::parse(repo).unwrap(),
            branch: BranchName::parse(branch).unwrap(),
        };
        let before = env.events().await.len();
        let heartbeat = Heartbeat::new(&env.deps);
        for branch in [
            Some(on("default", "bob/1-x")),
            Some(on("default", "bob/1-x")),
            Some(on("elsewhere", "bob/1-y")),
            None,
        ] {
            heartbeat
                .execute(&env.bob.actor, env.bob.session, branch.as_ref())
                .await
                .unwrap();
        }
        let kinds = env.kinds_after(before).await;
        assert!(
            matches!(kinds.as_slice(), [EventKind::BranchRecorded(recorded)]
                if recorded.item == item && recorded.branch.as_str() == "bob/1-x"),
            "once, and never for a repository the project does not list: {kinds:?}"
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        assert_eq!(
            stored.branch(&RepoId::implicit()).map(BranchName::as_str),
            Some("bob/1-x")
        );
    }

    #[tokio::test]
    async fn a_report_is_kept_per_member_machine_and_day_and_a_resend_replaces_it() {
        use std::collections::BTreeMap;

        use dibs_domain::report::{DayCounts, DayReport, MachineId, Tally};

        let env = Env::new().await;
        env.advance(Duration::days(30));
        let today = ReportDay::of(env.now());
        let day_of = |day: ReportDay, held: u64| DayReport {
            day,
            counts: DayCounts::new(BTreeMap::from([(Tally::HeldBy, held)]), Vec::new()).unwrap(),
        };
        let laptop = MachineId::from_bits(1);
        let other = MachineId::from_bits(2);
        let beat = |session: SessionId, machine: &MachineId, reports: Vec<DayReport>| {
            let reports = HeartbeatReports::new(machine.clone(), reports).unwrap();
            let env = &env;
            async move {
                Heartbeat::new(&env.deps)
                    .execute_reporting(&env.bob.actor, session, None, Some(&reports))
                    .await
                    .unwrap()
            }
        };
        let before = env.events().await.len();
        let first = beat(
            env.bob.session,
            &laptop,
            vec![day_of(today, 1), day_of(today.minus_days(30), 4)],
        )
        .await;
        assert_eq!(first.reported, [today]);
        assert_eq!(
            first.accepted,
            [today],
            "a day older than a week is not kept"
        );
        let again = beat(env.bob.session, &laptop, vec![day_of(today, 1)]).await;
        assert!(again.reported.is_empty(), "unchanged");
        assert_eq!(again.accepted, [today], "accepted all the same");
        assert_eq!(
            beat(env.bob.session, &laptop, vec![day_of(today, 3)])
                .await
                .reported,
            [today]
        );
        let renamed = session(&env.deps, &env.bob.actor, "desktop-renamed", "renamed/0a").await;
        beat(renamed, &laptop, vec![day_of(today, 4)]).await;
        let twin = session(&env.deps, &env.bob.actor, "desktop", "desktop/twin").await;
        beat(twin, &other, vec![day_of(today, 5)]).await;
        let reported: Vec<(String, u64)> = env
            .kinds_after(before)
            .await
            .into_iter()
            .filter_map(|kind| match kind {
                EventKind::HookReported(report) => Some((
                    report.host.to_string(),
                    report.counts.counts[&Tally::HeldBy],
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            reported,
            [
                ("desktop".to_owned(), 1),
                ("desktop".to_owned(), 3),
                ("desktop-renamed".to_owned(), 4),
                ("desktop".to_owned(), 5)
            ]
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        let kept = tx.hook_reports(env.project, today, today).await.unwrap();
        let rows: Vec<(String, String, u64)> = kept
            .iter()
            .map(|report| {
                (
                    report.machine.to_string(),
                    report.host.to_string(),
                    report.report.counts.counts[&Tally::HeldBy],
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (laptop.to_string(), "desktop-renamed".to_owned(), 4),
                (other.to_string(), "desktop".to_owned(), 5)
            ],
            "a renamed machine keeps one row, two machines of one name keep two"
        );
    }

    #[tokio::test]
    async fn a_heartbeat_names_a_session_of_the_caller() {
        let env = Env::new().await;
        let error = Heartbeat::new(&env.deps)
            .execute(&env.bob.actor, env.alice.session, None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
    }
}
