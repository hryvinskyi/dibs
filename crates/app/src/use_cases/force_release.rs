//! An admin ending a claim.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ref_scheme::RefSchemeRegistry;

use dibs_domain::claim::decide_force_release;
use dibs_domain::event::{self, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemStatus;
use dibs_domain::message::Importance;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{EndedClaim, ItemName, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::claim_notice::ClaimNotice;
use crate::use_cases::common::existing_item;
use crate::use_cases::handover::Handovers;
use crate::use_cases::writing::titled_body;
use crate::work::Work;

/// Ends any member's claim on an item at an admin's word; the item is open again. In the same
/// transaction every area another claim waited for it to hand over moves to that claim, and the
/// holder is told in the claim's conversation, as a `high` message, who ended it and that they
/// may claim the item again.
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
        let released = event::ForceReleased {
            item: item.id,
            member: forced.claim.member,
            by: Some(actor.member),
        };
        let ending = Ending {
            status: ItemStatus::Open,
            event: EventKind::ForceReleased(released.clone()),
        };
        let settings = work.project_settings(&self.hot_rules).await?;
        let handovers = Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        end_claim(&mut work, &forced.claim, ending, &handovers).await?;
        let names = Names::load(&mut work).await?;
        let named = ItemName::of(&item, &self.ref_schemes);
        let by = released.by.map(|admin| names.handle(admin)).transpose()?;
        ClaimNotice {
            claim: &forced.claim,
            item: &named,
            body: titled_body(&force_released(by.as_ref(), &named), None)?,
            importance: Importance::High,
        }
        .send(&mut work, &*self.ids, now)
        .await?;
        let ended = EndedClaim {
            claim: names.claim(forced.claim)?,
            item: named,
        };
        work.commit(&*self.notifier).await?;
        Ok(ended)
    }
}

/// What the holder of a claim an admin ended is told: who ended it — the admin by handle, or the
/// server's operator when no admin is named — that the item is open again, and that they claim it
/// again if they still work on it.
fn force_released(by: Option<&Handle>, item: &ItemName) -> String {
    let who = by.map_or_else(
        || "the operator of this server".to_owned(),
        |admin| format!("{admin}, an admin,"),
    );
    format!(
        "Released: {who} ended your claim on work item {label} \"{title}\". The item is open again and its areas are free. If you still work on it, claim it again (dibs claim {key}).",
        label = item.label(),
        title = item.title,
        key = item.key(),
    )
}

#[cfg(test)]
mod tests {
    use dibs_domain::item::{ExternalRef, Title};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{ClaimItem, ClaimItemInput};

    async fn bob_claims(env: &Env, item: ItemId) {
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    branches: Default::default(),
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
        let kinds = env.kinds_after(before).await;
        assert_eq!(
            kinds[0],
            EventKind::ForceReleased(event::ForceReleased {
                item,
                member: env.bob.actor.member,
                by: Some(env.alice.actor.member),
            })
        );
        assert!(
            matches!(
                &kinds[1..],
                [EventKind::ThreadStarted(started), EventKind::MessagePosted(_)]
                    if started.item == Some(item) && started.author.is_none()
            ),
            "the holder is told: {kinds:?}"
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

    fn named(reference: Option<&str>) -> ItemName {
        ItemName {
            id: ItemId(7),
            external_ref: reference.map(|text| ExternalRef::parse(text).unwrap()),
            title: Title::parse("Refund rounding").unwrap(),
            link: None,
        }
    }

    #[test]
    fn the_holder_is_told_who_ended_the_claim_and_how_to_take_it_again() {
        let alice = Handle::parse("alice").unwrap();
        assert_eq!(
            force_released(Some(&alice), &named(Some("gh:acme/widgets#42"))),
            "Released: alice, an admin, ended your claim on work item gh:acme/widgets#42 \"Refund rounding\". The item is open again and its areas are free. If you still work on it, claim it again (dibs claim gh:acme/widgets#42)."
        );
        let by_operator = force_released(None, &named(None));
        assert!(
            by_operator.starts_with(
                "Released: the operator of this server ended your claim on work item #7 \"Refund rounding\"."
            ),
            "{by_operator}"
        );
        assert!(by_operator.ends_with("(dibs claim 7)."), "{by_operator}");
    }
}
