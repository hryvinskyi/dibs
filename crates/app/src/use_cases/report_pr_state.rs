//! An integration reporting whether a pull request merges cleanly into its base.

use std::sync::Arc;

use dibs_domain::event::{EventKind, PrStateReported};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::radar::{BranchName, BranchSource};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::review::PrRef;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::radar::Radar;
use crate::work::Work;

/// What an integration reports about one pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStateReport {
    /// The pull request.
    pub pr: PrRef,
    /// Whether it merges cleanly into its base.
    pub mergeable: bool,
    /// Its head branch, when the integration read it.
    pub head: Option<BranchName>,
}

/// Keeps what an integration last reported about each pull request, and appends a
/// `PrStateReported` event when a pull request is reported not to merge cleanly or merges cleanly
/// again after that; the same report twice appends nothing, and a pull request first reported as
/// merging cleanly is only remembered. The pull request need not be one a claim recorded.
///
/// A report that names the pull request's head records it on the claim in review that records the
/// pull request, as the claim's branch in the pull request's repository learned from that pull
/// request, as recording the pull request with its head does. A pull request no claim in review
/// records, or one of a repository the project does not list, records nothing; nor does any head
/// while the project's stored settings do not parse, whose report records the state all the same.
pub struct ReportPrState {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl ReportPrState {
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

    /// Records the report; answers whether it appended a `PrStateReported` event.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for anyone but an admin or the importer; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(&self, actor: &Actor, report: PrStateReport) -> Result<bool, AppError> {
        actor.require_import()?;
        let now = self.clock.now();
        let PrStateReport {
            pr,
            mergeable,
            head,
        } = report;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let kept = work.pr_mergeable(actor.project, &pr).await?;
        work.save_pr_mergeable(actor.project, &pr, mergeable, now)
            .await?;
        let told = kept != Some(mergeable) && (!mergeable || kept == Some(false));
        if told {
            work.record(
                now,
                EventKind::PrStateReported(PrStateReported {
                    pr: pr.clone(),
                    mergeable,
                }),
            )
            .await?;
        }
        let settings = match &head {
            None => None,
            Some(_) => match work.project_settings(&self.hot_rules).await {
                Ok(settings) => Some(settings),
                Err(AppError::StoredSettings(_)) => None,
                Err(error) => return Err(error),
            },
        };
        if let (Some(head), Some(settings)) = (head, settings) {
            let repo = settings.repo_of_pr(&pr).map(|repo| repo.id.clone());
            let claims = work.claims(actor.project).await?;
            let recording = claims.iter().find(|claim| claim.state.records(&pr));
            if let (Some(repo), Some(claim)) = (repo, recording) {
                Radar {
                    ids: &*self.ids,
                    schemes: &self.ref_schemes,
                    settings: &settings,
                    now,
                }
                .record_branch(
                    &mut work,
                    claim,
                    &repo,
                    &head,
                    BranchSource::PullRequest(pr),
                )
                .await?;
            }
        }
        work.commit(&*self.notifier).await?;
        Ok(told)
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::ids::ItemId;
    use dibs_domain::radar::BranchSourceKind;
    use dibs_domain::repo::RepoId;

    use super::*;
    use crate::use_cases::testing::{Env, Person, document};
    use crate::use_cases::{
        ApplySettings, ClaimItem, ClaimItemInput, StartReview, StartReviewInput,
    };

    /// A report that `pr` merges cleanly or not, naming no head.
    fn state(pr: PrRef, mergeable: bool) -> PrStateReport {
        PrStateReport {
            pr,
            mergeable,
            head: None,
        }
    }

    fn pr(text: &str) -> PrRef {
        PrRef::parse(text).unwrap()
    }

    fn branch(text: &str) -> BranchName {
        BranchName::parse(text).unwrap()
    }

    /// A report that `text` merges cleanly, with its head `head`.
    async fn report_head(env: &Env, text: &str, head: &str) -> bool {
        let report = PrStateReport {
            pr: pr(text),
            mergeable: true,
            head: Some(branch(head)),
        };
        ReportPrState::new(&env.deps)
            .execute(&env.importer, report)
            .await
            .unwrap()
    }

