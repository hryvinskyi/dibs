//! One work item and its claim.

use std::sync::Arc;

use dibs_domain::ids::ItemId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedClaim, NamedItem, Names};
use crate::ports::Store;
use crate::use_cases::common::existing_item;
use crate::work::Work;

/// A work item with whoever holds it, each member with their handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemView {
    /// The item.
    pub item: NamedItem,
    /// Its claim, live or stale.
    pub claim: Option<NamedClaim>,
}

/// Shows one work item of the caller's project.
pub struct GetItem {
    store: Arc<dyn Store>,
}

impl GetItem {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The item and its claim.
    ///
    /// # Errors
    /// [`AppError::NotFound`] when the project has no such item; [`AppError::Store`] when the store
    /// fails.
    pub async fn execute(&self, actor: &Actor, item: ItemId) -> Result<ItemView, AppError> {
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let item = existing_item(&mut work, item).await?;
        let claim = work.claim(actor.project, item.id).await?;
        let names = Names::load(&mut work).await?;
        Ok(ItemView {
            item: names.item(item)?,
            claim: claim.map(|claim| names.claim(claim)).transpose()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{ClaimItem, ClaimItemInput, InitProject, InitProjectInput};
    use dibs_domain::project::ProjectSlug;

    #[tokio::test]
    async fn an_item_shows_its_holder() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    item,
                    session: env.bob.session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        let view = GetItem::new(&env.deps)
            .execute(&env.alice.actor, item)
            .await
            .unwrap();
        assert_eq!(view.item.item.id, item);
        assert_eq!(view.item.created_by.as_str(), "alice");
        let claim = view.claim.unwrap();
        assert_eq!(claim.claim.member, env.bob.actor.member);
        assert_eq!(claim.holder.as_str(), "bob");
    }

    #[tokio::test]
    async fn an_item_of_another_project_is_not_found() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let other = InitProject::new(&env.deps)
            .execute(InitProjectInput {
                slug: ProjectSlug::parse("gadgets").unwrap(),
                admin: crate::use_cases::testing::new_member("dave"),
            })
            .await
            .unwrap();
        let dave = crate::use_cases::Authenticate::new(&env.deps)
            .execute(other.token.expose())
            .await
            .unwrap();
        let error = GetItem::new(&env.deps)
            .execute(&dave, item)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
    }
}
