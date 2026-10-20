//! An admin ending a claim.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ref_scheme::RefSchemeRegistry;

use dibs_domain::claim::decide_force_release;
use dibs_domain::event::{self, EventKind};
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemStatus;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{EndedClaim, ItemName, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::common::existing_item;
use crate::use_cases::handover::Handovers;
use crate::work::Work;

/// Ends any member's claim on an item at an admin's word; the item is open again. In the same
/// transaction every area another claim waited for it to hand over moves to that claim.
pub struct ForceRelease {
    ref_schemes: Arc<RefSchemeRegistry>,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl ForceRelease {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            ref_schemes: deps.ref_schemes.clone(),
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Ends the claim, answering the claim that ended with its holder's handle. A caller who is no
    /// admin is refused before anything is looked up, so the refusal is the same for every item.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] when the caller is no admin; [`AppError::ForceRelease`] when nobody
    /// holds the item; [`AppError::NotFound`] for an unknown item; [`AppError::Store`] when the
    /// store fails.
    pub async fn execute(&self, actor: &Actor, item: ItemId) -> Result<EndedClaim, AppError> {
        actor.require_admin()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let item = existing_item(&mut work, item).await?;
        let current = work.claim(actor.project, item.id).await?;
        let forced = decide_force_release(current.as_ref(), actor.role)?;
        let ending = Ending {
            status: ItemStatus::Open,
            event: EventKind::ForceReleased(event::ForceReleased {
                item: item.id,
                member: forced.claim.member,
                by: Some(actor.member),
            }),
        };
        let settings = work.project_settings(&self.hot_rules).await?;
        let handovers = Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        end_claim(&mut work, &forced.claim, ending, &handovers).await?;
        let ended = EndedClaim {
            claim: Names::load(&mut work).await?.claim(forced.claim)?,
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
                    kind: None,
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
        for caller in [&env.bob.actor, &env.importer] {
            assert_eq!(
                force
                    .execute(caller, ItemId(9999))
                    .await
                    .unwrap_err()
                    .code(),
                "forbidden"
            );
        }
        let before = env.events().await.len();
        force.execute(&env.alice.actor, item).await.unwrap();
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::ForceReleased(event::ForceReleased {
                item,
                member: env.bob.actor.member,
                by: Some(env.alice.actor.member),
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
