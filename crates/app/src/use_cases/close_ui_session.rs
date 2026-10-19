//! Signing a browser out.

use std::sync::Arc;

use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Store, TokenHasher};
use crate::token::SecretToken;
use crate::work::Work;

/// Ends a browser session. Signing out a session that does not exist, or no longer does, succeeds.
pub struct CloseUiSession {
    store: Arc<dyn Store>,
    hasher: Arc<dyn TokenHasher>,
}

impl CloseUiSession {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            hasher: deps.hasher.clone(),
        }
    }

    /// Ends the session the browser sent.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, secret: &str) -> Result<(), AppError> {
        let Ok(secret) = SecretToken::parse(secret) else {
            return Ok(());
        };
        let hash = self.hasher.hash(&secret);
        let mut tx = self.store.begin().await?;
        let Some(found) = tx.ui_session(&hash).await? else {
            return Ok(());
        };
        let mut work = Work::adopt(tx, found.project);
        work.delete_ui_session(found.project, &hash).await?;
        work.commit_quietly().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{CreateMember, CreateMemberInput, OpenUiSession, ResolveUiSession};

    #[tokio::test]
    async fn a_closed_session_no_longer_resolves_and_closing_again_succeeds() {
        let env = Env::new().await;
        let created = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: crate::use_cases::testing::new_member("carol"),
                    role: dibs_domain::member::Role::Member,
                },
            )
            .await
            .unwrap();
        let opened = OpenUiSession::new(&env.deps)
            .execute(created.token.expose())
            .await
            .unwrap();
        let close = CloseUiSession::new(&env.deps);
        close.execute(opened.secret.expose()).await.unwrap();
        let error = ResolveUiSession::new(&env.deps)
            .execute(opened.secret.expose())
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Unauthorized));
        close.execute(opened.secret.expose()).await.unwrap();
        close.execute("not a session").await.unwrap();
    }
}
