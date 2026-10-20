//! A session's sign of life.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store};
use crate::use_cases::common::own_session;
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
pub struct Heartbeat {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl Heartbeat {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            hot_rules: deps.hot_rules.clone(),
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
    ) -> Result<HeartbeatOutcome, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let mut session = own_session(&mut work, actor, session).await?;
        let stale_after = work.project_settings(&self.hot_rules).await?.stale_after();
        session.last_seen = now;
        work.save_session(actor.project, &session).await?;
        let mut refreshed = Vec::new();
        for mut claim in work.claims(actor.project).await? {
            let ours = claim.member == actor.member && claim.host == session.host;
            if ours && claim.is_live(now, stale_after) {
                claim.last_seen = now;
                work.save_claim(actor.project, &claim).await?;
                refreshed.push(claim.item);
            }
        }
        let mut board = work.slot_board(actor.project).await?;
        if board.refresh_member(actor.member, now) {
            work.put_slot_board(actor.project, &board).await?;
        }
        work.commit_quietly().await?;
        Ok(HeartbeatOutcome { refreshed })
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
            .execute(&env.bob.actor, env.bob.session)
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
    async fn a_heartbeat_names_a_session_of_the_caller() {
        let env = Env::new().await;
        let error = Heartbeat::new(&env.deps)
            .execute(&env.bob.actor, env.alice.session)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
    }
}
