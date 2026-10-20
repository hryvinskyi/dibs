//! Revoking one token by its prefix.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::named::{NamedToken, Names};
use crate::ports::{Clock, Store};
use crate::token::TokenPrefix;
use crate::work::Work;

/// Revokes the project's token of a prefix. Browser sessions it opened end with it. Revoking a
/// revoked token changes nothing.
pub struct RevokeToken {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
}

impl RevokeToken {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
        }
    }

    /// Revokes the token, answering it with its member's handle.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] unless the caller is an admin; [`AppError::NotFound`] when the
    /// project has no token of the prefix; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        prefix: TokenPrefix,
    ) -> Result<NamedToken, AppError> {
        actor.require_admin()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let mut token = work
            .token_by_prefix(actor.project, &prefix)
            .await?
            .ok_or(AppError::NotFound(Missing::Token(prefix)))?;
        if token.is_active() {
            work.revoke_token(actor.project, &token.hash, now).await?;
            token.revoked_at = Some(now);
        }
        let revoked = Names::load(&mut work).await?.token(token)?;
        work.commit_quietly().await?;
        Ok(revoked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;

    #[tokio::test]
    async fn a_token_is_revoked_once_and_stays_revoked() {
        let env = Env::new().await;
        let mut tx = env.deps.store.begin().await.unwrap();
        let bob = tx
            .token_by_hash(&env.bob.actor.token)
            .await
            .unwrap()
            .unwrap()
            .record;
        drop(tx);
        let revoke = RevokeToken::new(&env.deps);
        let first = revoke
            .execute(&env.alice.actor, bob.prefix.clone())
            .await
            .unwrap();
        assert_eq!(
            first.record.revoked_at,
            Some(crate::use_cases::testing::START)
        );
        assert_eq!(first.member.as_str(), "bob");
        env.advance(time::Duration::minutes(5));
        let again = revoke
            .execute(&env.alice.actor, bob.prefix.clone())
            .await
            .unwrap();
        assert_eq!(again.record.revoked_at, first.record.revoked_at);
    }

    #[tokio::test]
    async fn revoking_needs_an_admin_and_a_known_prefix() {
        let env = Env::new().await;
        let revoke = RevokeToken::new(&env.deps);
        let prefix = TokenPrefix::parse("zzzzzzzz").unwrap();
        let forbidden = revoke
            .execute(&env.bob.actor, prefix.clone())
            .await
            .unwrap_err();
        assert_eq!(forbidden.code(), "forbidden");
        let missing = revoke.execute(&env.alice.actor, prefix).await.unwrap_err();
        assert_eq!(missing.code(), "not_found");
    }
}
