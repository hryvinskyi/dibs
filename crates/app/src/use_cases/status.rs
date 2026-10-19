//! Who holds what right now.

use std::sync::Arc;

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::Claim;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, MemberId};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store, StoreError};
use crate::records::ItemRecord;
use crate::work::Work;

/// A claim with its item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldItem {
    /// The claim.
    pub claim: Claim,
    /// The item.
    pub item: ItemRecord,
}

/// An area and the live claim holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaHold {
    /// The area.
    pub area: QualifiedArea,
    /// Its holder.
    pub member: MemberId,
    /// The item of the holder's claim.
    pub item: ItemId,
}

/// The project's claims and holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusView {
    /// Live claims, ordered by item.
    pub live: Vec<HeldItem>,
    /// Stale claims, ordered by item: they hold nothing and wait for their holder or a takeover.
    pub stale: Vec<HeldItem>,
    /// The areas live claims hold, ordered by area.
    pub holds: Vec<AreaHold>,
}

/// Shows the project's live and stale claims and the areas held.
pub struct Status {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl Status {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// The project's status now.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<StatusView, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let stale_after = work.project_settings(&self.hot_rules).await?.stale_after();
        let mut view = StatusView {
            live: Vec::new(),
            stale: Vec::new(),
            holds: Vec::new(),
        };
        for claim in work.claims(actor.project).await? {
            let item =
                work.item(actor.project, claim.item)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt {
                        what: format!("claim on item {} that does not exist", claim.item),
                    })?;
            let held = HeldItem { claim, item };
            if held.claim.is_live(now, stale_after) {
                view.holds
                    .extend(held.claim.areas.iter().map(|area| AreaHold {
                        area: area.clone(),
                        member: held.claim.member,
                        item: held.claim.item,
                    }));
                view.live.push(held);
            } else {
                view.stale.push(held);
            }
        }
        view.holds
            .sort_by(|a, b| (&a.area, a.item).cmp(&(&b.area, b.item)));
        Ok(view)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::{ClaimItem, ClaimItemInput};

    async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str]) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    item,
                    session: who.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn live_and_stale_claims_and_holds_are_told_apart() {
        let env = Env::new().await;
        let old = env.item("gh:acme/widgets#1").await;
        let fresh = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, old, &["search"]).await;
        env.advance(Duration::hours(25));
        claim(&env, &env.bob, fresh, &["billing", "search"]).await;
        let view = Status::new(&env.deps)
            .execute(&env.alice.actor)
            .await
            .unwrap();
        let ids = |held: &[HeldItem]| held.iter().map(|h| h.item.id).collect::<Vec<_>>();
        assert_eq!(ids(&view.live), [fresh]);
        assert_eq!(ids(&view.stale), [old]);
        let holds: Vec<(String, ItemId)> = view
            .holds
            .iter()
            .map(|h| (h.area.to_string(), h.item))
            .collect();
        assert_eq!(
            holds,
            [
                ("default:billing".to_owned(), fresh),
                ("default:search".to_owned(), fresh)
            ]
        );
        assert!(view.holds.iter().all(|h| h.member == env.bob.actor.member));
    }
}
