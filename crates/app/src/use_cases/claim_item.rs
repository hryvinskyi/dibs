//! Claiming a work item.

use std::collections::BTreeMap;
use std::sync::Arc;

use dibs_domain::claim::{ClaimOutcome, ClaimRequest, decide_claim};
use dibs_domain::dependency::blockers;
use dibs_domain::event::{Claimed, EventKind, Renewed};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::item::ItemStatus;
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, Names, refuse};
use crate::ports::{Clock, EventNotifier, Store};
use crate::records::ItemRecord;
use crate::use_cases::common::{existing_item, own_session, resolve_areas};
use crate::use_cases::grant_endings::end_orphaned_grants;
use crate::work::Work;

/// The claim to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimItemInput {
    /// The item, by local id.
    pub item: ItemId,
    /// The caller's session asking.
    pub session: SessionId,
    /// The areas to declare, as a person names them: `repo:area`, or a bare name exactly one
    /// repository declares.
    pub areas: Vec<String>,
}

/// What a claim did, with the claiming member's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimMade {
    /// The new or renewed claim.
    pub outcome: ClaimOutcome,
    /// The handle of the claim's member, the caller.
    pub holder: dibs_domain::handle::Handle,
    /// The items the claimed item waits for that are neither done nor closed. A claim on a blocked
    /// item is made all the same — dependent work is often started in parallel — and the caller
    /// shows these as a warning.
    pub blocked_by: Vec<ItemName>,
}

/// Claims a work item for the caller, or renews the caller's own claim on it.
///
/// The item's claim and the project's other claims are loaded and decided on inside one exclusive
/// transaction, so of two members racing for one item, or for one area, exactly one wins and the
/// other learns who holds it. A closed item cannot be claimed. A renewal that loses areas to another
/// member ends the grants the caller gave in them. A blocked item is claimed, and the answer names
/// what blocks it.
pub struct ClaimItem {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl ClaimItem {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Makes the claim.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item or a
    /// session not the caller's; [`AppError::ItemClosed`] for a done or dropped item;
    /// [`AppError::Area`] for an area that does not resolve; [`AppError::Claim`] when another
    /// member holds the item or a declared area, naming that member; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: ClaimItemInput,
    ) -> Result<ClaimMade, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin(&*self.store, project).await?;
        let session = own_session(&mut work, actor, input.session).await?;
        let item = existing_item(&mut work, input.item).await?;
        if item.is_closed() {
            return Err(AppError::ItemClosed {
                status: item.status,
            });
        }
        let settings = work.project_settings(&self.hot_rules).await?;
        let request = ClaimRequest {
            item: item.id,
            by: actor.member,
            session: session.id,
            host: session.host,
            areas: resolve_areas(&settings, &input.areas)?,
        };
        let current = work.claim(project, item.id).await?;
        let others = work.claims(project).await?;
        let outcome = match decide_claim(current.as_ref(), &others, &request, now, &settings) {
            Ok(outcome) => outcome,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let claim = outcome.claim();
        work.save_claim(project, claim).await?;
        let event = match &outcome {
            ClaimOutcome::New { claim } => {
                work.set_item_status(project, item.id, ItemStatus::Claimed)
                    .await?;
                EventKind::Claimed(Claimed {
                    item: claim.item,
                    member: claim.member,
                    session: claim.session,
                    host: claim.host.clone(),
                    areas: claim.areas.clone(),
                })
            }
            ClaimOutcome::Renewed { claim, .. } => EventKind::Renewed(Renewed {
                item: claim.item,
                member: claim.member,
                session: claim.session,
                host: claim.host.clone(),
                areas: claim.areas.clone(),
            }),
        };
        work.record(now, event).await?;
        end_orphaned_grants(&mut work, now).await?;
        let holder = Names::load(&mut work).await?.handle(actor.member)?;
        let blocked_by = self.blockers_of(&mut work, item.id).await?;
        work.commit(&*self.notifier).await?;
        Ok(ClaimMade {
            outcome,
            holder,
            blocked_by,
        })
    }

