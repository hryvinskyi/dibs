//! Signing in to the browser interface with a token.

use std::sync::Arc;

use time::{Duration, OffsetDateTime};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store, TokenGenerator, TokenHasher};
use crate::records::UiSessionRecord;
use crate::token::SecretToken;
use crate::use_cases::authenticate::authenticate_in;
use crate::work::Work;

/// How long a browser session lasts without being used; every use starts the time again.
pub const UI_SESSION_LIFETIME: Duration = Duration::hours(12);

/// The browser session opened.
#[derive(Debug)]
pub struct OpenedUiSession {
    /// The session's id, for the browser's cookie. It is shown once; only its hash is kept.
    pub secret: SecretToken,
    /// Who signed in.
    pub actor: Actor,
    /// When it ends unless used before then.
    pub expires_at: OffsetDateTime,
}

/// Opens a browser session for the person a token authenticates. The session is bound to that
/// token, so revoking the token ends it. Expired sessions of every project are swept on each
/// sign-in.
pub struct OpenUiSession {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    tokens: Arc<dyn TokenGenerator>,
    hasher: Arc<dyn TokenHasher>,
}

impl OpenUiSession {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            tokens: deps.tokens.clone(),
            hasher: deps.hasher.clone(),
        }
    }

    /// Signs in with the token's text.
    ///
    /// # Errors
    /// [`AppError::Unauthorized`] for every way the token can fail; [`AppError::Forbidden`] for an
    /// integration's token; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, token: &str) -> Result<OpenedUiSession, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let actor = authenticate_in(&mut *tx, &*self.hasher, token).await?;
        actor.require_person()?;
        let mut work = Work::adopt(tx, actor.project);
        work.delete_expired_ui_sessions(now).await?;
        let secret = self.tokens.generate();
        let expires_at = now + UI_SESSION_LIFETIME;
        let record = UiSessionRecord {
            hash: self.hasher.hash(&secret),
            member: actor.member,
            token: actor.token.clone(),
            expires_at,
        };
        work.insert_ui_session(actor.project, &record).await?;
        work.commit_quietly().await?;
        Ok(OpenedUiSession {
            secret,
            actor,
            expires_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::{Env, START};
    use crate::use_cases::{CreateMember, CreateMemberInput, ResolveUiSession};

    #[tokio::test]
    async fn a_person_signs_in_and_the_session_is_bound_to_their_token() {
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
        assert_eq!(opened.actor.member, created.member.id);
        assert_eq!(opened.expires_at, START + UI_SESSION_LIFETIME);
        let hash = env.deps.hasher.hash(&opened.secret);
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.ui_session(&hash).await.unwrap().unwrap();
        assert_eq!(stored.project, env.project);
        assert_eq!(stored.record.token, opened.actor.token);
        drop(tx);
        let resolved = ResolveUiSession::new(&env.deps)
            .execute(opened.secret.expose())
            .await
            .unwrap();
        assert_eq!(resolved, opened.actor);
    }

    #[tokio::test]
    async fn bad_tokens_and_integrations_cannot_sign_in() {
        let env = Env::new().await;
        let open = OpenUiSession::new(&env.deps);
        assert_eq!(
            open.execute("nope").await.unwrap_err().code(),
            "unauthorized"
        );
        let created = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: crate::use_cases::testing::new_member("mirror"),
                    role: dibs_domain::member::Role::Importer,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            open.execute(created.token.expose())
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }

    #[tokio::test]
    async fn signing_in_sweeps_expired_sessions() {
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
        let open = OpenUiSession::new(&env.deps);
        let old = open.execute(created.token.expose()).await.unwrap();
        env.advance(UI_SESSION_LIFETIME + Duration::minutes(1));
        open.execute(created.token.expose()).await.unwrap();
        let mut tx = env.deps.store.begin().await.unwrap();
        let hash = env.deps.hasher.hash(&old.secret);
        assert_eq!(tx.ui_session(&hash).await.unwrap(), None);
    }
}
