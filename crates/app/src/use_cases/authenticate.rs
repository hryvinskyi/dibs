//! Turning a bearer token into the member it authenticates.

use std::sync::Arc;

use dibs_domain::integration::call_due;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store, TokenHasher, Tx};
use crate::records::MemberRecord;
use crate::token::SecretToken;

/// Finds the member a token authenticates. A malformed, unknown or revoked token, and a token whose
/// member is gone or was removed from the project, are all refused the same way. An integration's
/// calls are recorded, at most one a minute, so the project can tell when its integrations fall
/// silent without a write on every request of a worker that polls.
pub struct Authenticate {
    store: Arc<dyn Store>,
    hasher: Arc<dyn TokenHasher>,
    clock: Arc<dyn Clock>,
}

impl Authenticate {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            hasher: deps.hasher.clone(),
            clock: deps.clock.clone(),
        }
    }

    /// Authenticates the token's text.
    ///
    /// # Errors
    /// [`AppError::Unauthorized`] for every way the token can fail; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(&self, token: &str) -> Result<Actor, AppError> {
        let mut tx = self.store.begin().await?;
        let actor = authenticate_in(&mut *tx, &*self.hasher, token).await?;
        if !actor.role.is_person() {
            let now = self.clock.now();
            let last = tx.last_call(actor.project, actor.member).await?;
            if call_due(last, now) {
                tx.record_call(actor.project, actor.member, now).await?;
                tx.commit().await?;
            }
        }
        Ok(actor)
    }
}

/// Authenticates a token inside an open transaction.
pub(crate) async fn authenticate_in(
    tx: &mut dyn Tx,
    hasher: &dyn TokenHasher,
    token: &str,
) -> Result<Actor, AppError> {
    let token = SecretToken::parse(token).map_err(|_| AppError::Unauthorized)?;
    let hash = hasher.hash(&token);
    let found = tx
        .token_by_hash(&hash)
        .await?
        .filter(|found| found.record.is_active())
        .ok_or(AppError::Unauthorized)?;
    let member = tx
        .member(found.project, found.record.member)
        .await?
        .filter(MemberRecord::is_present)
        .ok_or(AppError::Unauthorized)?;
    Ok(Actor {
        project: found.project,
        member: member.id,
        role: member.role,
        token: hash,
    })
}

#[cfg(test)]
mod tests {
    use dibs_domain::member::Role;

    use super::*;
    use crate::token::TokenPrefix;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{CreateMember, CreateMemberInput, RevokeToken};

    #[tokio::test]
    async fn a_token_authenticates_its_member_in_its_project() {
        let env = Env::new().await;
        assert_eq!(env.bob.actor.project, env.project);
        assert_eq!(env.bob.actor.role, Role::Member);
        assert_ne!(env.alice.actor.member, env.bob.actor.member);
    }

    #[tokio::test]
    async fn an_integrations_call_is_recorded_and_a_persons_is_not() {
        let env = Env::new().await;
        let calls = || async {
            let mut tx = env.deps.store.begin().await.unwrap();
            tx.last_calls(env.project).await.unwrap()
        };
        let before = calls().await;
        assert_eq!(
            before.iter().map(|(member, _)| *member).collect::<Vec<_>>(),
            [env.importer.member],
            "only the integration's own sign-in was recorded"
        );
        let token = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: crate::use_cases::testing::new_member("worker"),
                    role: Role::Importer,
                },
            )
            .await
            .unwrap()
            .token;
        env.fakes.clock.advance(time::Duration::minutes(3));
        let authenticate = Authenticate::new(&env.deps);
        let worker = authenticate.execute(token.expose()).await.unwrap();
        let first = crate::use_cases::testing::START + time::Duration::minutes(3);
        let after = calls().await;
        assert_eq!(after.len(), 2);
        assert!(after.contains(&(worker.member, first)));
        env.fakes.clock.advance(time::Duration::seconds(30));
        authenticate.execute(token.expose()).await.unwrap();
        assert!(
            calls().await.contains(&(worker.member, first)),
            "a call within the minute is not written again"
        );
        env.fakes.clock.advance(time::Duration::seconds(31));
        authenticate.execute(token.expose()).await.unwrap();
        assert!(
            calls()
                .await
                .contains(&(worker.member, first + time::Duration::seconds(61)))
        );
    }

    #[tokio::test]
    async fn every_bad_token_is_refused_alike() {
        let env = Env::new().await;
        let authenticate = Authenticate::new(&env.deps);
        let unknown = format!("dibs_{}", "q".repeat(43));
        for text in ["", "not a token", "dibs_short", unknown.as_str()] {
            let error = authenticate.execute(text).await.unwrap_err();
            assert!(matches!(error, AppError::Unauthorized), "{text:?}");
        }
    }

    #[tokio::test]
    async fn a_revoked_token_no_longer_authenticates() {
        let env = Env::new().await;
        let created = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: crate::use_cases::testing::new_member("carol"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap();
        let authenticate = Authenticate::new(&env.deps);
        assert!(authenticate.execute(created.token.expose()).await.is_ok());
        let prefix = TokenPrefix::parse(created.token.prefix().as_str()).unwrap();
        RevokeToken::new(&env.deps)
            .execute(&env.alice.actor, prefix)
            .await
            .unwrap();
        let error = authenticate
            .execute(created.token.expose())
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Unauthorized));
    }
}
