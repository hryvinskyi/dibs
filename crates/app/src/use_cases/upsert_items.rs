//! Bringing the project's items and imported dependencies in line with an outside source.

use std::collections::BTreeMap;
use std::sync::Arc;

use dibs_domain::event::{DependencyAdded, DependencyRemoved, EventKind, ItemCreated, ItemUpdated};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ItemId;
use dibs_domain::import::{
    Closure, ImportBatch, ImportContext, ItemChange, PlannedItem, RefusedEdge, plan_import,
};
use dibs_domain::item::{ItemMeta, SourceId};
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::records::{ItemFilter, ItemRecord};
use crate::work::Work;

/// What a batch did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertReport {
    /// Each reported item, in the order reported, with what happened to it.
    pub items: Vec<PlannedItem>,
    /// The source's items a complete batch closed because it did not mention them.
    pub closed: Vec<ItemId>,
    /// How many imported edges were added.
    pub edges_added: usize,
    /// How many imported edges were removed.
    pub edges_removed: usize,
    /// The edges not recorded, each with why.
    pub refused_edges: Vec<RefusedEdge>,
}

/// Creates and updates the items an importer reports, keyed by their reference, and replaces each
/// listed item's imported dependencies, all in one transaction.
///
/// The domain plans the batch; this use case writes what the plan changes and appends an event per
/// change only, so a batch equal to what is stored writes nothing and appends nothing. An edge
/// refused for a cycle or an unknown blocker is reported on its own and the rest applies. Nothing
/// in a batch touches a claim. Only an admin and the importer may send one.
pub struct UpsertItems {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl UpsertItems {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Applies the batch.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a member who is neither an admin nor the importer;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        batch: ImportBatch,
    ) -> Result<UpsertReport, AppError> {
        actor.require_import()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin(&*self.store, project).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let records = work.items(project, &ItemFilter::default()).await?;
        let graph = work.dependencies(project).await?;
        let known: Vec<_> = records.iter().map(ItemRecord::known).collect();
        let context = ImportContext {
            known: &known,
            graph: &graph,
            settings: &settings,
            schemes: &self.ref_schemes,
            by: actor.role,
        };
        let plan = plan_import(&batch, &context, &mut || ItemId(self.ids.next()));
        let mut stored: BTreeMap<ItemId, ItemRecord> =
            records.into_iter().map(|item| (item.id, item)).collect();
        let mut writer = Writer {
            work: &mut work,
            actor,
            now,
        };
        for planned in &plan.items {
            match &planned.change {
                ItemChange::Create {
                    id,
                    repository,
                    meta,
                    source,
                } => {
                    let mut record = ItemRecord::new(
                        *id,
                        Some(planned.reference.clone()),
                        meta.title.clone(),
                        repository.clone(),
                        actor.member,
                        now,
                    );
                    record.set_meta(meta.clone());
                    record.source.clone_from(source);
                    writer.create(&record).await?;
                }
                ItemChange::Update { id, meta, source } => {
                    let record = stored_item(&mut stored, *id)?;
                    writer.update(record, meta.clone(), source.clone()).await?;
                }
                ItemChange::Unchanged { .. }
                | ItemChange::SkippedClosed
                | ItemChange::Refused(_) => {}
            }
        }
        for Closure { id, meta } in &plan.closed {
            let record = stored_item(&mut stored, *id)?;
            let source = record.source.clone();
            writer.update(record, meta.clone(), source).await?;
        }
        for edge in &plan.removed {
            writer
                .work
                .delete_dependency(project, edge.item, edge.blocked_by)
                .await?;
            writer
                .record(EventKind::DependencyRemoved(DependencyRemoved {
                    item: edge.item,
                    blocked_by: edge.blocked_by,
                    by: actor.member,
                }))
                .await?;
        }
        for edge in &plan.added {
            writer.work.save_dependency(project, edge).await?;
            writer
                .record(EventKind::DependencyAdded(DependencyAdded {
                    item: edge.item,
                    blocked_by: edge.blocked_by,
                    by: actor.member,
                }))
                .await?;
        }
        work.commit(&*self.notifier).await?;
        Ok(UpsertReport {
            closed: plan.closed.iter().map(|closure| closure.id).collect(),
            edges_added: plan.added.len(),
            edges_removed: plan.removed.len(),
            items: plan.items,
            refused_edges: plan.refused_edges,
        })
    }
}

