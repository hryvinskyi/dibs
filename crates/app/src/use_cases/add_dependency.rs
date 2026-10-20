//! A person recording that one work item is blocked by another.

use std::sync::Arc;

use dibs_domain::dependency::{Dependency, DependencySource, add_dependency};
use dibs_domain::event::{DependencyAdded, EventKind};
use dibs_domain::ids::ItemId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedItem, Names};
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::common::existing_item;
use crate::work::Work;

/// The edge to record or remove: `item` is blocked by `blocked_by`, both by local id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyInput {
    /// The item that waits.
    pub item: ItemId,
    /// The item it waits for.
    pub blocked_by: ItemId,
}

/// The edge after the call, with both items named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyChange {
    /// The item that waits.
    pub item: NamedItem,
    /// The item it waits for.
    pub blocked_by: NamedItem,
    /// Whether this call changed the work graph: `false` when the edge was already there (for an
    /// addition) or was not there (for a removal).
    pub changed: bool,
}

/// Records, for a person, that an item is blocked by another. The edge is the person's own: an
/// import replaces only the edges it reported, so this one survives every import, and an edge an
/// import reported for the same pair becomes the person's. An edge that would make an item wait
/// for itself, directly or through other items, is refused. Adding an edge that exists changes
/// nothing and appends nothing.
pub struct AddDependency {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl AddDependency {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Records the edge.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an item the project
    /// does not have; [`AppError::Dependency`] for an edge that would close a cycle;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: DependencyInput,
    ) -> Result<DependencyChange, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin(&*self.store, project).await?;
        let item = existing_item(&mut work, input.item).await?;
        let blocked_by = existing_item(&mut work, input.blocked_by).await?;
        let graph = work.dependencies(project).await?;
        let edge = Dependency {
            item: item.id,
            blocked_by: blocked_by.id,
            source: DependencySource::Manual,
        };
        let existing = graph
            .iter()
            .find(|known| known.item == edge.item && known.blocked_by == edge.blocked_by)
            .copied();
        let changed = existing.is_none();
        if changed {
            add_dependency(&graph, &edge)?;
            work.record(
                now,
                EventKind::DependencyAdded(DependencyAdded {
                    item: edge.item,
                    blocked_by: edge.blocked_by,
                    by: actor.member,
                }),
            )
            .await?;
        }
        if existing.is_none_or(|known| known.source != DependencySource::Manual) {
            work.save_dependency(project, &edge).await?;
        }
        let names = Names::load(&mut work).await?;
        let change = DependencyChange {
            item: names.item(item)?,
            blocked_by: names.item(blocked_by)?,
            changed,
        };
        work.commit(&*self.notifier).await?;
        Ok(change)
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::dependency::Dependency;
    use dibs_domain::import::{ImportBatch, Listing, ReportedItem};
    use dibs_domain::item::{ExternalRef, Priority, SourceId, Title};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{ClaimItem, ClaimItemInput, RemoveDependency, UpsertItems};

    fn input(item: ItemId, blocked_by: ItemId) -> DependencyInput {
        DependencyInput { item, blocked_by }
    }

    async fn edges(env: &Env) -> Vec<Dependency> {
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.dependencies(env.project).await.unwrap()
    }

