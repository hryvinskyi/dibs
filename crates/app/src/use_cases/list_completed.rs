//! The items completed since a moment.

use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::handle::Handle;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedItem, Names};
use crate::ports::{Store, StoreError};
use crate::work::Work;

/// The most completions one call answers.
pub const MAX_COMPLETED: usize = 200;

/// An item completed, with the handle of the member whose claim completed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedItem {
    /// The item, with its creator's handle.
    pub item: NamedItem,
    /// The handle of the member who completed it.
    pub member: Handle,
    /// When.
    pub at: OffsetDateTime,
}

/// Lists the items of the caller's project completed at or after a moment, newest first, at most
/// [`MAX_COMPLETED`] of them. Items and handles are read in the same transaction as the log.
pub struct ListCompleted {
    store: Arc<dyn Store>,
}

impl ListCompleted {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The items completed since `since`.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails, or when the log names an item or a member the
    /// project does not have.
    pub async fn execute(
        &self,
        actor: &Actor,
        since: OffsetDateTime,
    ) -> Result<Vec<CompletedItem>, AppError> {
        let project = actor.project;
        let mut work = Work::begin(&*self.store, project).await?;
        let completions = work
            .completions_since(project, since, MAX_COMPLETED)
            .await?;
        let names = Names::load(&mut work).await?;
        let mut completed = Vec::with_capacity(completions.len());
        for completion in completions {
            let item =
                work.item(project, completion.item)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt {
                        what: format!(
                            "a completion of item {} that does not exist",
                            completion.item
                        ),
                    })?;
            completed.push(CompletedItem {
                item: names.item(item)?,
                member: names.handle(completion.member)?,
                at: completion.at,
            });
        }
        Ok(completed)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::use_cases::testing::{Env, Person, START};
    use crate::use_cases::{ClaimItem, ClaimItemInput, Complete};

    async fn complete(env: &Env, who: &Person, reference: &str) -> dibs_domain::ids::ItemId {
        let item = env.item(reference).await;
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    wide: None,
                    item,
                    session: who.session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        Complete::new(&env.deps)
            .execute(&who.actor, item)
            .await
            .unwrap();
        item
    }

    #[tokio::test]
    async fn completions_since_a_moment_come_newest_first_with_their_names() {
        let env = Env::new().await;
        let early = complete(&env, &env.alice, "gh:acme/widgets#1").await;
        env.advance(Duration::hours(2));
        let first = complete(&env, &env.bob, "gh:acme/widgets#2").await;
        env.advance(Duration::minutes(5));
        let second = complete(&env, &env.alice, "gh:acme/widgets#3").await;
        let list = ListCompleted::new(&env.deps);
        let since = START + Duration::hours(1);
        let found = list.execute(&env.bob.actor, since).await.unwrap();
        let seen: Vec<_> = found
            .iter()
            .map(|done| (done.item.item.id, done.member.as_str(), done.at))
            .collect();
        assert_eq!(
            seen,
            [
                (second, "alice", START + Duration::minutes(125)),
                (first, "bob", START + Duration::hours(2)),
            ]
        );
        assert_eq!(found[0].item.created_by.as_str(), "alice");
        let all = list.execute(&env.bob.actor, START).await.unwrap();
        assert_eq!(all.last().map(|done| done.item.item.id), Some(early));
        assert!(
            list.execute(&env.bob.actor, START + Duration::days(1))
                .await
                .unwrap()
                .is_empty()
        );
    }
}
