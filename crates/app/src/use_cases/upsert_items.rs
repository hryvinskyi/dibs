//! Bringing the project's items and imported dependencies in line with an outside source.

use std::collections::BTreeMap;
use std::sync::Arc;

use dibs_domain::claim::Claim;
use dibs_domain::event::{
    CompletedByClose, DependencyAdded, DependencyRemoved, EventKind, ItemCreated, ItemUpdated,
};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ItemId;
use dibs_domain::import::{
    Closure, ImportBatch, ImportContext, ItemChange, PlannedItem, RefusedEdge, plan_import,
};
use dibs_domain::item::{ItemMeta, ItemStatus, SourceId};
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::records::{ItemFilter, ItemRecord};
use crate::use_cases::claim_ending::{Ending, end_claim, tell_holder};
use crate::use_cases::handover::Handovers;
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
/// change only. An edge refused for a cycle or an unknown blocker is reported on its own and the
/// rest applies. Once the items, closures and edges are written, the claim standing on each item
/// the batch speaks for as its source and finds closed is completed in the same transaction,
/// whatever its state: the item is done, the end is recorded as `completed_by_close`, and its
/// holder is told in a message from dibs. Those items are the ones a batch naming its source
/// reports closed, listed or carried as a blocker whose stored source is that same source, and the
/// ones of that source a complete batch leaves out, whether it closes them now or they were closed
/// before. Each claim is read again just before it ends, since ending one settles handovers that
/// can change another.
///
/// A claim on an item the batch reports open, or that a partial batch does not mention, is left as
/// it is. So is one on an item the batch carries only as a closed blocker while it belongs to
/// another source or to none, or reports closed without naming a source: that report only marks
/// the item closed. A batch equal to what is stored, with no claim on an item it finds closed,
/// writes nothing and appends nothing. Only an admin and the importer may send one.
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
        let mut work = Work::begin_for(&*self.store, actor).await?;
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
        let handovers = Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        for id in plan.closed_items() {
            let Some(claim) = work.claim(project, id).await? else {
                continue;
            };
            self.complete(&mut work, &claim, &handovers).await?;
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

    /// Completes a claim whose item the batch finds closed, and tells its holder.
    async fn complete(
        &self,
        work: &mut Work,
        claim: &Claim,
        handovers: &Handovers<'_>,
    ) -> Result<(), AppError> {
        let ending = Ending {
            status: ItemStatus::Done,
            event: EventKind::CompletedByClose(CompletedByClose {
                item: claim.item,
                member: claim.member,
            }),
        };
        end_claim(work, claim, ending, handovers).await?;
        tell_holder(
            work,
            &*self.ids,
            &self.ref_schemes,
            claim,
            "Done in the tracker: ",
            |item, title| {
                format!(
                    "Work item {item} \"{title}\" was closed in its tracker; the claim on it is done. Its areas are free and the item is done."
                )
            },
            handovers.now,
        )
        .await
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
    use time::Duration;

    use dibs_domain::area::QualifiedArea;
    use dibs_domain::dependency::{Dependency, DependencySource};
    use dibs_domain::event::AreaPassing;
    use dibs_domain::handover::ClaimKind;
    use dibs_domain::ids::MemberId;
    use dibs_domain::import::{EdgeRefused, Listing, ReportedItem};
    use dibs_domain::item::{ExternalRef, HintDropped, Priority, SourceStatus, Title};
    use dibs_domain::message::{Author, Importance};
    use dibs_domain::review::PrRef;

    use super::*;
    use crate::records::MessageRecord;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::writing::TITLE_NOTE;
    use crate::use_cases::{
        AddDependency, ClaimItem, ClaimItemInput, DependencyInput, ItemQuery, ListItems,
        ListThreads, ReadThread, StartReview, StartReviewInput,
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

    async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str], kind: Option<ClaimKind>) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    wide: None,
                    item,
                    session: who.session,
                    areas: areas.iter().map(|area| (*area).to_owned()).collect(),
                    kind,
                },
            )
            .await
            .unwrap();
    }

    async fn stored_claim(env: &Env, item: ItemId) -> Option<Claim> {
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.claim(env.project, item).await.unwrap()
    }

    fn closed(item: ReportedItem) -> ReportedItem {
        ReportedItem {
            closed: true,
            ..item
        }
    }

    /// Every message `who` can read, with the subject of its conversation, oldest conversation
    /// first.
    async fn messages(env: &Env, who: &Person) -> Vec<(String, MessageRecord)> {
        let mut threads = ListThreads::new(&env.deps)
            .execute(&who.actor, None)
            .await
            .unwrap();
        threads.sort_by_key(|summary| summary.thread.record.id);
        let mut all = Vec::new();
        for summary in threads {
            let view = ReadThread::new(&env.deps)
                .execute(&who.actor, summary.thread.record.id)
                .await
                .unwrap();
            for message in view.messages {
                all.push((summary.thread.record.subject.to_string(), message.record));
            }
        }
        all
    }

    fn done_by_close(item: ItemId, member: MemberId) -> EventKind {
        EventKind::CompletedByClose(CompletedByClose { item, member })
    }

    /// Asserts that the claim on the item is gone, the item done and closed, and the end recorded
    /// once among `kinds` under its holder.
    async fn assert_completed(env: &Env, text: &str, holder: MemberId, kinds: &[EventKind]) {
        let item = item(env, text).await;
        assert_eq!(stored_claim(env, item.id).await, None, "the claim ended");
        assert_eq!(item.status, ItemStatus::Done);
        assert!(item.closed);
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == done_by_close(item.id, holder))
                .count(),
            1,
            "{kinds:?}"
        );
    }

    #[tokio::test]
    async fn an_import_that_leaves_a_claimed_item_open_changes_no_claim() {
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
        claim(&env, &env.bob, held, &["billing"], None).await;
        let claim_before = stored_claim(&env, held).await;
        let before = env.events().await.len();
        let mut renamed = listed("gh:acme/web#1", &["gh:acme/web#2"]);
        renamed.title = Title::parse("Renamed upstream").unwrap();
        import(&env, complete(vec![renamed, listed("gh:acme/web#2", &[])])).await;
        assert_eq!(stored_claim(&env, held).await, claim_before);
        let stored = item(&env, "gh:acme/web#1").await;
        assert!(!stored.closed);
        assert_eq!(stored.status, ItemStatus::Claimed, "still claimed");
        assert_eq!(
            names(&env.kinds_after(before).await),
            ["item_updated", "dependency_added"]
        );
    }

    #[tokio::test]
    async fn an_import_reporting_a_claimed_item_closed_completes_its_claim_and_tells_its_holder() {
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
        claim(&env, &env.bob, held, &["billing"], None).await;
        let told = messages(&env, &env.bob).await.len();
        let before = env.events().await.len();
        let report = import(&env, batch(vec![closed(listed("gh:acme/web#1", &[]))])).await;
        assert!(matches!(report.items[0].change, ItemChange::Update { .. }));
        let kinds = env.kinds_after(before).await;
        let bob = env.bob.actor.member;
        assert_completed(&env, "gh:acme/web#1", bob, &kinds).await;
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| kind.completion().is_some())
                .count(),
            1,
            "one completion: {kinds:?}"
        );
        let new = messages(&env, &env.bob).await.split_off(told);
        assert_eq!(new.len(), 1, "one message to the holder: {new:?}");
        let (subject, message) = &new[0];
        assert_eq!(subject, "Done in the tracker: Work on gh:acme/web#1");
        assert_eq!(message.author, Author::System);
        assert_eq!(
            message.body.as_str(),
            format!(
                "Work item gh:acme/web#1 \"Work on gh:acme/web#1\" was closed in its tracker; the claim on it is done. Its areas are free and the item is done.\n{TITLE_NOTE}"
            )
        );
        assert_eq!(message.importance, Importance::Normal);
        assert!(!message.ack_required);
        let other = item(&env, "gh:acme/web#2").await.id;
        claim(&env, &env.alice, other, &["billing"], None).await;
        assert!(
            stored_claim(&env, other)
                .await
                .unwrap()
                .holds(&QualifiedArea::parse("default:billing").unwrap()),
            "its areas are free"
        );
    }

    #[tokio::test]
    async fn a_complete_batch_that_leaves_a_claimed_item_out_completes_its_claim() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &[]),
                listed("gh:acme/web#2", &[]),
            ]),
        )
        .await;
        let held = item(&env, "gh:acme/web#2").await.id;
        claim(&env, &env.bob, held, &["billing"], None).await;
        let before = env.events().await.len();
        let report = import(&env, complete(vec![listed("gh:acme/web#1", &[])])).await;
        assert_eq!(report.closed, [held]);
        let kinds = env.kinds_after(before).await;
        assert_completed(&env, "gh:acme/web#2", env.bob.actor.member, &kinds).await;
        assert_eq!(
            messages(&env, &env.bob).await.last().unwrap().0,
            "Done in the tracker: Work on gh:acme/web#2"
        );
    }

    #[tokio::test]
    async fn a_claim_on_an_item_its_tracker_closed_before_is_completed_by_the_next_import_unchanged_as_it_is()
     {
        let env = Env::new().await;
        import(&env, batch(vec![listed("gh:acme/web#1", &[])])).await;
        let view = batch(vec![closed(listed("gh:acme/web#1", &[]))]);
        import(&env, view.clone()).await;
        let held = item(&env, "gh:acme/web#1").await;
        assert!(held.closed);
        assert_eq!(
            held.status,
            ItemStatus::Open,
            "an open item stays claimable"
        );
        claim(&env, &env.bob, held.id, &["billing"], None).await;
        let before = env.events().await.len();
        let report = import(&env, view).await;
        assert_eq!(
            report.items[0].change,
            ItemChange::Unchanged { id: held.id },
            "the import reports what it reported before"
        );
        let kinds = env.kinds_after(before).await;
        assert_completed(&env, "gh:acme/web#1", env.bob.actor.member, &kinds).await;
    }

    #[tokio::test]
    async fn a_claim_in_review_and_a_stale_claim_are_completed_too() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &[]),
                listed("gh:acme/web#2", &[]),
            ]),
        )
        .await;
        let reviewed = item(&env, "gh:acme/web#1").await.id;
        let stale = item(&env, "gh:acme/web#2").await.id;
        claim(&env, &env.bob, reviewed, &["billing"], None).await;
        StartReview::new(&env.deps)
            .execute(
                &env.bob.actor,
                StartReviewInput {
                    item: reviewed,
                    pr: PrRef::parse("acme/web#7").unwrap(),
                    more: true,
                },
            )
            .await
            .unwrap();
        claim(&env, &env.alice, stale, &["search"], None).await;
        env.advance(Duration::hours(25));
        assert!(
            stored_claim(&env, reviewed)
                .await
                .unwrap()
                .state
                .is_in_review()
        );
        let before = env.events().await.len();
        import(
            &env,
            batch(vec![
                closed(listed("gh:acme/web#1", &[])),
                closed(listed("gh:acme/web#2", &[])),
            ]),
        )
        .await;
        let kinds = env.kinds_after(before).await;
        assert_completed(&env, "gh:acme/web#1", env.bob.actor.member, &kinds).await;
        assert_completed(&env, "gh:acme/web#2", env.alice.actor.member, &kinds).await;
    }

    /// Asserts that the claim on the item is the one it was, the item still claimed and marked
    /// closed, no end recorded among `kinds` and no message sent to `holder` since `told`.
    async fn assert_claim_stands(
        env: &Env,
        text: &str,
        claim_before: Option<Claim>,
        kinds: &[EventKind],
        holder: &Person,
        told: usize,
    ) {
        let item = item(env, text).await;
        assert!(claim_before.is_some());
        assert_eq!(
            stored_claim(env, item.id).await,
            claim_before,
            "the claim stands"
        );
        assert_eq!(item.status, ItemStatus::Claimed);
        assert!(item.closed, "the item is marked closed upstream");
        assert!(
            !names(kinds).contains(&"completed_by_close"),
            "no completion: {kinds:?}"
        );
        assert_eq!(messages(env, holder).await.len(), told, "nobody is told");
    }

    #[tokio::test]
    async fn an_item_its_own_source_reports_closed_only_as_a_blocker_has_its_claim_completed() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/widgets#40", &[]),
                listed("gh:acme/widgets#41", &["gh:acme/widgets#40"]),
            ]),
        )
        .await;
        let held = item(&env, "gh:acme/widgets#40").await;
        assert_eq!(held.source, Some(SourceId::parse("board").unwrap()));
        claim(&env, &env.bob, held.id, &["billing"], None).await;
        let told = messages(&env, &env.bob).await.len();
        let before = env.events().await.len();
        import(
            &env,
            complete(vec![
                listed("gh:acme/widgets#41", &["gh:acme/widgets#40"]),
                blocker("gh:acme/widgets#40", true),
            ]),
        )
        .await;
        let kinds = env.kinds_after(before).await;
        assert_completed(&env, "gh:acme/widgets#40", env.bob.actor.member, &kinds).await;
        assert_eq!(
            names(&kinds)
                .iter()
                .filter(|name| **name == "completed_by_close")
                .count(),
            1,
            "{kinds:?}"
        );
        let new = messages(&env, &env.bob).await.split_off(told);
        assert_eq!(new.len(), 1, "one message to the holder: {new:?}");
        assert_eq!(new[0].0, "Done in the tracker: Work on gh:acme/widgets#40");
    }

    #[tokio::test]
    async fn an_item_no_source_lists_reported_closed_only_as_a_blocker_keeps_its_claim() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &["gh:acme/api#8"]),
                blocker("gh:acme/api#8", false),
            ]),
        )
        .await;
        let held = item(&env, "gh:acme/api#8").await.id;
        claim(&env, &env.bob, held, &["billing"], None).await;
        let claim_before = stored_claim(&env, held).await;
        let told = messages(&env, &env.bob).await.len();
        let before = env.events().await.len();
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &["gh:acme/api#8"]),
                blocker("gh:acme/api#8", true),
            ]),
        )
        .await;
        let kinds = env.kinds_after(before).await;
        assert_claim_stands(&env, "gh:acme/api#8", claim_before, &kinds, &env.bob, told).await;
    }

    #[tokio::test]
    async fn an_item_a_person_made_closed_only_as_a_blocker_keeps_its_claim_and_comes_back_open() {
        let env = Env::new().await;
        let held = env.item("gh:acme/api#8").await;
        claim(&env, &env.bob, held, &["billing"], None).await;
        let claim_before = stored_claim(&env, held).await;
        let told = messages(&env, &env.bob).await.len();
        let before = env.events().await.len();
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &["gh:acme/api#8"]),
                blocker("gh:acme/api#8", true),
            ]),
        )
        .await;
        let kinds = env.kinds_after(before).await;
        assert_claim_stands(
            &env,
            "gh:acme/api#8",
            claim_before.clone(),
            &kinds,
            &env.bob,
            told,
        )
        .await;
        assert_eq!(item(&env, "gh:acme/api#8").await.source, None);
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &["gh:acme/api#8"]),
                blocker("gh:acme/api#8", false),
            ]),
        )
        .await;
        let reopened = item(&env, "gh:acme/api#8").await;
        assert!(!reopened.closed);
        assert_eq!(reopened.status, ItemStatus::Claimed);
        assert_eq!(stored_claim(&env, held).await, claim_before);
    }

    #[tokio::test]
    async fn a_claim_on_an_item_one_source_lists_survives_another_carrying_it_as_a_closed_blocker()
    {
        let env = Env::new().await;
        let tracker = |items| {
            ImportBatch::new(Some(SourceId::parse("tracker").unwrap()), false, items).unwrap()
        };
        import(&env, tracker(vec![listed("gh:acme/api#8", &[])])).await;
        let held = item(&env, "gh:acme/api#8").await.id;
        claim(&env, &env.bob, held, &["billing"], None).await;
        let claim_before = stored_claim(&env, held).await;
        let told = messages(&env, &env.bob).await.len();
        let before = env.events().await.len();
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &["gh:acme/api#8"]),
                blocker("gh:acme/api#8", true),
            ]),
        )
        .await;
        let kinds = env.kinds_after(before).await;
        assert_claim_stands(
            &env,
            "gh:acme/api#8",
            claim_before.clone(),
            &kinds,
            &env.bob,
            told,
        )
        .await;
        import(&env, tracker(vec![listed("gh:acme/api#8", &[])])).await;
        let listed_again = item(&env, "gh:acme/api#8").await;
        assert!(!listed_again.closed, "its own source has it open");
        assert_eq!(listed_again.status, ItemStatus::Claimed);
        assert_eq!(stored_claim(&env, held).await, claim_before);
    }

    #[tokio::test]
    async fn a_listed_item_reported_closed_by_a_batch_naming_no_source_keeps_its_claim() {
        let env = Env::new().await;
        import(&env, batch(vec![listed("gh:acme/web#1", &[])])).await;
        let held = item(&env, "gh:acme/web#1").await.id;
        claim(&env, &env.bob, held, &["billing"], None).await;
        let claim_before = stored_claim(&env, held).await;
        let told = messages(&env, &env.bob).await.len();
        let before = env.events().await.len();
        let sourceless =
            ImportBatch::new(None, false, vec![closed(listed("gh:acme/web#1", &[]))]).unwrap();
        import(&env, sourceless).await;
        let kinds = env.kinds_after(before).await;
        assert_claim_stands(&env, "gh:acme/web#1", claim_before, &kinds, &env.bob, told).await;
    }

    #[tokio::test]
    async fn an_imported_item_completed_by_close_and_reported_open_again_is_claimed_again() {
        let env = Env::new().await;
        import(&env, batch(vec![listed("gh:acme/web#1", &[])])).await;
        let held = item(&env, "gh:acme/web#1").await.id;
        claim(&env, &env.bob, held, &["billing"], None).await;
        import(&env, batch(vec![closed(listed("gh:acme/web#1", &[]))])).await;
        assert_eq!(item(&env, "gh:acme/web#1").await.status, ItemStatus::Done);
        import(&env, batch(vec![listed("gh:acme/web#1", &[])])).await;
        claim(&env, &env.alice, held, &["billing"], None).await;
        let back = item(&env, "gh:acme/web#1").await;
        assert_eq!(back.status, ItemStatus::Claimed, "the claim brings it back");
        assert_eq!(
            stored_claim(&env, held).await.map(|claim| claim.member),
            Some(env.alice.actor.member)
        );
    }

    #[tokio::test]
    async fn a_complete_batch_completes_a_claim_made_on_an_item_it_closed_while_nobody_held_it() {
        let env = Env::new().await;
        import(
            &env,
            batch(vec![
                listed("gh:acme/web#1", &[]),
                listed("gh:acme/web#5", &[]),
            ]),
        )
        .await;
        let view = complete(vec![listed("gh:acme/web#1", &[])]);
        import(&env, view.clone()).await;
        let gone = item(&env, "gh:acme/web#5").await;
        assert!(gone.closed);
        assert_eq!(
            gone.status,
            ItemStatus::Open,
            "an open item stays claimable"
        );
        claim(&env, &env.bob, gone.id, &["billing"], None).await;
        let told = messages(&env, &env.bob).await.len();
        let before = env.events().await.len();
        let report = import(&env, view.clone()).await;
        assert!(report.closed.is_empty(), "closed already, not closed again");
        let kinds = env.kinds_after(before).await;
        assert_completed(&env, "gh:acme/web#5", env.bob.actor.member, &kinds).await;
        assert!(
            !names(&kinds).contains(&"item_updated"),
            "its facts already said closed: {kinds:?}"
        );
        let new = messages(&env, &env.bob).await.split_off(told);
        assert_eq!(new.len(), 1, "one message to the holder: {new:?}");
        assert_eq!(new[0].0, "Done in the tracker: Work on gh:acme/web#5");
        let events = env.events().await.len();
        let wakes = env.fakes.notifier.calls().len();
        import(&env, view).await;
        assert_eq!(
            env.events().await.len(),
            events,
            "the repeat appends nothing"
        );
        assert_eq!(env.fakes.notifier.calls().len(), wakes, "and wakes nobody");
    }

    #[tokio::test]
    async fn the_same_batch_again_after_it_completed_a_claim_appends_nothing_and_wakes_nobody() {
        let env = Env::new().await;
        import(&env, batch(vec![listed("gh:acme/web#1", &[])])).await;
        let held = item(&env, "gh:acme/web#1").await.id;
        claim(&env, &env.bob, held, &["billing"], None).await;
        let view = batch(vec![closed(listed("gh:acme/web#1", &[]))]);
        import(&env, view.clone()).await;
        assert_eq!(stored_claim(&env, held).await, None);
        let events = env.events().await.len();
        let wakes = env.fakes.notifier.calls().len();
        import(&env, view).await;
        assert_eq!(
            env.events().await.len(),
            events,
            "the repeat appends nothing"
        );
        assert_eq!(env.fakes.notifier.calls().len(), wakes, "and wakes nobody");
    }

    /// Imports `gh:acme/widgets#7` and `gh:acme/widgets#42`; alice's debt claim on the first holds
    /// billing and search, and bob's feature claim on the second waits for billing. Answers the
    /// two items.
    async fn bob_waits_for_alices_debt(env: &Env) -> (ItemId, ItemId) {
        import(
            env,
            batch(vec![
                listed("gh:acme/widgets#7", &[]),
                listed("gh:acme/widgets#42", &[]),
            ]),
        )
        .await;
        let debt = item(env, "gh:acme/widgets#7").await.id;
        let feature = item(env, "gh:acme/widgets#42").await.id;
        claim(
            env,
            &env.alice,
            debt,
            &["billing", "search"],
            Some(ClaimKind::Debt),
        )
        .await;
        claim(env, &env.bob, feature, &["billing"], None).await;
        assert!(!stored_claim(env, feature).await.unwrap().pending.is_empty());
        (debt, feature)
    }

    fn ends_of_a_wait(kinds: &[EventKind]) -> usize {
        kinds
            .iter()
            .filter(|kind| {
                matches!(
                    kind,
                    EventKind::AreaHandedOver(_) | EventKind::WaitWithdrawn(_)
                )
            })
            .count()
    }

    #[tokio::test]
    async fn one_batch_closing_a_debt_claim_and_the_claim_waiting_for_it_ends_the_wait_once() {
        let env = Env::new().await;
        bob_waits_for_alices_debt(&env).await;
        let before = env.events().await.len();
        import(
            &env,
            batch(vec![
                closed(listed("gh:acme/widgets#7", &[])),
                closed(listed("gh:acme/widgets#42", &[])),
            ]),
        )
        .await;
        let kinds = env.kinds_after(before).await;
        assert_completed(&env, "gh:acme/widgets#7", env.alice.actor.member, &kinds).await;
        assert_completed(&env, "gh:acme/widgets#42", env.bob.actor.member, &kinds).await;
        assert_eq!(ends_of_a_wait(&kinds), 1, "{kinds:?}");
    }

    #[tokio::test]
    async fn a_claim_waiting_for_an_area_of_the_completed_claim_gets_it() {
        let env = Env::new().await;
        let (debt, feature) = bob_waits_for_alices_debt(&env).await;
        let before = env.events().await.len();
        import(&env, batch(vec![closed(listed("gh:acme/widgets#7", &[]))])).await;
        let kinds = env.kinds_after(before).await;
        assert_completed(&env, "gh:acme/widgets#7", env.alice.actor.member, &kinds).await;
        let billing = QualifiedArea::parse("default:billing").unwrap();
        assert!(kinds.contains(&EventKind::AreaHandedOver(AreaPassing {
            area: billing.clone(),
            from: debt,
            to: feature,
        })));
        let bobs = stored_claim(&env, feature).await.unwrap();
        assert!(bobs.holds(&billing));
        assert!(bobs.pending.is_empty());
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