    #[tokio::test]
    async fn an_edge_is_recorded_once_and_announced_once() {
        let env = Env::new().await;
        let a = env.item("gh:acme/web#1").await;
        let b = env.item("gh:acme/web#2").await;
        let add = AddDependency::new(&env.deps);
        let before = env.events().await.len();
        let made = add.execute(&env.bob.actor, input(a, b)).await.unwrap();
        assert!(made.changed);
        assert_eq!(made.blocked_by.item.id, b);
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::DependencyAdded(DependencyAdded {
                item: a,
                blocked_by: b,
                by: env.bob.actor.member,
            })]
        );
        let again = add.execute(&env.alice.actor, input(a, b)).await.unwrap();
        assert!(!again.changed);
        assert_eq!(env.events().await.len(), before + 1);
        assert_eq!(edges(&env).await.len(), 1);
    }

    #[tokio::test]
    async fn an_edge_that_would_close_a_cycle_or_wait_for_itself_is_refused() {
        let env = Env::new().await;
        let a = env.item("gh:acme/web#1").await;
        let b = env.item("gh:acme/web#2").await;
        let c = env.item("gh:acme/web#3").await;
        let add = AddDependency::new(&env.deps);
        add.execute(&env.bob.actor, input(a, b)).await.unwrap();
        add.execute(&env.bob.actor, input(b, c)).await.unwrap();
        let cycle = add.execute(&env.bob.actor, input(c, a)).await.unwrap_err();
        assert_eq!(cycle.code(), "dependency_cycle");
        let itself = add.execute(&env.bob.actor, input(a, a)).await.unwrap_err();
        assert_eq!(itself.code(), "self_dependency");
        assert_eq!(edges(&env).await.len(), 2);
    }

    #[tokio::test]
    async fn an_edge_the_importer_reported_becomes_the_persons_and_survives() {
        let env = Env::new().await;
        let reported = |text: &str, blocked_by: &[&str]| ReportedItem {
            reference: ExternalRef::parse(text).unwrap(),
            title: Title::parse("Imported").unwrap(),
            closed: false,
            listing: Some(Listing {
                areas_hint: Vec::new(),
                priority: Priority::default(),
                source_status: None,
                blocked_by: blocked_by
                    .iter()
                    .map(|b| ExternalRef::parse(b).unwrap())
                    .collect(),
            }),
        };
        let board = || Some(SourceId::parse("board").unwrap());
        let upsert = UpsertItems::new(&env.deps);
        upsert
            .execute(
                &env.importer,
                ImportBatch::new(
                    board(),
                    false,
                    vec![
                        reported("gh:acme/web#1", &["gh:acme/web#2"]),
                        reported("gh:acme/web#2", &[]),
                    ],
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let [edge] = edges(&env).await[..] else {
            panic!("one edge");
        };
        let before = env.events().await.len();
        let made = AddDependency::new(&env.deps)
            .execute(&env.bob.actor, input(edge.item, edge.blocked_by))
            .await
            .unwrap();
        assert!(!made.changed);
        assert_eq!(env.events().await.len(), before);
        upsert
            .execute(
                &env.importer,
                ImportBatch::new(
                    board(),
                    false,
                    vec![
                        reported("gh:acme/web#1", &[]),
                        reported("gh:acme/web#2", &[]),
                    ],
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(edges(&env).await.len(), 1, "the person's edge stays");
    }

    #[tokio::test]
    async fn removing_an_edge_announces_it_and_removing_it_again_changes_nothing() {
        let env = Env::new().await;
        let a = env.item("gh:acme/web#1").await;
        let b = env.item("gh:acme/web#2").await;
        AddDependency::new(&env.deps)
            .execute(&env.bob.actor, input(a, b))
            .await
            .unwrap();
        let remove = RemoveDependency::new(&env.deps);
        let before = env.events().await.len();
        assert!(
            remove
                .execute(&env.alice.actor, input(a, b))
                .await
                .unwrap()
                .changed
        );
        assert_eq!(env.kinds_after(before).await.len(), 1);
        assert!(
            !remove
                .execute(&env.alice.actor, input(a, b))
                .await
                .unwrap()
                .changed
        );
        assert_eq!(env.events().await.len(), before + 1);
        assert!(edges(&env).await.is_empty());
    }

    #[tokio::test]
    async fn an_integration_may_not_edit_the_graph_and_an_unknown_item_is_missing() {
        let env = Env::new().await;
        let a = env.item("gh:acme/web#1").await;
        let b = env.item("gh:acme/web#2").await;
        let add = AddDependency::new(&env.deps);
        assert_eq!(
            add.execute(&env.importer, input(a, b))
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
        assert_eq!(
            RemoveDependency::new(&env.deps)
                .execute(&env.importer, input(a, b))
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
        assert_eq!(
            add.execute(&env.bob.actor, input(a, ItemId(9_999)))
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
    }

    #[tokio::test]
    async fn a_claim_on_a_blocked_item_is_made_and_names_its_blockers() {
        let env = Env::new().await;
        let a = env.item("gh:acme/web#1").await;
        let b = env.item("gh:acme/web#2").await;
        AddDependency::new(&env.deps)
            .execute(&env.bob.actor, input(a, b))
            .await
            .unwrap();
        let made = ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    wide: None,
                    item: a,
                    session: env.bob.session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(made.blocked_by.len(), 1);
        assert_eq!(made.blocked_by[0].id, b);
        assert_eq!(
            made.blocked_by[0].link.as_deref(),
            Some("https://github.com/acme/web/issues/2")
        );
        let free = ClaimItem::new(&env.deps)
            .execute(
                &env.alice.actor,
                ClaimItemInput {
                    wide: None,
                    item: b,
                    session: env.alice.session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert!(free.blocked_by.is_empty());
    }
}
