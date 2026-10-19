//! A holder letting another member edit inside an area they hold.

use std::sync::Arc;

use dibs_domain::event::{EventKind, Granted};
use dibs_domain::grant::{self, GrantOutcome, decide_grant};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{GrantId, ItemId};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedGrant, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::records::GrantRecord;
use crate::use_cases::common::{existing_item, member_by_handle};
use crate::work::Work;

/// The grant to give.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantInput {
    /// The grantee's work item: the work that needs the edit.
    pub item: ItemId,
    /// The area, as a person names it.
    pub area: String,
    /// The grantee.
    pub to: Handle,
}

/// The grant in force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantResult {
    /// The grant, with the handles of its two members.
    pub grant: NamedGrant,
    /// Whether this call gave it, rather than finding it given.
    pub created: bool,
}

/// Records the caller's consent that another member may edit inside an area the caller holds, for
/// the grantee's live claim on an item. Giving a grant that exists changes nothing.
pub struct Grant {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl Grant {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Gives the grant.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item or
    /// grantee; [`AppError::Area`] for an area that does not resolve; [`AppError::Grant`] when the
    /// caller does not hold the area, grants to themselves, or the grantee holds no live claim on
    /// the item; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, input: GrantInput) -> Result<GrantResult, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin(&*self.store, project).await?;
        let item = existing_item(&mut work, input.item).await?;
        let grantee = member_by_handle(&mut work, &input.to).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let request = grant::Grant {
            item: item.id,
            area: settings.resolve_area(&input.area)?,
            from: actor.member,
            to: grantee.id,
        };
        let records = work.grants(project).await?;
        let existing: Vec<grant::Grant> = records.iter().map(|r| r.grant.clone()).collect();
        let claims = work.claims(project).await?;
        match decide_grant(&request, &existing, &claims, now, &settings)? {
            GrantOutcome::AlreadyGranted => {
                let grant = records
                    .into_iter()
                    .find(|record| record.grant == request)
                    .ok_or_else(|| StoreError::Corrupt {
                        what: "a grant the domain found but the store did not return".to_owned(),
                    })?;
                Ok(GrantResult {
                    grant: Names::load(&mut work).await?.grant(grant)?,
                    created: false,
                })
            }
            GrantOutcome::New(grant) => {
                let record = GrantRecord {
                    id: GrantId(self.ids.next()),
                    grant,
                    created_at: now,
                };
                work.insert_grant(project, &record).await?;
                work.record(
                    now,
                    EventKind::Granted(Granted {
                        grant: record.id,
                        item: record.grant.item,
                        area: record.grant.area.clone(),
                        from: record.grant.from,
                        to: record.grant.to,
                    }),
                )
                .await?;
                let grant = Names::load(&mut work).await?.grant(record)?;
                work.commit(&*self.notifier).await?;
                Ok(GrantResult {
                    grant,
                    created: true,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::area::QualifiedArea;

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

    fn to_bob(item: ItemId, area: &str) -> GrantInput {
        GrantInput {
            item,
            area: area.to_owned(),
            to: Handle::parse("bob").unwrap(),
        }
    }

    #[tokio::test]
    async fn a_holder_grants_once_and_a_repeat_changes_nothing() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        let grant = Grant::new(&env.deps);
        let before = env.events().await.len();
        let given = grant
            .execute(&env.alice.actor, to_bob(bobs, "billing"))
            .await
            .unwrap();
        assert!(given.created);
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::Granted(Granted {
                grant: given.grant.record.id,
                item: bobs,
                area: QualifiedArea::parse("default:billing").unwrap(),
                from: env.alice.actor.member,
                to: env.bob.actor.member,
            })]
        );
        let after = env.events().await.len();
        let again = grant
            .execute(&env.alice.actor, to_bob(bobs, "default:billing"))
            .await
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.grant, given.grant);
        assert_eq!(env.events().await.len(), after);
    }

    #[tokio::test]
    async fn every_rule_of_a_grant_is_enforced() {
        let env = Env::new().await;
        let alices = env.item("gh:acme/widgets#1").await;
        let bobs = env.item("gh:acme/widgets#2").await;
        let nobodys = env.item("gh:acme/widgets#3").await;
        claim(&env, &env.alice, alices, &["billing"]).await;
        claim(&env, &env.bob, bobs, &[]).await;
        let grant = Grant::new(&env.deps);
        let to_alice = GrantInput {
            to: Handle::parse("alice").unwrap(),
            ..to_bob(alices, "billing")
        };
        let cases = [
            (env.alice.actor.clone(), to_alice, "self_grant"),
            (
                env.alice.actor.clone(),
                to_bob(bobs, "search"),
                "not_holder",
            ),
            (
                env.alice.actor.clone(),
                to_bob(nobodys, "billing"),
                "grantee_has_no_claim",
            ),
            (
                env.alice.actor.clone(),
                to_bob(bobs, "nowhere"),
                "unknown_area",
            ),
            (
                env.alice.actor.clone(),
                to_bob(ItemId(9999), "billing"),
                "not_found",
            ),
            (
                env.alice.actor.clone(),
                GrantInput {
                    to: Handle::parse("nobody").unwrap(),
                    ..to_bob(bobs, "billing")
                },
                "not_found",
            ),
            (env.importer.clone(), to_bob(bobs, "billing"), "forbidden"),
        ];
        let before = env.events().await.len();
        for (actor, input, code) in cases {
            let error = grant.execute(&actor, input).await.unwrap_err();
            assert_eq!(error.code(), code);
        }
        assert_eq!(env.events().await.len(), before);
    }
}
