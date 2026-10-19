//! A grantor withdrawing a grant.

use std::sync::Arc;

use dibs_domain::event::GrantEnd;
use dibs_domain::grant::decide_revoke_grant;
use dibs_domain::ids::GrantId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::named::{NamedGrant, Names};
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::grant_endings::grant_revoked;
use crate::work::Work;

/// Withdraws a grant the caller gave; its record is removed and answered with the handles of its
/// two members.
pub struct RevokeGrant {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl RevokeGrant {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Withdraws the grant.
    ///
    /// # Errors
    /// [`AppError::NotFound`] when the project has no such grant; [`AppError::Revoke`] when the
    /// caller did not give it; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, grant: GrantId) -> Result<NamedGrant, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let record = work
            .grant(actor.project, grant)
            .await?
            .ok_or(AppError::NotFound(Missing::Grant(grant)))?;
        decide_revoke_grant(&record.grant, actor.member)?;
        work.delete_grant(actor.project, record.id).await?;
        work.record(now, grant_revoked(&record, GrantEnd::Revoked))
            .await?;
        let revoked = Names::load(&mut work).await?.grant(record)?;
        work.commit(&*self.notifier).await?;
        Ok(revoked)
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::event::{EventKind, GrantRevoked};
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::ItemId;

    use super::*;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::{ClaimItem, ClaimItemInput, Grant, GrantInput};

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
    async fn only_the_grantor_revokes_and_the_grant_is_gone() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        let given = Grant::new(&env.deps)
            .execute(
                &env.alice.actor,
                GrantInput {
                    item: bobs,
                    area: "billing".to_owned(),
                    to: Handle::parse("bob").unwrap(),
                },
            )
            .await
            .unwrap()
            .grant;
        let revoke = RevokeGrant::new(&env.deps);
        assert_eq!(
            revoke
                .execute(&env.bob.actor, given.record.id)
                .await
                .unwrap_err()
                .code(),
            "not_grantor"
        );
        let before = env.events().await.len();
        revoke
            .execute(&env.alice.actor, given.record.id)
            .await
            .unwrap();
        let kinds = env.kinds_after(before).await;
        assert!(matches!(
            kinds.as_slice(),
            [EventKind::GrantRevoked(GrantRevoked {
                cause: GrantEnd::Revoked,
                ..
            })]
        ));
        let mut tx = env.deps.store.begin().await.unwrap();
        assert!(tx.grants(env.project).await.unwrap().is_empty());
        drop(tx);
        assert_eq!(
            revoke
                .execute(&env.alice.actor, given.record.id)
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
    }
}
