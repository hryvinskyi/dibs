//! What the caller is: their member record, their project, its settings.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::settings::ProjectSettings;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Store, StoreError};
use crate::records::{MemberRecord, ProjectRecord};
use crate::work::Work;

/// The caller as the service knows them.
#[derive(Debug, Clone)]
pub struct MeView {
    /// The caller.
    pub member: MemberRecord,
    /// Their project.
    pub project: ProjectRecord,
    /// The project's settings.
    pub settings: ProjectSettings,
}

/// Answers who the caller is and in which project.
pub struct Me {
    store: Arc<dyn Store>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl Me {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// The caller's member record, project and settings.
    ///
    /// # Errors
    /// [`AppError::Unauthorized`] when the member is gone; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<MeView, AppError> {
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let member = work
            .member(actor.project, actor.member)
            .await?
            .ok_or(AppError::Unauthorized)?;
        let project = work
            .project(actor.project)
            .await?
            .ok_or_else(|| StoreError::Corrupt {
                what: format!("project {} of member {}", actor.project, actor.member),
            })?;
        let settings = work.project_settings(&self.hot_rules).await?;
        Ok(MeView {
            member,
            project,
            settings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;

    #[tokio::test]
    async fn the_caller_sees_themselves_their_project_and_its_settings() {
        let env = Env::new().await;
        let me = Me::new(&env.deps).execute(&env.bob.actor).await.unwrap();
        assert_eq!(me.member.handle.as_str(), "bob");
        assert_eq!(me.project.slug.as_str(), "widgets");
        assert_eq!(me.settings.repos()[0].areas.len(), 2);
    }
}
