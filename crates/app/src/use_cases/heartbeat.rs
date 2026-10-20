//! A session's sign of life.

use std::sync::Arc;

use dibs_domain::event::{BranchRecorded, EventKind};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::radar::heartbeat_branches;
use dibs_domain::ref_scheme::RefSchemeRegistry;

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

    /// Records the heartbeat.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] when the session is not the caller's; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        session: SessionId,
        branch: Option<&OnBranch>,
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
        work.commit(&*self.notifier).await?;
        Ok(HeartbeatOutcome {
            refreshed: kept.iter().map(|claim| claim.item).collect(),
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
    async fn a_heartbeat_names_a_session_of_the_caller() {
        let env = Env::new().await;
        let error = Heartbeat::new(&env.deps)
            .execute(&env.bob.actor, env.alice.session, None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
    }
}