/// The stored item a plan names.
fn stored_item(
    stored: &mut BTreeMap<ItemId, ItemRecord>,
    id: ItemId,
) -> Result<&mut ItemRecord, AppError> {
    stored.get_mut(&id).ok_or_else(|| {
        AppError::Store(StoreError::Corrupt {
            what: format!("an import plans a change of item {id}, which does not exist"),
        })
    })
}

/// Writes the items of a batch and appends their events in the transaction.
struct Writer<'a> {
    work: &'a mut Work,
    actor: &'a Actor,
    now: time::OffsetDateTime,
}

impl Writer<'_> {
    async fn create(&mut self, record: &ItemRecord) -> Result<(), AppError> {
        let project = self.work.project_id();
        self.work.insert_item(project, record).await?;
        self.record(EventKind::ItemCreated(ItemCreated {
            item: record.id,
            external_ref: record.external_ref.clone(),
            title: record.title.clone(),
            by: self.actor.member,
        }))
        .await
    }

    async fn update(
        &mut self,
        record: &mut ItemRecord,
        meta: ItemMeta,
        source: Option<SourceId>,
    ) -> Result<(), AppError> {
        let project = self.work.project_id();
        record.set_meta(meta);
        record.source = source;
        self.work.update_item(project, record).await?;
        self.record(EventKind::ItemUpdated(ItemUpdated {
            item: record.id,
            by: self.actor.member,
        }))
        .await
    }

    async fn record(&mut self, kind: EventKind) -> Result<(), AppError> {
        self.work.record(self.now, kind).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::dependency::{Dependency, DependencySource};
    use dibs_domain::import::{EdgeRefused, Listing, ReportedItem};
    use dibs_domain::item::{ExternalRef, HintDropped, Priority, SourceStatus, Title};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        AddDependency, ClaimItem, ClaimItemInput, DependencyInput, ItemQuery, ListItems,
    };

    fn reference(text: &str) -> ExternalRef {
        ExternalRef::parse(text).unwrap()
    }

    fn listed(text: &str, blocked_by: &[&str]) -> ReportedItem {
        ReportedItem {
            reference: reference(text),
            title: Title::parse(&format!("Work on {text}")).unwrap(),
            closed: false,
            listing: Some(Listing {
                areas_hint: Vec::new(),
                priority: Priority::default(),
                source_status: None,
                blocked_by: blocked_by.iter().map(|b| reference(b)).collect(),
            }),
        }
    }

    fn blocker(text: &str, closed: bool) -> ReportedItem {
        ReportedItem {
            closed,
            listing: None,
            ..listed(text, &[])
        }
    }

    fn batch(items: Vec<ReportedItem>) -> ImportBatch {
        ImportBatch::new(Some(SourceId::parse("board").unwrap()), false, items).unwrap()
    }

    fn complete(items: Vec<ReportedItem>) -> ImportBatch {
        ImportBatch::new(Some(SourceId::parse("board").unwrap()), true, items).unwrap()
    }

    async fn import(env: &Env, batch: ImportBatch) -> UpsertReport {
        UpsertItems::new(&env.deps)
            .execute(&env.importer, batch)
            .await
            .unwrap()
    }

    async fn item(env: &Env, text: &str) -> ItemRecord {
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.item_by_ref(env.project, &reference(text))
            .await
            .unwrap()
            .unwrap()
    }

    async fn edges(env: &Env) -> Vec<Dependency> {
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.dependencies(env.project).await.unwrap()
    }

    fn names(kinds: &[EventKind]) -> Vec<&'static str> {
        kinds.iter().map(EventKind::name).collect()
    }

    #[tokio::test]
    async fn a_batch_creates_its_items_and_edges_and_a_repeat_writes_and_appends_nothing() {
        let env = Env::new().await;
        let before = env.events().await.len();
        let mut first = listed("gh:acme/web#1", &["gh:acme/web#2"]);
        if let Some(listing) = first.listing.as_mut() {
            listing.priority = Priority::new(1).unwrap();
            listing.areas_hint = vec!["billing".to_owned()];
            listing.source_status = Some(SourceStatus::parse("Todo").unwrap());
        }
        let items = vec![first, listed("gh:Acme/Web#2", &[])];
        let report = import(&env, batch(items.clone())).await;
        assert_eq!(report.edges_added, 1);
        assert!(report.refused_edges.is_empty());
        assert_eq!(
            names(&env.kinds_after(before).await),
            ["item_created", "item_created", "dependency_added"]
        );
        let stored = item(&env, "gh:acme/web#1").await;
        assert_eq!(stored.priority.get(), 1);
        assert_eq!(
            stored.source_status.as_ref().map(SourceStatus::as_str),
            Some("Todo")
        );
        assert_eq!(stored.source.as_ref().map(SourceId::as_str), Some("board"));
        assert_eq!(
            stored
                .areas_hint
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["default:billing"]
        );
        assert_eq!(stored.created_by, env.importer.member);

        let events = env.events().await.len();
        let wakes = env.fakes.notifier.calls().len();
        let again = import(&env, batch(items)).await;
        assert!(
            again
                .items
                .iter()
                .all(|planned| matches!(planned.change, ItemChange::Unchanged { .. }))
        );
        assert_eq!(
            env.events().await.len(),
            events,
            "a repeated batch appends nothing"
        );
        assert_eq!(env.fakes.notifier.calls().len(), wakes, "and wakes nobody");
        assert_eq!(item(&env, "gh:acme/web#1").await, stored);
    }

    #[tokio::test]
    async fn an_event_is_appended_only_for_the_item_that_changed() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &[]),
                listed("gh:acme/web#2", &[]),
            ]),
        )
        .await;
        let before = env.events().await.len();
        let mut renamed = listed("gh:acme/web#2", &[]);
        renamed.title = Title::parse("Renamed upstream").unwrap();
        import(&env, batch(vec![listed("gh:acme/web#1", &[]), renamed])).await;
        let id = item(&env, "gh:acme/web#2").await.id;
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::ItemUpdated(dibs_domain::event::ItemUpdated {
                item: id,
                by: env.importer.member,
            })]
        );
        assert_eq!(
            item(&env, "gh:acme/web#2").await.title.as_str(),
            "Renamed upstream"
        );
    }

    #[tokio::test]
    async fn a_closed_item_the_project_does_not_know_is_skipped_and_a_known_one_is_closed() {
        let env = Env::new().await;
        import(&env, batch(vec![listed("gh:acme/web#1", &[])])).await;
        let mut known = listed("gh:acme/web#1", &[]);
        known.closed = true;
        let mut unknown = listed("gh:acme/web#9", &[]);
        unknown.closed = true;
        let report = import(&env, batch(vec![known, unknown])).await;
        assert_eq!(report.items[1].change, ItemChange::SkippedClosed);
        assert!(item(&env, "gh:acme/web#1").await.closed);
        let all = ListItems::new(&env.deps)
            .execute(&env.alice.actor, ItemQuery::default())
            .await
            .unwrap();
        assert_eq!(all.len(), 1, "the closed ticket was not created");
    }

    #[tokio::test]
    async fn an_unknown_blocker_is_refused_unless_the_batch_carries_it() {
        let env = Env::new().await;
        let report = import(
            &env,
            batch(vec![
                listed(
                    "gh:acme/web#1",
                    &["gh:acme/api#7", "gh:acme/api#8", "gh:acme/api#9"],
                ),
                blocker("gh:acme/api#8", false),
                blocker("gh:acme/api#9", true),
            ]),
        )
        .await;
        assert_eq!(report.refused_edges.len(), 1);
        assert_eq!(
            report.refused_edges[0].blocked_by,
            reference("gh:acme/api#7")
        );
        assert_eq!(report.refused_edges[0].reason, EdgeRefused::UnknownBlocker);
        let waiting = item(&env, "gh:acme/web#1").await.id;
        let open_blocker = item(&env, "gh:acme/api#8").await;
        assert_eq!(
            open_blocker.source, None,
            "a blocker off the source belongs to none"
        );
        assert_eq!(
            edges(&env).await,
            [Dependency {
                item: waiting,
                blocked_by: open_blocker.id,
                source: DependencySource::Imported,
            }]
        );
    }

    #[tokio::test]
    async fn an_edge_closing_a_cycle_is_refused_while_the_rest_of_the_batch_applies() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &["gh:acme/web#2"]),
                listed("gh:acme/web#2", &[]),
            ]),
        )
        .await;
        let report = import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &["gh:acme/web#2"]),
                listed("gh:acme/web#2", &["gh:acme/web#1"]),
                listed("gh:acme/web#3", &["gh:acme/web#2"]),
            ]),
        )
        .await;
        assert_eq!(report.refused_edges.len(), 1);
        assert_eq!(report.refused_edges[0].reason, EdgeRefused::Cycle);
        assert_eq!(report.refused_edges[0].item, reference("gh:acme/web#2"));
        assert_eq!(report.edges_added, 1, "the third item's edge applies");
        assert_eq!(edges(&env).await.len(), 2);
    }

    #[tokio::test]
    async fn a_persons_edge_survives_every_import() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &["gh:acme/web#2"]),
                listed("gh:acme/web#2", &[]),
                listed("gh:acme/web#3", &[]),
            ]),
        )
        .await;
        let one = item(&env, "gh:acme/web#1").await.id;
        let three = item(&env, "gh:acme/web#3").await.id;
        AddDependency::new(&env.deps)
            .execute(
                &env.bob.actor,
                DependencyInput {
                    item: one,
                    blocked_by: three,
                },
            )
            .await
            .unwrap();
        let report = import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &[]),
                listed("gh:acme/web#2", &[]),
                listed("gh:acme/web#3", &[]),
            ]),
        )
        .await;
        assert_eq!(report.edges_removed, 1);
        assert_eq!(
            edges(&env).await,
            [Dependency {
                item: one,
                blocked_by: three,
                source: DependencySource::Manual,
            }]
        );
    }

    #[tokio::test]
    async fn a_complete_batch_closes_what_it_did_not_mention_and_a_partial_one_does_not() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &[]),
                listed("gh:acme/web#2", &[]),
            ]),
        )
        .await;
        let mine = env.item("gh:acme/web#5").await;
        import(&env, batch(vec![listed("gh:acme/web#1", &[])])).await;
        assert!(!item(&env, "gh:acme/web#2").await.closed);
        let report = import(&env, complete(vec![listed("gh:acme/web#1", &[])])).await;
        let gone = item(&env, "gh:acme/web#2").await;
        assert_eq!(report.closed, [gone.id]);
        assert!(gone.closed);
        assert!(!item(&env, "gh:acme/web#1").await.closed);
        let person_made = ListItems::new(&env.deps)
            .execute(&env.alice.actor, ItemQuery::default())
            .await
            .unwrap()
            .into_iter()
            .find(|named| named.item.id == mine)
            .unwrap();
        assert!(
            !person_made.item.closed,
            "an item of no source is never closed by a batch"
        );
    }

    #[tokio::test]
    async fn an_import_changes_no_claim() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &[]),
                listed("gh:acme/web#2", &[]),
            ]),
        )
        .await;
        let held = item(&env, "gh:acme/web#1").await.id;
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    wide: None,
                    item: held,
                    session: env.bob.session,
                    areas: vec!["billing".to_owned()],
                },
            )
            .await
            .unwrap();
        let claim_before = {
            let mut tx = env.deps.store.begin().await.unwrap();
            tx.claim(env.project, held).await.unwrap()
        };
        let before = env.events().await.len();
        let mut closed = listed("gh:acme/web#1", &["gh:acme/web#2"]);
        closed.closed = true;
        import(&env, complete(vec![closed])).await;
        let mut tx = env.deps.store.begin().await.unwrap();
        assert_eq!(tx.claim(env.project, held).await.unwrap(), claim_before);
        let stored = tx.item(env.project, held).await.unwrap().unwrap();
        assert!(stored.closed, "closed upstream");
        assert_eq!(
            stored.status,
            dibs_domain::item::ItemStatus::Claimed,
            "still claimed"
        );
        drop(tx);
        let kinds = names(&env.kinds_after(before).await);
        assert!(
            kinds.iter().all(
                |kind| ["item_updated", "dependency_added", "dependency_removed"].contains(kind)
            ),
            "{kinds:?}"
        );
    }

    #[tokio::test]
    async fn an_area_the_project_does_not_declare_is_dropped_with_a_note() {
        let env = Env::new().await;
        let mut hinted = listed("gh:acme/web#1", &[]);
        if let Some(listing) = hinted.listing.as_mut() {
            listing.areas_hint = vec!["search".to_owned(), "payroll".to_owned()];
        }
        let report = import(&env, batch(vec![hinted])).await;
        assert_eq!(report.items[0].dropped_hints.len(), 1);
        assert_eq!(report.items[0].dropped_hints[0].text, "payroll");
        assert_eq!(
            report.items[0].dropped_hints[0].reason,
            HintDropped::Undeclared
        );
        assert_eq!(item(&env, "gh:acme/web#1").await.areas_hint.len(), 1);
    }

    #[tokio::test]
    async fn only_an_admin_and_the_importer_may_send_a_batch() {
        let env = Env::new().await;
        let upsert = UpsertItems::new(&env.deps);
        let refused = upsert
            .execute(&env.bob.actor, batch(vec![listed("gh:acme/web#1", &[])]))
            .await
            .unwrap_err();
        assert_eq!(refused.code(), "forbidden");
        assert!(
            upsert
                .execute(&env.alice.actor, batch(vec![listed("gh:acme/web#1", &[])]))
                .await
                .is_ok()
        );
    }
}