    /// The items `item` waits for that still block it, named.
    async fn blockers_of(&self, work: &mut Work, item: ItemId) -> Result<Vec<ItemName>, AppError> {
        let project = work.project_id();
        let graph = work.dependencies(project).await?;
        let mut waited_for = BTreeMap::new();
        for edge in graph.iter().filter(|edge| edge.item == item) {
            if let Some(record) = work.item(project, edge.blocked_by).await? {
                waited_for.insert(record.id, record);
            }
        }
        let state = |id| {
            waited_for
                .get(&id)
                .map(|record: &ItemRecord| (record.status, record.closed))
        };
        Ok(blockers(item, &graph, state)
            .into_iter()
            .filter_map(|id| waited_for.get(&id))
            .map(|record| ItemName::of(record, &self.ref_schemes))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::area::QualifiedArea;
    use dibs_domain::claim::ClaimRefused;
    use dibs_domain::event::{GrantEnd, GrantRevoked};
    use dibs_domain::handle::Handle;

    use super::*;
    use crate::named::Refusal;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        Complete, CreateItem, CreateItemInput, Grant, GrantInput, InitProject, InitProjectInput,
    };

    fn area(text: &str) -> QualifiedArea {
        QualifiedArea::parse(text).unwrap()
    }

    fn input(item: ItemId, session: SessionId, areas: &[&str]) -> ClaimItemInput {
        ClaimItemInput {
            item,
            session,
            areas: areas.iter().map(|a| (*a).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn a_free_item_is_claimed_with_its_areas_qualified() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let before = env.events().await.len();
        let outcome = ClaimItem::new(&env.deps)
            .execute(&env.bob.actor, input(item, env.bob.session, &["billing"]))
            .await
            .unwrap();
        assert_eq!(outcome.holder.as_str(), "bob");
        let ClaimOutcome::New { claim } = &outcome.outcome else {
            panic!("expected a new claim, got {outcome:?}");
        };
        assert_eq!(claim.member, env.bob.actor.member);
        assert_eq!(claim.host.as_str(), "desktop");
        assert!(claim.holds(&area("default:billing")));
        let kinds = env.kinds_after(before).await;
        assert_eq!(kinds.len(), 1);
        let EventKind::Claimed(claimed) = &kinds[0] else {
            panic!("expected claimed, got {kinds:?}");
        };
        assert_eq!(claimed.item, item);
        let json = serde_json::to_string(&kinds[0]).unwrap();
        assert!(json.contains("\"default:billing\""), "{json}");
        let mut tx = env.deps.store.begin().await.unwrap();
        let stored = tx.item(env.project, item).await.unwrap().unwrap();
        assert_eq!(stored.status, ItemStatus::Claimed);
        assert_eq!(
            tx.claim(env.project, item).await.unwrap().as_ref(),
            Some(claim)
        );
        drop(tx);
        let last = env.events().await.last().unwrap().id;
        assert_eq!(
            env.fakes.notifier.calls().last(),
            Some(&(env.project, last))
        );
    }

    #[tokio::test]
    async fn claiming_again_renews_and_unions_the_areas() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let claim = ClaimItem::new(&env.deps);
        claim
            .execute(&env.bob.actor, input(item, env.bob.session, &["billing"]))
            .await
            .unwrap();
        env.advance(Duration::hours(30));
        let before = env.events().await.len();
        let outcome = claim
            .execute(&env.bob.actor, input(item, env.bob.session, &["search"]))
            .await
            .unwrap();
        let ClaimOutcome::Renewed { claim, .. } = &outcome.outcome else {
            panic!("expected a renewal, got {outcome:?}");
        };
        assert_eq!(
            claim.areas,
            [area("default:billing"), area("default:search")].into()
        );
        let kinds = env.kinds_after(before).await;
        assert!(
            matches!(kinds.as_slice(), [EventKind::Renewed(_)]),
            "{kinds:?}"
        );
    }

    #[tokio::test]
    async fn a_live_claim_of_another_member_refuses_and_names_them() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let claim = ClaimItem::new(&env.deps);
        claim
            .execute(&env.bob.actor, input(item, env.bob.session, &[]))
            .await
            .unwrap();
        let before = env.events().await.len();
        let error = claim
            .execute(&env.alice.actor, input(item, env.alice.session, &[]))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "held");
        let AppError::Claim(Refusal {
            reason: ClaimRefused::HeldBy { member, .. },
            member: Some(handle),
            ..
        }) = &error
        else {
            panic!("expected held, naming the holder, got {error:?}");
        };
        assert_eq!(*member, env.bob.actor.member);
        assert_eq!(handle.as_str(), "bob");
        assert_eq!(env.events().await.len(), before);
    }

