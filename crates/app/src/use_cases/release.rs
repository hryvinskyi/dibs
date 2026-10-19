//! Releasing a claim.

use std::sync::Arc;

use dibs_domain::claim::{Released, decide_release};
use dibs_domain::event::{self, EventKind};
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemStatus;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::common::existing_item;
use crate::work::Work;

/// Releases the caller's claim on an item, live or stale; the item is open again.
pub struct Release {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl Release {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Releases the claim.
    ///
    /// # Errors
    /// [`AppError::NotFound`] for an unknown item; [`AppError::HolderOnly`] when nobody or another
    /// member holds it; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, item: ItemId) -> Result<Released, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let item = existing_item(&mut work, item).await?;
        let current = work.claim(actor.project, item.id).await?;
        let released = decide_release(current.as_ref(), actor.member)?;
        let ending = Ending {
            status: ItemStatus::Open,
            event: EventKind::Released(event::Released {
                item: item.id,
                member: released.claim.member,
            }),
        };
        end_claim(&mut work, &released.claim, ending, now).await?;
        work.commit(&*self.notifier).await?;
        Ok(released)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::area::QualifiedArea;
    use dibs_domain::event::{GrantEnd, GrantRevoked};
    use dibs_domain::handle::Handle;
    use dibs_domain::paths::RepoPath;

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        ClaimItem, ClaimItemInput, Complete, EditCheck, EditCheckInput, Grant, GrantInput,
    };

    async fn claim(
        env: &Env,
        who: &crate::use_cases::testing::Person,
        item: ItemId,
        areas: &[&str],
    ) {
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

    async fn status(env: &Env, item: ItemId) -> ItemStatus {
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.item(env.project, item).await.unwrap().unwrap().status
    }

    #[tokio::test]
    async fn the_holder_releases_and_the_item_is_open_again() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, &env.bob, item, &["billing"]).await;
        EditCheck::new(&env.deps)
            .execute(
                &env.bob.actor,
                EditCheckInput {
                    session: env.bob.session,
                    paths: vec![RepoPath::parse("default:config/app.yaml").unwrap()],
                    reserve: true,
                },
            )
            .await
            .unwrap();
        env.advance(Duration::hours(30));
        let before = env.events().await.len();
        let released = Release::new(&env.deps)
            .execute(&env.bob.actor, item)
            .await
            .unwrap();
        assert_eq!(released.claim.member, env.bob.actor.member);
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::Released(event::Released {
                item,
                member: env.bob.actor.member
            })]
        );
        assert_eq!(status(&env, item).await, ItemStatus::Open);
        let mut tx = env.deps.store.begin().await.unwrap();
        assert_eq!(tx.claim(env.project, item).await.unwrap(), None);
        assert!(tx.shared_paths(env.project).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_the_holder_releases_or_completes() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let release = Release::new(&env.deps);
        let complete = Complete::new(&env.deps);
        assert_eq!(
            release
                .execute(&env.bob.actor, item)
                .await
                .unwrap_err()
                .code(),
            "no_claim"
        );
        claim(&env, &env.bob, item, &[]).await;
        let before = env.events().await.len();
        assert_eq!(
            release
                .execute(&env.alice.actor, item)
                .await
                .unwrap_err()
                .code(),
            "not_holder"
        );
        assert_eq!(
            complete
                .execute(&env.alice.actor, item)
                .await
                .unwrap_err()
                .code(),
            "not_holder"
        );
        assert_eq!(
            release
                .execute(&env.alice.actor, ItemId(9999))
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(env.events().await.len(), before);
    }

    #[tokio::test]
    async fn a_released_claim_ends_the_grants_given_to_it() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        let granted = Grant::new(&env.deps)
            .execute(
                &env.alice.actor,
                GrantInput {
                    item: bobs,
                    area: "billing".to_owned(),
                    to: Handle::parse("bob").unwrap(),
                },
            )
            .await
            .unwrap();
        let before = env.events().await.len();
        Release::new(&env.deps)
            .execute(&env.bob.actor, bobs)
            .await
            .unwrap();
        let kinds = env.kinds_after(before).await;
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert_eq!(
            kinds[1],
            EventKind::GrantRevoked(GrantRevoked {
                grant: granted.grant.id,
                item: bobs,
                area: QualifiedArea::parse("default:billing").unwrap(),
                from: env.alice.actor.member,
                to: env.bob.actor.member,
                cause: GrantEnd::GranteeLeft,
            })
        );
    }

    #[tokio::test]
    async fn a_failed_commit_releases_nothing_and_wakes_nobody() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        claim(&env, &env.bob, item, &[]).await;
        let calls = env.fakes.notifier.calls().len();
        let before = env.events().await.len();
        env.fakes.store.fail_next_commit();
        let error = Release::new(&env.deps)
            .execute(&env.bob.actor, item)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "internal");
        assert_eq!(env.events().await.len(), before);
        assert_eq!(env.fakes.notifier.calls().len(), calls);
        assert_eq!(status(&env, item).await, ItemStatus::Claimed);
    }
}
