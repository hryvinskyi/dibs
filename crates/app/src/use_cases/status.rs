//! Who holds what right now.

use std::sync::Arc;

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, MemberId};
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedClaim, NamedItem, Names};
use crate::ports::{Clock, Store, StoreError};
use crate::use_cases::ready::{ReadyScope, ReadyView};
use crate::work::Work;

/// A claim with its item, each member with their handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldItem {
    /// The claim.
    pub claim: NamedClaim,
    /// The item.
    pub item: NamedItem,
}

/// An area and the live claim holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaHold {
    /// The area.
    pub area: QualifiedArea,
    /// Its holder.
    pub member: MemberId,
    /// The holder's handle.
    pub holder: Handle,
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
    /// The work the caller can pick up now, and the open items that are not ready.
    pub ready: ReadyView,
}

/// Shows the project's live and stale claims, the areas held, and the caller's ready list.
pub struct Status {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl Status {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// The project's status now.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<StatusView, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let stale_after = settings.stale_after();
        let names = Names::load(&mut work).await?;
        let ready = ReadyScope {
            me: actor.member,
            now,
            settings: &settings,
            schemes: &self.ref_schemes,
        }
        .view(&mut work, &names)
        .await?;
        let mut view = StatusView {
            live: Vec::new(),
            stale: Vec::new(),
            holds: Vec::new(),
            ready,
        };
        for claim in work.claims(actor.project).await? {
            let item =
                work.item(actor.project, claim.item)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt {
                        what: format!("claim on item {} that does not exist", claim.item),
                    })?;
            let held = HeldItem {
                claim: names.claim(claim)?,
                item: names.item(item)?,
            };
            let claim = &held.claim;
            if claim.claim.is_live(now, stale_after) {
                view.holds
                    .extend(claim.claim.areas.iter().map(|area| AreaHold {
                        area: area.clone(),
                        member: claim.claim.member,
                        holder: claim.holder.clone(),
                        item: claim.claim.item,
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
        let ids = |held: &[HeldItem]| held.iter().map(|h| h.item.item.id).collect::<Vec<_>>();
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
        assert!(view.holds.iter().all(|h| h.holder.as_str() == "bob"));
        assert_eq!(view.live[0].claim.holder.as_str(), "bob");
    }
}
