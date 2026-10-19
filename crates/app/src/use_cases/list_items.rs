//! Finding work items.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::Store;
use crate::records::{ItemFilter, ItemRecord};
use crate::work::Work;

/// Lists the project's items matching a filter, ordered by id. A filter on the reference is an exact
/// lookup: it answers the one item of that reference, or none.
pub struct ListItems {
    store: Arc<dyn Store>,
}

impl ListItems {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The matching items.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        filter: ItemFilter,
    ) -> Result<Vec<ItemRecord>, AppError> {
        let mut work = Work::begin(&*self.store, actor.project).await?;
        Ok(work.items(actor.project, &filter).await?)
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::item::{ExternalRef, ItemStatus};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{ClaimItem, ClaimItemInput, Complete};

    #[tokio::test]
    async fn items_filter_by_status_holder_and_reference() {
        let env = Env::new().await;
        let open = env.item("gh:acme/widgets#1").await;
        let held = env.item("gh:acme/widgets#2").await;
        let done = env.item("gh:acme/widgets#3").await;
        let claim = ClaimItem::new(&env.deps);
        for item in [held, done] {
            claim
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
        }
        Complete::new(&env.deps)
            .execute(&env.bob.actor, done)
            .await
            .unwrap();
        let list = ListItems::new(&env.deps);
        let ids = |items: Vec<ItemRecord>| items.into_iter().map(|i| i.id).collect::<Vec<_>>();
        let all = list
            .execute(&env.bob.actor, ItemFilter::default())
            .await
            .unwrap();
        assert_eq!(ids(all), [open, held, done]);
        let by_status = |status| ItemFilter {
            status: Some(status),
            ..ItemFilter::default()
        };
        assert_eq!(
            ids(list
                .execute(&env.bob.actor, by_status(ItemStatus::Open))
                .await
                .unwrap()),
            [open]
        );
        assert_eq!(
            ids(list
                .execute(&env.bob.actor, by_status(ItemStatus::Claimed))
                .await
                .unwrap()),
            [held]
        );
        assert_eq!(
            ids(list
                .execute(&env.bob.actor, by_status(ItemStatus::Done))
                .await
                .unwrap()),
            [done]
        );
        let held_by_bob = ItemFilter {
            holder: Some(env.bob.actor.member),
            ..ItemFilter::default()
        };
        assert_eq!(
            ids(list.execute(&env.alice.actor, held_by_bob).await.unwrap()),
            [held]
        );
        let by_ref = ItemFilter {
            external_ref: Some(ExternalRef::parse("gh:acme/widgets#2").unwrap()),
            ..ItemFilter::default()
        };
        assert_eq!(
            ids(list.execute(&env.alice.actor, by_ref).await.unwrap()),
            [held]
        );
    }
}
