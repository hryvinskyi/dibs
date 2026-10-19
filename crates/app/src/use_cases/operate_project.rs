//! The operator of the service taking up one project by its slug.

use std::sync::Arc;

use dibs_domain::project::ProjectSlug;

use crate::authority::Operator;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::ports::Store;

/// Finds a project by its slug and issues the operator's authority over it, for the service's own
/// command line. Nothing is written.
pub struct OperateProject {
    store: Arc<dyn Store>,
}

impl OperateProject {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The operator of the project of this slug.
    ///
    /// # Errors
    /// [`AppError::NotFound`] when no project has the slug; [`AppError::Store`] when the store
    /// fails.
    pub async fn execute(&self, slug: &ProjectSlug) -> Result<Operator, AppError> {
        let mut tx = self.store.begin().await?;
        let project = tx
            .project_by_slug(slug)
            .await?
            .ok_or_else(|| AppError::NotFound(Missing::Project(slug.clone())))?;
        Ok(Operator::of(project.id))
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::member::Role;

    use super::*;
    use crate::use_cases::testing::{Env, new_member};
    use crate::use_cases::{Authenticate, CreateMember, CreateMemberInput, RotateToken};

    #[tokio::test]
    async fn an_unknown_slug_is_not_found() {
        let env = Env::new().await;
        let error = OperateProject::new(&env.deps)
            .execute(&ProjectSlug::parse("gadgets").unwrap())
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
        assert_eq!(error.to_string(), "there is no project 'gadgets'");
    }

    #[tokio::test]
    async fn the_operator_adds_members_and_rotates_an_admins_token() {
        let env = Env::new().await;
        let operator = OperateProject::new(&env.deps)
            .execute(&ProjectSlug::parse("widgets").unwrap())
            .await
            .unwrap();
        assert_eq!(operator.project(), env.project);

        let created = CreateMember::new(&env.deps)
            .execute(
                &operator,
                CreateMemberInput {
                    member: new_member("carol"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap();
        let authenticate = Authenticate::new(&env.deps);
        let carol = authenticate.execute(created.token.expose()).await.unwrap();
        assert_eq!(carol.project, env.project);

        let rotated = RotateToken::new(&env.deps)
            .execute(&operator, &Handle::parse("alice").unwrap())
            .await
            .unwrap();
        let alice = authenticate.execute(rotated.token.expose()).await.unwrap();
        assert_eq!(alice.member, env.alice.actor.member);
        assert_eq!(alice.role, Role::Admin);
        let mut tx = env.deps.store.begin().await.unwrap();
        let old = tx
            .token_by_hash(&env.alice.actor.token)
            .await
            .unwrap()
            .unwrap();
        assert!(!old.record.is_active());
    }
}
