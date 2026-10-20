//! Adding a member to a project.

use std::sync::Arc;

use dibs_domain::member::Role;

use crate::authority::AdminAuthority;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store};
use crate::records::MemberRecord;
use crate::token::SecretToken;
use crate::use_cases::common::{Enrolment, NewMember};
use crate::work::Work;

/// The member to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateMemberInput {
    /// Who.
    pub member: NewMember,
    /// With what role.
    pub role: Role,
}

/// The added member.
#[derive(Debug)]
pub struct CreatedMember {
    /// The member.
    pub member: MemberRecord,
    /// Their token. It is shown once; only its hash is kept.
    pub token: SecretToken,
}

/// Adds a member to a project and issues their token, for an admin of the project or the operator
/// of the service.
pub struct CreateMember {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    enrolment: Enrolment,
}

impl CreateMember {
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

    /// Adds the member to the project `by` administers.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] unless `by` may administer the project; [`AppError::Duplicate`]
    /// when the handle is taken in the project; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        by: &impl AdminAuthority,
        input: CreateMemberInput,
    ) -> Result<CreatedMember, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_administering(&*self.store, by).await?;
        let member = self
            .enrolment
            .add_member(&mut work, input.member, input.role, now)
            .await?;
        let token = self
            .enrolment
            .issue_token(&mut work, member.id, now)
            .await?;
        work.commit_quietly().await?;
        Ok(CreatedMember { member, token })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::{Env, new_member};
    use crate::use_cases::{Authenticate, ListMembers};

    #[tokio::test]
    async fn an_admin_adds_a_member_whose_token_works() {
        let env = Env::new().await;
        let created = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: new_member("carol"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap();
        let actor = Authenticate::new(&env.deps)
            .execute(created.token.expose())
            .await
            .unwrap();
        assert_eq!(actor.member, created.member.id);
        assert_eq!(actor.project, env.project);
        let handles: Vec<String> = ListMembers::new(&env.deps)
            .execute(&env.bob.actor)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.handle.to_string())
            .collect();
        assert_eq!(handles, ["alice", "bob", "carol", "importer"]);
    }

    #[tokio::test]
    async fn only_an_admin_adds_members() {
        let env = Env::new().await;
        let error = CreateMember::new(&env.deps)
            .execute(
                &env.bob.actor,
                CreateMemberInput {
                    member: new_member("carol"),
                    role: Role::Admin,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "forbidden");
    }

    #[tokio::test]
    async fn a_handle_is_taken_once_per_project() {
        let env = Env::new().await;
        let error = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: new_member("bob"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "handle_taken");
    }
}