    /// `who` claims `item`, then records the pull requests `prs` on the claim, more to follow.
    async fn in_review(env: &Env, who: &Person, item: ItemId, prs: &[&str]) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    branches: Default::default(),
                    item,
                    session: who.session,
                    areas: Vec::new(),
                    wide: None,
                    kind: None,
                },
            )
            .await
            .unwrap();
        for text in prs {
            StartReview::new(&env.deps)
                .execute(
                    &who.actor,
                    StartReviewInput {
                        head: None,
                        item,
                        pr: pr(text),
                        more: true,
                    },
                )
                .await
                .unwrap();
        }
    }

    /// The branch `item`'s claim records in `repo`, and the pull request it was learned from.
    async fn recorded(env: &Env, item: ItemId, repo: &str) -> Option<(String, Option<String>)> {
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        let recorded = stored.branches.get(&RepoId::parse(repo).unwrap())?;
        Some((
            recorded.branch.to_string(),
            recorded.source.pr().map(ToString::to_string),
        ))
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

    #[tokio::test]
    async fn a_conflict_and_its_end_are_told_once_each_and_a_clean_start_is_only_remembered() {
        let env = Env::new().await;
        let report = ReportPrState::new(&env.deps);
        let pr = |n: u32| PrRef::parse(&format!("acme/widgets#{n}")).unwrap();
        let before = env.events().await.len();
        let answers = [
            report.execute(&env.importer, state(pr(1), true)).await,
            report.execute(&env.importer, state(pr(1), true)).await,
            report.execute(&env.importer, state(pr(1), false)).await,
            report.execute(&env.importer, state(pr(1), false)).await,
            report.execute(&env.alice.actor, state(pr(1), true)).await,
            report.execute(&env.importer, state(pr(2), false)).await,
        ]
        .map(Result::unwrap);
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
            .execute(&env.bob.actor, state(pr(3), false))
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
                .execute(&env.importer, state(pr.clone(), false))
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
            report
                .execute(&env.importer, state(pr, true))
                .await
                .unwrap(),
            "so merging cleanly again is told"
        );
    }

    #[tokio::test]
    async fn a_head_is_recorded_on_the_claim_in_review_that_records_its_pull_request_alone() {
        let env = Env::new().await;
        let bobs = env.item("gh:acme/widgets#1").await;
        let alices = env.item("gh:acme/widgets#2").await;
        in_review(&env, &env.bob, bobs, &["acme/widgets#7"]).await;
        in_review(&env, &env.alice, alices, &["acme/widgets#8"]).await;
        let before = env.events().await.len();
        assert!(
            !report_head(&env, "acme/widgets#7", "bob/1-refunds").await,
            "the answer still says whether a state event was appended"
        );
        assert_eq!(
            recordings(&env, before).await,
            [(
                bobs,
                "bob/1-refunds".to_owned(),
                BranchSourceKind::PullRequest
            )]
        );
        assert_eq!(
            recorded(&env, bobs, "default").await,
            Some((
                "bob/1-refunds".to_owned(),
                Some("acme/widgets#7".to_owned())
            ))
        );
        assert_eq!(recorded(&env, alices, "default").await, None);
    }

    #[tokio::test]
    async fn the_two_pull_requests_of_one_claim_reported_in_turn_change_its_branch_once() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        in_review(&env, &env.bob, item, &["acme/widgets#7", "acme/widgets#8"]).await;
        let before = env.events().await.len();
        for _ in 0..2 {
            report_head(&env, "acme/widgets#7", "bob/1-first").await;
            report_head(&env, "acme/widgets#8", "bob/1-second").await;
        }
        assert_eq!(
            recordings(&env, before).await,
            [(
                item,
                "bob/1-first".to_owned(),
                BranchSourceKind::PullRequest
            )],
            "the head of the pull request recorded first holds while it is recorded"
        );
        assert_eq!(
            recorded(&env, item, "default").await,
            Some(("bob/1-first".to_owned(), Some("acme/widgets#7".to_owned())))
        );
    }

    #[tokio::test]
    async fn a_head_of_a_pull_request_nobody_records_or_of_an_unlisted_repository_records_nothing()
    {
        let env = Env::with_settings(
            r#"{"repos": [
                {"id": "api", "remote": "github.com/acme/api"},
                {"id": "web", "remote": "github.com/acme/web"}]}"#,
        )
        .await;
        let item = env.item("gh:acme/api#1").await;
        in_review(&env, &env.bob, item, &["acme/api#7"]).await;
        let before = env.events().await.len();
        report_head(&env, "acme/api#9", "bob/1-other").await;
        ApplySettings::new(&env.deps)
            .execute(
                &env.alice.actor,
                document(r#"{"repos": [{"id": "web", "remote": "github.com/acme/web"}]}"#),
            )
            .await
            .unwrap();
        report_head(&env, "acme/api#7", "bob/1-refunds").await;
        assert!(recordings(&env, before).await.is_empty());
        assert_eq!(recorded(&env, item, "api").await, None);
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.claim(env.project, item).await.unwrap().unwrap();
        assert!(
            stored.state.records(&pr("acme/api#7")),
            "the claim still records the pull request, of a repository no longer listed"
        );
    }

    #[tokio::test]
    async fn settings_that_do_not_parse_record_the_state_of_a_report_with_a_head_and_no_head() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        in_review(&env, &env.bob, item, &["acme/widgets#7"]).await;
        let unknown = r#"{"hot": [{"paths": ["**/*.lock"], "kind": "rebuild-on-merge"}]}"#;
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.put_settings(env.project, &document(unknown))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let before = env.events().await.len();
        let report = PrStateReport {
            pr: pr("acme/widgets#7"),
            mergeable: false,
            head: Some(branch("bob/1-refunds")),
        };
        let told = ReportPrState::new(&env.deps)
            .execute(&env.importer, report)
            .await
            .unwrap();
        assert!(told, "the conflict is told");
        let mut tx = env.deps.store.begin().await.unwrap();
        assert_eq!(
            tx.pr_mergeable(env.project, &pr("acme/widgets#7"))
                .await
                .unwrap(),
            Some(false)
        );
        drop(tx);
        assert!(recordings(&env, before).await.is_empty());
        assert_eq!(recorded(&env, item, "default").await, None);
    }
}
