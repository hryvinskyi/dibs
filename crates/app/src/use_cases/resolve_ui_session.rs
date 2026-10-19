//! Recognising a signed-in browser.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store, TokenHasher};
use crate::token::SecretToken;
use crate::use_cases::open_ui_session::UI_SESSION_LIFETIME;
use crate::work::Work;

/// Finds the person a browser session belongs to and moves its end to [`UI_SESSION_LIFETIME`] from
/// now. A session that expired, whose token was revoked, or whose member is gone is refused and
/// removed.
pub struct ResolveUiSession {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    hasher: Arc<dyn TokenHasher>,
}

impl ResolveUiSession {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            hasher: deps.hasher.clone(),
        }
    }

    /// Resolves the session id the browser sent.
    ///
    /// # Errors
    /// [`AppError::Unauthorized`] for every way the session can fail; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(&self, secret: &str) -> Result<Actor, AppError> {
        let now = self.clock.now();
        let secret = SecretToken::parse(secret).map_err(|_| AppError::Unauthorized)?;
        let hash = self.hasher.hash(&secret);
        let mut tx = self.store.begin().await?;
        let found = tx.ui_session(&hash).await?.ok_or(AppError::Unauthorized)?;
        let project = found.project;
        let session = found.record;
        let mut work = Work::adopt(tx, project);
        let token = work
            .token_by_hash(&session.token)
            .await?
            .filter(|token| token.project == project && token.record.is_active());
        let member = if token.is_some() {
            work.member(project, session.member).await?
        } else {
            None
        };
        let Some(member) = member.filter(|_| session.expires_at >= now) else {
            work.delete_ui_session(project, &hash).await?;
            work.commit_quietly().await?;
            return Err(AppError::Unauthorized);
        };
        work.extend_ui_session(project, &hash, now + UI_SESSION_LIFETIME)
            .await?;
        work.commit_quietly().await?;
        Ok(Actor {
            project,
            member: member.id,
            role: member.role,
            token: session.token,
        })
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::token::TokenPrefix;
    use crate::use_cases::testing::{Env, START};
    use crate::use_cases::{
        CreateMember, CreateMemberInput, OpenUiSession, OpenedUiSession, RevokeToken,
    };

    async fn sign_in(env: &Env) -> (OpenedUiSession, TokenPrefix) {
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
        (opened, created.token.prefix())
    }

    async fn stored_end(env: &Env, opened: &OpenedUiSession) -> Option<time::OffsetDateTime> {
        let hash = env.deps.hasher.hash(&opened.secret);
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.ui_session(&hash)
            .await
            .unwrap()
            .map(|found| found.record.expires_at)
    }

    #[tokio::test]
    async fn each_use_moves_the_end_of_the_session() {
        let env = Env::new().await;
        let (opened, _) = sign_in(&env).await;
        env.advance(Duration::hours(11));
        let resolve = ResolveUiSession::new(&env.deps);
        resolve.execute(opened.secret.expose()).await.unwrap();
        assert_eq!(
            stored_end(&env, &opened).await,
            Some(START + Duration::hours(11) + UI_SESSION_LIFETIME)
        );
        env.advance(Duration::hours(11));
        assert!(resolve.execute(opened.secret.expose()).await.is_ok());
    }

    #[tokio::test]
    async fn an_expired_session_is_refused_and_removed() {
        let env = Env::new().await;
        let (opened, _) = sign_in(&env).await;
        env.advance(UI_SESSION_LIFETIME + Duration::seconds(1));
        let error = ResolveUiSession::new(&env.deps)
            .execute(opened.secret.expose())
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Unauthorized));
        assert_eq!(stored_end(&env, &opened).await, None);
    }

    #[tokio::test]
    async fn revoking_the_token_ends_the_sessions_it_opened() {
        let env = Env::new().await;
        let (opened, prefix) = sign_in(&env).await;
        RevokeToken::new(&env.deps)
            .execute(&env.alice.actor, prefix)
            .await
            .unwrap();
        let error = ResolveUiSession::new(&env.deps)
            .execute(opened.secret.expose())
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Unauthorized));
        assert_eq!(stored_end(&env, &opened).await, None);
    }

    #[tokio::test]
    async fn unknown_and_malformed_sessions_are_refused_alike() {
        let env = Env::new().await;
        let resolve = ResolveUiSession::new(&env.deps);
        let unknown = format!("dibs_{}", "u".repeat(43));
        for text in ["", "garbage", unknown.as_str()] {
            assert!(matches!(
                resolve.execute(text).await.unwrap_err(),
                AppError::Unauthorized
            ));
        }
    }
}
