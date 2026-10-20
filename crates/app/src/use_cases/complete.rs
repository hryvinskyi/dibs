//! Completing a claim.

use std::sync::Arc;

use dibs_domain::ref_scheme::RefSchemeRegistry;

use dibs_domain::claim::decide_complete;
use dibs_domain::event::{self, EventKind};
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemStatus;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{EndedClaim, ItemName, Names, refuse};
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::common::existing_item;
use crate::work::Work;

/// Completes the caller's claim on an item; the item is done.
pub struct Complete {
    ref_schemes: Arc<RefSchemeRegistry>,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl Complete {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            ref_schemes: deps.ref_schemes.clone(),
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Completes the claim, answering the claim that ended with its holder's handle.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item; [`AppError::HolderOnly`] when nobody or another
    /// member holds it; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, item: ItemId) -> Result<EndedClaim, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let item = existing_item(&mut work, item).await?;
        let current = work.claim(actor.project, item.id).await?;
        let completed = match decide_complete(current.as_ref(), actor.member) {
            Ok(completed) => completed,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let ending = Ending {
            status: ItemStatus::Done,
            event: EventKind::Completed(event::Completed {
                item: item.id,
                member: completed.claim.member,
            }),
        };
        end_claim(&mut work, &completed.claim, ending, now).await?;
        let ended = EndedClaim {
            claim: Names::load(&mut work).await?.claim(completed.claim)?,
            item: ItemName::of(&item, &self.ref_schemes),
        };
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
                    wide: None,
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
    async fn the_holder_completes_and_the_item_is_done() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        bob_claims(&env, item).await;
        let before = env.events().await.len();
        Complete::new(&env.deps)
            .execute(&env.bob.actor, item)
            .await
            .unwrap();
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::Completed(event::Completed {
                item,
                member: env.bob.actor.member
            })]
        );
        assert_eq!(status(&env, item).await, ItemStatus::Done);
    }
}
