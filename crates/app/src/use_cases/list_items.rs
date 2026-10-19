//! Finding work items.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::Store;
use dibs_domain::handle::Handle;
use dibs_domain::item::{ExternalRef, ItemStatus};

use crate::named::{NamedItem, Names};
use crate::records::ItemFilter;
use crate::work::Work;

/// Which items to list, as a person asks. Every filter given must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemQuery {
    /// Only items with this status.
    pub status: Option<ItemStatus>,
    /// Only items the member of this handle holds a claim on, live or stale. A handle no member of
    /// the project has matches no item.
    pub holder: Option<Handle>,
    /// Only the item with this reference.
    pub external_ref: Option<ExternalRef>,
}

/// Lists the project's items matching a query, ordered by id, each with its creator's handle. A
/// filter on the reference is an exact lookup: it answers the one item of that reference, or none.
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
        query: ItemQuery,
    ) -> Result<Vec<NamedItem>, AppError> {
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let names = Names::load(&mut work).await?;
        let holder = match &query.holder {
            None => None,
            Some(handle) => match names.member(handle) {
                Some(member) => Some(member),
                None => return Ok(Vec::new()),
            },
        };
        let filter = ItemFilter {
            status: query.status,
            holder,
            external_ref: query.external_ref,
        };
        work.items(actor.project, &filter)
            .await?
            .into_iter()
            .map(|item| names.item(item))
            .collect()
    }
}

#[cfg(test)]
mod tests {

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
        let ids = |items: Vec<NamedItem>| {
            items
                .into_iter()
                .map(|named| named.item.id)
                .collect::<Vec<_>>()
        };
        let all = list
            .execute(&env.bob.actor, ItemQuery::default())
            .await
            .unwrap();
        assert_eq!(ids(all), [open, held, done]);
        let by_status = |status| ItemQuery {
            status: Some(status),
            ..ItemQuery::default()
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
        let held_by = |handle: &str| ItemQuery {
            holder: Some(Handle::parse(handle).unwrap()),
            ..ItemQuery::default()
        };
        assert_eq!(
            ids(list
                .execute(&env.alice.actor, held_by("bob"))
                .await
                .unwrap()),
            [held]
        );
        assert!(
            ids(list
                .execute(&env.alice.actor, held_by("nobody"))
                .await
                .unwrap())
            .is_empty(),
            "a handle no member has matches nothing"
        );
        let by_ref = ItemQuery {
            external_ref: Some(ExternalRef::parse("gh:acme/widgets#2").unwrap()),
            ..ItemQuery::default()
        };
        let found = list.execute(&env.alice.actor, by_ref).await.unwrap();
        assert_eq!(found[0].created_by.as_str(), "alice");
        assert_eq!(ids(found), [held]);
    }
}
