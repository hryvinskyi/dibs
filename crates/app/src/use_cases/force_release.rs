//! An admin ending a claim.

use std::sync::Arc;

use dibs_domain::claim::decide_force_release;
use dibs_domain::event::{self, EventKind};
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemStatus;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedClaim, Names};
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::common::existing_item;
use crate::work::Work;

/// Ends any member's claim on an item at an admin's word; the item is open again.
pub struct ForceRelease {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl ForceRelease {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Ends the claim, answering the claim that ended with its holder's handle.
    ///
    /// # Errors
    /// [`AppError::ForceRelease`] when the caller is no admin or nobody holds the item;
    /// [`AppError::NotFound`] for an unknown item; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, item: ItemId) -> Result<NamedClaim, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let item = existing_item(&mut work, item).await?;
        let current = work.claim(actor.project, item.id).await?;
        let forced = decide_force_release(current.as_ref(), actor.role)?;
        let ending = Ending {
            status: ItemStatus::Open,
            event: EventKind::ForceReleased(event::ForceReleased {
                item: item.id,
                member: forced.claim.member,
                by: actor.member,
            }),
        };
        end_claim(&mut work, &forced.claim, ending, now).await?;
        let ended = Names::load(&mut work).await?.claim(forced.claim)?;
        work.commit(&*self.notifier).await?;
        Ok(ended)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{ClaimItem, ClaimItemInput};

    async fn bob_claims(env: &Env, item: ItemId) {
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
    }

    async fn status(env: &Env, item: ItemId) -> ItemStatus {
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.item(env.project, item).await.unwrap().unwrap().status
    }

    #[tokio::test]
    async fn an_admin_forces_a_release_and_is_named() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        bob_claims(&env, item).await;
        let force = ForceRelease::new(&env.deps);
        assert_eq!(
            force
                .execute(&env.bob.actor, item)
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
        let before = env.events().await.len();
        force.execute(&env.alice.actor, item).await.unwrap();
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::ForceReleased(event::ForceReleased {
                item,
                member: env.bob.actor.member,
                by: env.alice.actor.member,
            })]
        );
        assert_eq!(status(&env, item).await, ItemStatus::Open);
        assert_eq!(
            force
                .execute(&env.alice.actor, item)
                .await
                .unwrap_err()
                .code(),
            "no_claim"
        );
    }
}
