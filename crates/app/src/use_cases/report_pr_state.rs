//! An integration reporting whether a pull request merges cleanly into its base.

use std::sync::Arc;

use dibs_domain::event::{EventKind, PrStateReported};
use dibs_domain::review::PrRef;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, Store};
use crate::work::Work;

/// Keeps what an integration last reported about each pull request, and appends a
/// `PrStateReported` event when a pull request is reported not to merge cleanly or merges cleanly
/// again after that; the same report twice appends nothing, and a pull request first reported as
/// merging cleanly is only remembered. The pull request need not be one a claim recorded.
pub struct ReportPrState {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl ReportPrState {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Records the report; answers whether it appended an event.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for anyone but an admin or the importer; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        pr: PrRef,
        mergeable: bool,
    ) -> Result<bool, AppError> {
        actor.require_import()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let kept = work.pr_mergeable(actor.project, &pr).await?;
        work.save_pr_mergeable(actor.project, &pr, mergeable, now)
            .await?;
        let told = kept != Some(mergeable) && (!mergeable || kept == Some(false));
        if told {
            work.record(
                now,
                EventKind::PrStateReported(PrStateReported { pr, mergeable }),
            )
            .await?;
        }
        work.commit(&*self.notifier).await?;
        Ok(told)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;

    #[tokio::test]
    async fn a_conflict_and_its_end_are_told_once_each_and_a_clean_start_is_only_remembered() {
        let env = Env::new().await;
        let report = ReportPrState::new(&env.deps);
        let pr = |n: u32| PrRef::parse(&format!("acme/widgets#{n}")).unwrap();
        let before = env.events().await.len();
        let answers = [
            report.execute(&env.importer, pr(1), true).await.unwrap(),
            report.execute(&env.importer, pr(1), true).await.unwrap(),
            report.execute(&env.importer, pr(1), false).await.unwrap(),
            report.execute(&env.importer, pr(1), false).await.unwrap(),
            report.execute(&env.alice.actor, pr(1), true).await.unwrap(),
            report.execute(&env.importer, pr(2), false).await.unwrap(),
        ];
        assert_eq!(answers, [false, false, true, false, true, true]);
        let kinds = env.kinds_after(before).await;
        let told: Vec<(String, bool)> = kinds
            .iter()
            .filter_map(|kind| match kind {
                EventKind::PrStateReported(reported) => {
                    Some((reported.pr.to_string(), reported.mergeable))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            told,
            [
                ("acme/widgets#1".to_owned(), false),
                ("acme/widgets#1".to_owned(), true),
                ("acme/widgets#2".to_owned(), false)
            ]
        );
        let error = report
            .execute(&env.bob.actor, pr(3), false)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "forbidden");
    }

    #[tokio::test]
    async fn a_pull_request_reported_conflicting_for_months_is_kept_and_counted_once() {
        use crate::use_cases::Prune;
        use time::Duration;

        let env = Env::new().await;
        let report = ReportPrState::new(&env.deps);
        let pr = PrRef::parse("acme/widgets#9").unwrap();
        let told = || async {
            env.events()
                .await
                .into_iter()
                .filter(|event| matches!(event.kind, EventKind::PrStateReported(_)))
                .count()
        };
        for month in 0..5 {
            report
                .execute(&env.importer, pr.clone(), false)
                .await
                .unwrap();
            if month == 2 {
                assert_eq!(told().await, 1, "three reports of one conflict, told once");
            }
            env.advance(Duration::days(30));
            Prune::new(&env.deps).execute().await.unwrap();
        }
        let mut tx = env.deps.store.begin().await.unwrap();
        assert_eq!(
            tx.pr_mergeable(env.project, &pr).await.unwrap(),
            Some(false),
            "150 days on, last reported a month ago, it is still known"
        );
        drop(tx);
        assert!(
            report.execute(&env.importer, pr, true).await.unwrap(),
            "so merging cleanly again is told"
        );
    }
}
