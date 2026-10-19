//! The members of the caller's project.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::Store;
use crate::records::MemberRecord;
use crate::work::Work;

/// Lists the members of the caller's project, ordered by handle.
pub struct ListMembers {
    store: Arc<dyn Store>,
}

impl ListMembers {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The members.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<Vec<MemberRecord>, AppError> {
        let mut work = Work::begin(&*self.store, actor.project).await?;
        Ok(work.members(actor.project).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{InitProject, InitProjectInput};
    use dibs_domain::project::ProjectSlug;

    #[tokio::test]
    async fn members_of_another_project_are_not_listed() {
        let env = Env::new().await;
        InitProject::new(&env.deps)
            .execute(InitProjectInput {
                slug: ProjectSlug::parse("gadgets").unwrap(),
                admin: crate::use_cases::testing::new_member("dave"),
            })
            .await
            .unwrap();
        let members = ListMembers::new(&env.deps)
            .execute(&env.alice.actor)
            .await
            .unwrap();
        let handles: Vec<&str> = members.iter().map(|m| m.handle.as_str()).collect();
        assert_eq!(handles, ["alice", "bob", "importer"]);
    }
}
