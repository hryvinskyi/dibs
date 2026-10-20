//! The live claims of a project, as an integration reads them to know which branches to measure.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedClaim, Names};
use crate::ports::{Clock, Store};
use crate::work::Work;

/// Lists every live claim with its holder's handle, ordered by item. A claim says where its work
/// stands and the branch its work is on in each repository; it names no path and no conversation,
/// so every role may read the list, an integration included.
pub struct ListClaims {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl ListClaims {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// The live claims now.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<Vec<NamedClaim>, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let stale_after = work.project_settings(&self.hot_rules).await?.stale_after();
        let claims = work.claims(actor.project).await?;
        let names = Names::load(&mut work).await?;
        let live = claims
            .into_iter()
            .filter(|claim| claim.is_live(now, stale_after))
            .map(|claim| names.claim(claim))
            .collect::<Result<Vec<_>, _>>()?;
        work.commit_quietly().await?;
        Ok(live)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{ClaimItem, ClaimItemInput};

    #[tokio::test]
    async fn every_role_reads_the_live_claims_and_a_stale_one_is_left_out() {
        let env = Env::new().await;
        let first = env.item("gh:acme/widgets#1").await;
        let second = env.item("gh:acme/widgets#2").await;
        for (who, item) in [(&env.alice, first), (&env.bob, second)] {
            ClaimItem::new(&env.deps)
                .execute(
                    &who.actor,
                    ClaimItemInput {
                        wide: None,
                        item,
                        session: who.session,
                        areas: Vec::new(),
                        kind: None,
                    },
                )
                .await
                .unwrap();
            env.advance(Duration::hours(13));
        }
        let list = ListClaims::new(&env.deps);
        for actor in [&env.importer, &env.bob.actor] {
            let listed = list.execute(actor).await.unwrap();
            let items: Vec<_> = listed.iter().map(|claim| claim.claim.item).collect();
            assert_eq!(items, [second], "alice's claim went stale");
            assert_eq!(listed[0].holder.as_str(), "bob");
        }
    }
}
