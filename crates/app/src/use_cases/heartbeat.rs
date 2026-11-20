//! A session's sign of life.

use std::sync::Arc;

use dibs_domain::event::{EventKind, HookReported};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::radar::BranchSource;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::report::{HeartbeatReports, HookReport, ReportDay};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::own_session;
use crate::use_cases::radar::{FromCheckout, Radar, fed_by_checkout, report_checkout};
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
/// A heartbeat that names its checkout stores that its session reported working there now,
/// whether or not the checkout is on a branch. When the checkout is on a branch of a repository the
/// project lists, that branch, learned from the checkout, is recorded on one claim at most: the
/// one live claim its session took or last renewed and works on, and only while the session
/// reported no other checkout and no other session of the member reported this one, within the
/// project's `stale_after`. A branch the claim learned from a stronger source that still holds is
/// kept. A `BranchRecorded` event marks a change of branch, and the claim's footprint of the
/// repository, which measured the branch before, goes with what it made; a change of source alone
/// is stored and nothing more. A heartbeat that names no checkout stores none and records no
/// branch.
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
        from: Option<&FromCheckout>,
    ) -> Result<HeartbeatOutcome, AppError> {
        self.execute_reporting(actor, session, from, None).await
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
        from: Option<&FromCheckout>,
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
        if let Some(from) = from {
            let reported =
                report_checkout(&mut work, actor.member, session.id, &from.token, now).await?;
            let on = from
                .branch
                .as_ref()
                .filter(|on| settings.repo(&on.repo).is_some());
            if let Some(on) = on
                && let Some(claim) =
                    fed_by_checkout(&mut work, &reported, &kept, stale_after, |_| true).await?
            {
                let radar = Radar {
                    ids: &*self.ids,
                    schemes: &self.ref_schemes,
                    settings: &settings,
                    now,
                };
                radar
                    .record_branch(
                        &mut work,
                        claim,
                        &on.repo,
                        &on.branch,
                        BranchSource::Checkout,
                    )
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

    use dibs_domain::radar::{BranchName, BranchSource, BranchSourceKind, RecordedBranch};
    use dibs_domain::repo::RepoId;

    use super::*;
    use crate::use_cases::testing::{Env, START, checkout, in_checkout, session};
    use crate::use_cases::{ClaimItem, ClaimItemInput, OnBranch};

    async fn claim(env: &Env, item: ItemId, session: SessionId) {
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    branches: Default::default(),
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

    async fn beat(env: &Env, session: SessionId, from: Option<FromCheckout>) {
        Heartbeat::new(&env.deps)
            .execute(&env.bob.actor, session, from.as_ref())
            .await
            .unwrap();
    }

    /// The branch `item`'s claim records in the project's one repository, and its source.
    async fn recorded(env: &Env, item: ItemId) -> Option<(String, BranchSource)> {
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        let recorded = stored.branches.get(&RepoId::implicit())?;
        Some((recorded.branch.to_string(), recorded.source.clone()))
    }

    /// The branches the log recorded after `before`, with their items and sources.
    async fn recordings(env: &Env, before: usize) -> Vec<(ItemId, String, BranchSourceKind)> {
        env.kinds_after(before)
            .await
            .into_iter()
            .filter_map(|kind| match kind {
                EventKind::BranchRecorded(recorded) => {
                    Some((recorded.item, recorded.branch.to_string(), recorded.source))
                }
                _ => None,
            })
            .collect()
    }

    fn from_checkout(name: &str) -> Option<(String, BranchSource)> {
        Some((name.to_owned(), BranchSource::Checkout))
    }

    #[tokio::test]
    async fn a_heartbeat_from_a_checkout_on_a_branch_records_it_on_the_claim_once() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, item, env.bob.session).await;
        let unlisted = FromCheckout {
            token: checkout('a'),
            branch: Some(OnBranch {
                repo: RepoId::parse("elsewhere").unwrap(),
                branch: BranchName::parse("bob/1-y").unwrap(),
            }),
        };
        let before = env.events().await.len();
        for from in [
            in_checkout('a', Some("bob/1-x")),
            in_checkout('a', Some("bob/1-x")),
            unlisted,
            in_checkout('a', None),
        ] {
            beat(&env, env.bob.session, Some(from)).await;
        }
        assert_eq!(
            recordings(&env, before).await,
            [(item, "bob/1-x".to_owned(), BranchSourceKind::Checkout)],
            "once, and never for a repository the project does not list"
        );
        assert_eq!(recorded(&env, item).await, from_checkout("bob/1-x"));
    }

    #[tokio::test]
    async fn a_new_branch_of_the_checkout_replaces_the_old_one_and_drops_its_footprint() {
        use dibs_domain::radar::{CommitSha, FootprintBase};

        use crate::use_cases::{FootprintInput, RecordFootprint};

        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, item, env.bob.session).await;
        beat(
            &env,
            env.bob.session,
            Some(in_checkout('a', Some("bob/1-x"))),
        )
        .await;
        RecordFootprint::new(&env.deps)
            .execute(
                &env.importer,
                FootprintInput {
                    item,
                    repo: "default".to_owned(),
                    branch: BranchName::parse("bob/1-x").unwrap(),
                    base: FootprintBase {
                        branch: BranchName::parse("main").unwrap(),
                        sha: CommitSha::parse("abcd").unwrap(),
                        at: env.now(),
                    },
                    paths: [dibs_domain::paths::RelPath::parse("a.rs").unwrap()].into(),
                    truncated: false,
                },
            )
            .await
            .unwrap();
        let footprints = |env: &Env| {
            let store = env.deps.store.clone();
            let project = env.project;
            async move {
                let mut tx = store.begin().await.unwrap();
                tx.footprints(project).await.unwrap().len()
            }
        };
        assert_eq!(footprints(&env).await, 1);
        let before = env.events().await.len();
        beat(
            &env,
            env.bob.session,
            Some(in_checkout('a', Some("bob/1-y"))),
        )
        .await;
        assert_eq!(
            recordings(&env, before).await,
            [(item, "bob/1-y".to_owned(), BranchSourceKind::Checkout)]
        );
        assert_eq!(recorded(&env, item).await, from_checkout("bob/1-y"));
        assert_eq!(footprints(&env).await, 0, "it measured the branch before");
    }

    #[tokio::test]
    async fn two_sessions_of_a_member_in_one_checkout_record_no_branch_until_one_ends() {
        use crate::use_cases::EndSession;

        let env = Env::new().await;
        let first = env.item("gh:acme/widgets#1").await;
        let second = env.item("gh:acme/widgets#2").await;
        let other = session(&env.deps, &env.bob.actor, "desktop", "desktop/second").await;
        claim(&env, first, env.bob.session).await;
        claim(&env, second, other).await;
        let before = env.events().await.len();
        beat(&env, other, Some(in_checkout('a', None))).await;
        for _ in 0..3 {
            beat(&env, env.bob.session, Some(in_checkout('a', Some("bob/1")))).await;
            beat(&env, other, Some(in_checkout('a', Some("bob/2")))).await;
        }
        assert_eq!(
            (recorded(&env, first).await, recorded(&env, second).await),
            (None, None),
            "a session on a detached head shares the checkout all the same"
        );
        assert!(recordings(&env, before).await.is_empty());
        EndSession::new(&env.deps)
            .execute(&env.bob.actor, other)
            .await
            .unwrap();
        beat(&env, env.bob.session, Some(in_checkout('a', Some("bob/1")))).await;
        assert_eq!(recorded(&env, first).await, from_checkout("bob/1"));
        assert_eq!(recorded(&env, second).await, None);
    }

    #[tokio::test]
    async fn a_heartbeat_that_names_no_checkout_stores_none_and_records_nothing() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, item, env.bob.session).await;
        let before = env.events().await.len();
        beat(&env, env.bob.session, None).await;
        assert!(recordings(&env, before).await.is_empty());
        assert_eq!(recorded(&env, item).await, None);
        let mut tx = env.deps.store.begin().await.unwrap();
        let member = env.bob.actor.member;
        let stored = tx.session_checkouts(env.project, member, START).await;
        assert!(stored.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_session_in_two_checkouts_records_from_neither() {
        use dibs_domain::paths::RepoPath;

        use crate::use_cases::{EditCheck, EditCheckInput};

        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, item, env.bob.session).await;
        beat(&env, env.bob.session, Some(in_checkout('a', Some("bob/1")))).await;
        let before = env.events().await.len();
        EditCheck::new(&env.deps)
            .execute(
                &env.bob.actor,
                EditCheckInput {
                    session: env.bob.session,
                    paths: vec![RepoPath::parse("default:packages/billing/a.rs").unwrap()],
                    reserve: true,
                    checkout: Some(in_checkout('b', Some("bob/1-agent"))),
                    worktree: None,
                },
            )
            .await
            .unwrap();
        beat(
            &env,
            env.bob.session,
            Some(in_checkout('a', Some("bob/1-next"))),
        )
        .await;
        assert!(recordings(&env, before).await.is_empty());
        assert_eq!(recorded(&env, item).await, from_checkout("bob/1"));
    }

    #[tokio::test]
    async fn a_branch_from_a_worktree_held_for_the_item_outlasts_any_checkout_heartbeat() {
        use dibs_domain::radar::WorktreeRef;
        use dibs_domain::report::MachineId;
        use dibs_domain::slot::Purpose;
        use dibs_domain::worktree::{PoolId, WorktreeName};

        use crate::use_cases::{TakeWorktree, TakeWorktreeInput};

        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, item, env.bob.session).await;
        let worktree = WorktreeRef {
            machine: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
            pool: PoolId::parse("a1b2c3").unwrap(),
            worktree: WorktreeName::parse("worktree1").unwrap(),
        };
        TakeWorktree::new(&env.deps)
            .execute(
                &env.bob.actor,
                TakeWorktreeInput {
                    session: env.bob.session,
                    machine: worktree.machine.clone(),
                    pool: worktree.pool.clone(),
                    names: vec![worktree.worktree.clone()],
                    agent: None,
                    purpose: Purpose::parse("refunds").unwrap(),
                    item: Some(item),
                },
            )
            .await
            .unwrap();
        let from_worktree = RecordedBranch::new(
            BranchName::parse("bob/1-worktree").unwrap(),
            BranchSource::Worktree(worktree),
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        let mut stored = tx.claim(env.project, item).await.unwrap().unwrap();
        stored
            .branches
            .insert(RepoId::implicit(), from_worktree.clone());
        tx.save_claim(env.project, &stored).await.unwrap();
        tx.commit().await.unwrap();
        let before = env.events().await.len();
        for name in ["bob/1-x", "bob/1-y", "bob/1-x"] {
            beat(&env, env.bob.session, Some(in_checkout('a', Some(name)))).await;
        }
        assert!(recordings(&env, before).await.is_empty());
        let mut tx = env.deps.store.begin().await.unwrap();
        let kept = tx.claim(env.project, item).await.unwrap().unwrap();
        assert_eq!(kept.branches.get(&RepoId::implicit()), Some(&from_worktree));
    }

    #[tokio::test]
    async fn a_heartbeat_keeps_a_branch_the_claim_learned_from_a_stronger_source() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, item, env.bob.session).await;
        let named = RecordedBranch::new(
            BranchName::parse("bob/1-named").unwrap(),
            BranchSource::Named,
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        let mut stored = tx.claim(env.project, item).await.unwrap().unwrap();
        stored.branches.insert(RepoId::implicit(), named.clone());
        tx.save_claim(env.project, &stored).await.unwrap();
        tx.commit().await.unwrap();
        let before = env.events().await.len();
        let from = in_checkout('a', Some("bob/9-elsewhere"));
        beat(&env, env.bob.session, Some(from)).await;
        assert!(env.kinds_after(before).await.is_empty());
        let mut tx = env.deps.store.begin().await.unwrap();
        let kept = tx.claim(env.project, item).await.unwrap().unwrap();
        assert_eq!(kept.branches.get(&RepoId::implicit()), Some(&named));
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
