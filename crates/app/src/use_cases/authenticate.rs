//! Turning a bearer token into the member it authenticates.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Store, TokenHasher, Tx};
use crate::records::MemberRecord;
use crate::token::SecretToken;

/// Finds the member a token authenticates. A malformed, unknown or revoked token, and a token whose
/// member is gone or was removed from the project, are all refused the same way.
pub struct Authenticate {
    store: Arc<dyn Store>,
    hasher: Arc<dyn TokenHasher>,
}

impl Authenticate {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            hasher: deps.hasher.clone(),
        }
    }

    /// Authenticates the token's text.
    ///
    /// # Errors
    /// [`AppError::Unauthorized`] for every way the token can fail; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(&self, token: &str) -> Result<Actor, AppError> {
        let mut tx = self.store.begin().await?;
        authenticate_in(&mut *tx, &*self.hasher, token).await
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