    #[tokio::test]
    async fn a_stale_claim_of_another_member_needs_a_takeover() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let claim = ClaimItem::new(&env.deps);
        claim
            .execute(&env.bob.actor, input(item, env.bob.session, &[]))
            .await
            .unwrap();
        env.advance(Duration::hours(25));
        let error = claim
            .execute(&env.alice.actor, input(item, env.alice.session, &[]))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "stale");
        assert!(matches!(
            &error,
            AppError::Claim(Refusal { member: Some(handle), .. }) if handle.as_str() == "bob"
        ));
    }

    #[tokio::test]
    async fn an_area_another_member_holds_refuses_the_claim() {
        let env = Env::new().await;
        let first = env.item("gh:acme/widgets#1").await;
        let second = env.item("gh:acme/widgets#2").await;
        let claim = ClaimItem::new(&env.deps);
        claim
            .execute(&env.bob.actor, input(first, env.bob.session, &["billing"]))
            .await
            .unwrap();
        let error = claim
            .execute(
                &env.alice.actor,
                input(second, env.alice.session, &["default:billing"]),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "area_held");
        assert!(matches!(
            &error,
            AppError::Claim(Refusal {
                reason: ClaimRefused::AreaHeld { item, .. },
                member: Some(handle),
                item_ref: Some(reference),
            }) if *item == first && handle.as_str() == "bob"
                && reference.to_string() == "gh:acme/widgets#1"
        ));
        let mut tx = env.deps.store.begin().await.unwrap();
        assert_eq!(tx.claim(env.project, second).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_closed_item_cannot_be_claimed() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let claim = ClaimItem::new(&env.deps);
        claim
            .execute(&env.bob.actor, input(item, env.bob.session, &[]))
            .await
            .unwrap();
        Complete::new(&env.deps)
            .execute(&env.bob.actor, item)
            .await
            .unwrap();
        let error = claim
            .execute(&env.alice.actor, input(item, env.alice.session, &[]))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "item_closed");
        assert!(matches!(
            error,
            AppError::ItemClosed {
                status: ItemStatus::Done
            }
        ));
    }

    #[tokio::test]
    async fn the_request_is_checked_before_anything_is_decided() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let claim = ClaimItem::new(&env.deps);
        let cases = [
            (
                env.importer.clone(),
                input(item, env.bob.session, &[]),
                "forbidden",
            ),
            (
                env.bob.actor.clone(),
                input(item, env.alice.session, &[]),
                "not_found",
            ),
            (
                env.bob.actor.clone(),
                input(ItemId(9999), env.bob.session, &[]),
                "not_found",
            ),
            (
                env.bob.actor.clone(),
                input(item, env.bob.session, &["nowhere"]),
                "unknown_area",
            ),
            (
                env.bob.actor.clone(),
                input(item, env.bob.session, &["Not An Area"]),
                "invalid_area",
            ),
        ];
        for (actor, input, code) in cases {
            let error = claim.execute(&actor, input).await.unwrap_err();
            assert_eq!(error.code(), code);
        }
        assert!(
            env.events()
                .await
                .iter()
                .all(|e| !matches!(e.kind, EventKind::Claimed(_)))
        );
    }

    #[tokio::test]
    async fn an_item_of_another_project_cannot_be_claimed() {
        let env = Env::new().await;
        let other = InitProject::new(&env.deps)
            .execute(InitProjectInput {
                slug: dibs_domain::project::ProjectSlug::parse("gadgets").unwrap(),
                admin: crate::use_cases::testing::new_member("dave"),
            })
            .await
            .unwrap();
        let dave = crate::use_cases::Authenticate::new(&env.deps)
            .execute(other.token.expose())
            .await
            .unwrap();
        let theirs = CreateItem::new(&env.deps)
            .execute(
                &dave,
                CreateItemInput {
                    external_ref: None,
                    title: dibs_domain::item::Title::parse("Theirs").unwrap(),
                },
            )
            .await
            .unwrap()
            .item
            .item
            .id;
        let error = ClaimItem::new(&env.deps)
            .execute(&env.bob.actor, input(theirs, env.bob.session, &[]))
            .await
            .unwrap_err();
        assert_eq!(error.code(), "not_found");
    }

    #[tokio::test]
    async fn a_renewal_that_loses_an_area_ends_the_grants_given_in_it() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        let carols = env.item("gh:acme/widgets#3").await;
        let claim = ClaimItem::new(&env.deps);
        claim
            .execute(
                &env.alice.actor,
                input(alices, env.alice.session, &["billing"]),
            )
            .await
            .unwrap();
        claim
            .execute(&env.bob.actor, input(bobs, env.bob.session, &[]))
            .await
            .unwrap();
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
        env.advance(Duration::hours(25));
        claim
            .execute(&env.bob.actor, input(bobs, env.bob.session, &[]))
            .await
            .unwrap();
        claim
            .execute(&env.bob.actor, input(carols, env.bob.session, &["billing"]))
            .await
            .unwrap();
        let before = env.events().await.len();
        let outcome = claim
            .execute(&env.alice.actor, input(alices, env.alice.session, &[]))
            .await
            .unwrap();
        let ClaimOutcome::Renewed { dropped_areas, .. } = &outcome.outcome else {
            panic!("expected a renewal, got {outcome:?}");
        };
        assert_eq!(*dropped_areas, [area("default:billing")].into());
        let kinds = env.kinds_after(before).await;
        assert_eq!(kinds.len(), 2, "{kinds:?}");
        assert!(matches!(kinds[0], EventKind::Renewed(_)));
        assert_eq!(
            kinds[1],
            EventKind::GrantRevoked(GrantRevoked {
                grant: granted.grant.record.id,
                item: bobs,
                area: area("default:billing"),
                from: env.alice.actor.member,
                to: env.bob.actor.member,
                cause: GrantEnd::GrantorLeft,
            })
        );
    }
}
