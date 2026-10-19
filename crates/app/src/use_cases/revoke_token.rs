//! Revoking one token by its prefix.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::ports::{Clock, Store};
use crate::records::TokenRecord;
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

    /// Revokes the token.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] unless the caller is an admin; [`AppError::NotFound`] when the
    /// project has no token of the prefix; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        prefix: TokenPrefix,
    ) -> Result<TokenRecord, AppError> {
        actor.require_admin()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let mut token = work
            .token_by_prefix(actor.project, &prefix)
            .await?
            .ok_or(AppError::NotFound(Missing::Token(prefix)))?;
        if token.is_active() {
            work.revoke_token(actor.project, &token.hash, now).await?;
            token.revoked_at = Some(now);
        }
        work.commit_quietly().await?;
        Ok(token)
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
        assert_eq!(first.revoked_at, Some(crate::use_cases::testing::START));
        env.advance(time::Duration::minutes(5));
        let again = revoke
            .execute(&env.alice.actor, bob.prefix.clone())
            .await
            .unwrap();
        assert_eq!(again.revoked_at, first.revoked_at);
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
