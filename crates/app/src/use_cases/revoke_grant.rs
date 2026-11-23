//! A grantor, or an admin, withdrawing a grant.

use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::event::GrantEnd;
use dibs_domain::grant::{Grant, GrantBlock, Revoker, decide_revoke_grant};
use dibs_domain::ids::GrantId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::named::{NamedGrant, Names};
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::grant_endings::grant_revoked;
use crate::work::Work;

/// Withdraws a grant: its grantor may, and so may an admin of the project, for any grant — one
/// dibs gave on the holder's behalf included. Its record is removed, `grant_revoked` names who
/// revoked it, and it is answered with the handles of its two members.
///
/// A revoke sticks. In the same transaction it settles every request the grantee put to the
/// grantor in the area that is not settled yet, and records a block between the grantee's claim
/// on the grant's item and the grantor's claim that holds the area, each as claimed then, so that
/// dibs never again grants that member that area on the grantor's behalf while both claims last.
/// The grantor may still grant it.
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
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] when the project has no
    /// such grant; [`AppError::Revoke`] when the caller neither gave it nor is an admin;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, grant: GrantId) -> Result<NamedGrant, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let record = work
            .grant(project, grant)
            .await?
            .ok_or(AppError::NotFound(Missing::Grant(grant)))?;
        decide_revoke_grant(
            &record.grant,
            Revoker {
                member: actor.member,
                admin: actor.require_admin().is_ok(),
            },
        )?;
        let claims = work.claims(project).await?;
        work.delete_grant(project, record.id).await?;
        work.record(
            now,
            grant_revoked(&record, GrantEnd::Revoked, Some(actor.member)),
        )
        .await?;
        for block in blocks_of(&record.grant, &claims, now) {
            work.insert_grant_block(project, &block).await?;
        }
        work.settle_asked_grants_between(
            project,
            record.grant.to,
            record.grant.from,
            &record.grant.area,
            now,
        )
        .await?;
        let revoked = Names::load(&mut work).await?.grant(record)?;
        work.commit(&*self.notifier).await?;
        Ok(revoked)
    }
}

/// The blocks a revoke of `grant` at `revoked_at` records: one between the grantee's claim on the
/// grant's item and each claim of the grantor's that holds the area, as the claims stand. None
/// when either side holds no such claim any more.
fn blocks_of(grant: &Grant, claims: &[Claim], revoked_at: OffsetDateTime) -> Vec<GrantBlock> {
    let Some(asker) = claims
        .iter()
        .find(|claim| claim.member == grant.to && claim.item == grant.item)
    else {
        return Vec::new();
    };
    claims
        .iter()
        .filter(|claim| claim.member == grant.from && claim.holds(&grant.area))
        .map(|holder| GrantBlock {
            asker: grant.to,
            asker_item: asker.item,
            asker_claimed_at: asker.claimed_at,
            holder: grant.from,
            holder_item: holder.item,
            holder_claimed_at: holder.claimed_at,
            area: grant.area.clone(),
            revoked_at,
        })
        .collect()
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
                    branches: Default::default(),
                    wide: None,
                    item,
                    session: who.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                    kind: None,
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
                by: Some(by),
                ..
            })] if *by == env.alice.actor.member
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

    #[tokio::test]
    async fn an_admin_revokes_a_grant_they_did_not_give_and_the_revoke_sticks() {
        use dibs_domain::member::Role;

        use crate::use_cases::testing::{new_member, session};
        use crate::use_cases::{Authenticate, CreateMember, CreateMemberInput};

        let env = Env::new().await;
        let token = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: new_member("carol"),
                    role: Role::Member,
                },
            )
            .await
            .unwrap()
            .token;
        let actor = Authenticate::new(&env.deps)
            .execute(token.expose())
            .await
            .unwrap();
        let carol = Person {
            session: session(&env.deps, &actor, "tablet", "tablet/0c3e55d1").await,
            actor,
        };
        let bobs = env.item("gh:acme/widgets#2").await;
        let carols = env.item("gh:acme/widgets#3").await;
        claim(&env, &env.bob, bobs, &["billing"]).await;
        claim(&env, &carol, carols, &[]).await;
        let given = Grant::new(&env.deps)
            .execute(
                &env.bob.actor,
                GrantInput {
                    item: carols,
                    area: "billing".to_owned(),
                    to: Handle::parse("carol").unwrap(),
                },
            )
            .await
            .unwrap()
            .grant;
        let revoke = RevokeGrant::new(&env.deps);
        let before = env.events().await.len();
        let refused = revoke
            .execute(&carol.actor, given.record.id)
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "not_grantor", "neither grantor nor admin");
        assert_eq!(env.events().await.len(), before);

        let revoked = revoke
            .execute(&env.alice.actor, given.record.id)
            .await
            .unwrap();

        assert_eq!(revoked.record.id, given.record.id);
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::GrantRevoked(GrantRevoked {
                grant: given.record.id,
                item: carols,
                area: given.record.grant.area.clone(),
                from: env.bob.actor.member,
                to: carol.actor.member,
                cause: GrantEnd::Revoked,
                by: Some(env.alice.actor.member),
            })]
        );
        let mut tx = env.deps.store.begin().await.unwrap();
        let claims = tx.claims(env.project).await.unwrap();
        let blocks = tx.grant_blocks(env.project).await.unwrap();
        let [block] = &blocks[..] else {
            panic!("one block: {blocks:?}");
        };
        assert_eq!(
            (
                block.asker,
                block.asker_item,
                block.holder,
                block.holder_item
            ),
            (carol.actor.member, carols, env.bob.actor.member, bobs)
        );
        assert!(
            block.applies(
                carol.actor.member,
                env.bob.actor.member,
                &given.record.grant.area,
                &claims
            ),
            "it stands between the two claims as they are"
        );
    }
}
