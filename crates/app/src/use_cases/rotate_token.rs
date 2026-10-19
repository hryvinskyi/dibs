//! Replacing a member's tokens with a new one.

use std::sync::Arc;

use dibs_domain::handle::Handle;

use crate::authority::AdminAuthority;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store};
use crate::records::MemberRecord;
use crate::token::SecretToken;
use crate::use_cases::common::{Enrolment, member_by_handle};
use crate::work::Work;

/// The member's new token.
#[derive(Debug)]
pub struct RotatedToken {
    /// The member.
    pub member: MemberRecord,
    /// The new token. It is shown once; only its hash is kept.
    pub token: SecretToken,
}

/// Revokes every token of a member and issues a new one, for an admin of the project or the operator
/// of the service. Browser sessions the old tokens opened end with them.
pub struct RotateToken {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    enrolment: Enrolment,
}

impl RotateToken {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            enrolment: Enrolment {
                ids: deps.ids.clone(),
                tokens: deps.tokens.clone(),
                hasher: deps.hasher.clone(),
            },
        }
    }

    /// Rotates the token of the member with this handle in the project `by` administers.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] unless `by` may administer the project; [`AppError::NotFound`] when
    /// the project has no such member; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        by: &impl AdminAuthority,
        handle: &Handle,
    ) -> Result<RotatedToken, AppError> {
        let project = by.administered_project()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, project).await?;
        let member = member_by_handle(&mut work, handle).await?;
        for token in work.tokens_of(project, member.id).await? {
            if token.is_active() {
                work.revoke_token(project, &token.hash, now).await?;
            }
        }
        let token = self
            .enrolment
            .issue_token(&mut work, member.id, now)
            .await?;
        work.commit_quietly().await?;
        Ok(RotatedToken { member, token })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::Authenticate;
    use crate::use_cases::testing::Env;

    #[tokio::test]
    async fn the_old_token_stops_and_the_new_one_works() {
        let env = Env::new().await;
        let rotated = RotateToken::new(&env.deps)
            .execute(&env.alice.actor, &Handle::parse("bob").unwrap())
            .await
            .unwrap();
        let authenticate = Authenticate::new(&env.deps);
        let actor = authenticate.execute(rotated.token.expose()).await.unwrap();
        assert_eq!(actor.member, env.bob.actor.member);
        let mut tx = env.deps.store.begin().await.unwrap();
        let old = tx
            .token_by_hash(&env.bob.actor.token)
            .await
            .unwrap()
            .unwrap();
        assert!(!old.record.is_active());
    }

    #[tokio::test]
    async fn rotation_is_for_admins_and_existing_members() {
        let env = Env::new().await;
        let rotate = RotateToken::new(&env.deps);
        let forbidden = rotate
            .execute(&env.bob.actor, &Handle::parse("alice").unwrap())
            .await
            .unwrap_err();
        assert_eq!(forbidden.code(), "forbidden");
        let missing = rotate
            .execute(&env.alice.actor, &Handle::parse("nobody").unwrap())
            .await
            .unwrap_err();
        assert_eq!(missing.code(), "not_found");
    }
}
