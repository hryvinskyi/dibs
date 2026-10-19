//! Creating a project with its first admin.

use std::sync::Arc;

use dibs_domain::ids::ProjectId;
use dibs_domain::member::Role;

use crate::dependencies::Dependencies;
use crate::error::{AppError, Duplicate};
use crate::ports::{Clock, IdGen, Store};
use crate::records::{MemberRecord, ProjectRecord};
use crate::token::SecretToken;
use crate::use_cases::common::{Enrolment, NewMember};
use crate::work::Work;
use dibs_domain::project::ProjectSlug;

/// What a new project starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitProjectInput {
    /// The project's slug.
    pub slug: ProjectSlug,
    /// Its first admin.
    pub admin: NewMember,
}

/// The new project.
#[derive(Debug)]
pub struct InitializedProject {
    /// The project.
    pub project: ProjectRecord,
    /// Its first admin.
    pub admin: MemberRecord,
    /// The admin's token. It is shown once; only its hash is kept.
    pub token: SecretToken,
}

/// Creates a project and its first admin, and issues the admin's token. Run by the operator of the
/// service, before any member exists to authenticate.
pub struct InitProject {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    enrolment: Enrolment,
}

impl InitProject {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            enrolment: Enrolment {
                ids: deps.ids.clone(),
                tokens: deps.tokens.clone(),
                hasher: deps.hasher.clone(),
            },
        }
    }

    /// Creates the project.
    ///
    /// # Errors
    /// [`AppError::Duplicate`] when a project of the slug exists; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(&self, input: InitProjectInput) -> Result<InitializedProject, AppError> {
        let now = self.clock.now();
        let id = ProjectId(self.ids.next());
        let mut work = Work::begin(&*self.store, id).await?;
        if work.project_by_slug(&input.slug).await?.is_some() {
            return Err(AppError::Duplicate(Duplicate::Project(input.slug)));
        }
        let project = ProjectRecord {
            id,
            slug: input.slug,
            created_at: now,
        };
        work.insert_project(&project).await?;
        let admin = self
            .enrolment
            .add_member(&mut work, input.admin, Role::Admin, now)
            .await?;
        let token = self.enrolment.issue_token(&mut work, admin.id, now).await?;
        work.commit_quietly().await?;
        Ok(InitializedProject {
            project,
            admin,
            token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::Fakes;
    use crate::use_cases::testing::{START, new_member};
    use crate::use_cases::{Authenticate, InitProject};

    #[tokio::test]
    async fn a_project_starts_with_an_admin_whose_token_authenticates() {
        let fakes = Fakes::new(START);
        let deps = fakes.dependencies();
        let created = InitProject::new(&deps)
            .execute(InitProjectInput {
                slug: ProjectSlug::parse("widgets").unwrap(),
                admin: new_member("alice"),
            })
            .await
            .unwrap();
        assert_eq!(created.project.slug.as_str(), "widgets");
        assert_eq!(created.admin.role, Role::Admin);
        let actor = Authenticate::new(&deps)
            .execute(created.token.expose())
            .await
            .unwrap();
        assert_eq!(actor.project, created.project.id);
        assert_eq!(actor.member, created.admin.id);
        assert!(fakes.store.all_events().await.is_empty());
        assert!(fakes.notifier.calls().is_empty());
    }

    #[tokio::test]
    async fn a_slug_is_taken_once() {
        let fakes = Fakes::new(START);
        let deps = fakes.dependencies();
        let init = InitProject::new(&deps);
        let input = InitProjectInput {
            slug: ProjectSlug::parse("widgets").unwrap(),
            admin: new_member("alice"),
        };
        init.execute(input.clone()).await.unwrap();
        let error = init.execute(input).await.unwrap_err();
        assert_eq!(error.code(), "project_exists");
    }

    #[tokio::test]
    async fn a_failed_commit_leaves_no_project() {
        let fakes = Fakes::new(START);
        let deps = fakes.dependencies();
        let init = InitProject::new(&deps);
        let input = InitProjectInput {
            slug: ProjectSlug::parse("widgets").unwrap(),
            admin: new_member("alice"),
        };
        fakes.store.fail_next_commit();
        assert_eq!(
            init.execute(input.clone()).await.unwrap_err().code(),
            "internal"
        );
        assert!(init.execute(input).await.is_ok());
    }
}
