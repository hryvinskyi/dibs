//! An importer's batch: the items an outside source lists, with the blockers they wait for, and
//! the plan that brings the project's items and imported dependencies in line with it.
//!
//! The plan changes only what differs from what is stored, so a batch equal to the stored state
//! plans nothing. It never touches a claim. An importer sends its whole view again and again; each
//! rule below keeps a repeated or partial view from doing harm:
//!
//! - an item the source closed that the project does not know is not created;
//! - an edge to a reference neither the project nor the batch knows is refused on its own, while a
//!   closed blocker the project does not know blocks nothing and is dropped;
//! - only the importer's own edges of an item are replaced, so an edge a person added stays;
//! - an edge that would close a cycle is refused on its own, and the rest of the batch applies;
//! - a batch that read the whole source closes that source's open items it did not mention.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::dependency::{Dependency, DependencyRefused, DependencySource, add_dependency};
use crate::ids::ItemId;
use crate::item::{
    CreateRefused, DroppedHint, ExternalRef, ItemMeta, ItemRepository, ItemStatus, Priority,
    SourceId, SourceStatus, Title, decide_create_item, resolve_hint,
};
use crate::member::Role;
use crate::ref_scheme::RefSchemeRegistry;
use crate::settings::ProjectSettings;

/// The most items one batch may report.
pub const MAX_BATCH_ITEMS: usize = 500;

/// What a source lists about one of its items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// The areas the work will probably touch, as the source names them.
    pub areas_hint: Vec<String>,
    /// How soon.
    pub priority: Priority,
    /// The source's own state of it.
    pub source_status: Option<SourceStatus>,
    /// The items it waits for.
    pub blocked_by: Vec<ExternalRef>,
}

/// One item of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedItem {
    /// Its reference.
    pub reference: ExternalRef,
    /// Its title.
    pub title: Title,
    /// Whether the source closed it.
    pub closed: bool,
    /// What the source lists about it; `None` for a blocker the batch carries only because a listed
    /// item waits for it, which belongs to no source and whose own edges are left as they are.
    pub listing: Option<Listing>,
}

/// Why a batch is refused as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BatchRefused {
    /// More items than one batch may report.
    #[error("a batch reports at most {max} items; send the rest in another batch")]
    TooMany {
        /// The most a batch may report.
        max: usize,
    },
    /// A batch said it read the whole source without naming the source.
    #[error("a batch marked complete names the source it read completely")]
    CompleteWithoutSource,
    /// The batch reports one reference twice.
    #[error("the batch reports {reference} more than once")]
    Duplicate {
        /// The reference.
        reference: ExternalRef,
    },
}

/// A batch an importer sends: the items of `source` it read, and whether it read all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBatch {
    source: Option<SourceId>,
    complete: bool,
    items: Vec<ReportedItem>,
}

impl ImportBatch {
    /// A batch of at most [`MAX_BATCH_ITEMS`] items, each reference once. `complete` says that
    /// every page of `source` was read without an error; it needs the source.
    ///
    /// # Errors
    /// [`BatchRefused`] naming the rule the batch breaks.
    pub fn new(
        source: Option<SourceId>,
        complete: bool,
        items: Vec<ReportedItem>,
    ) -> Result<Self, BatchRefused> {
        if items.len() > MAX_BATCH_ITEMS {
            return Err(BatchRefused::TooMany {
                max: MAX_BATCH_ITEMS,
            });
        }
        if complete && source.is_none() {
            return Err(BatchRefused::CompleteWithoutSource);
        }
        let mut seen = BTreeSet::new();
        for item in &items {
            if !seen.insert(&item.reference) {
                return Err(BatchRefused::Duplicate {
                    reference: item.reference.clone(),
                });
            }
        }
        Ok(Self {
            source,
            complete,
            items,
        })
    }

    /// The source the listed items come from.
    pub fn source(&self) -> Option<&SourceId> {
        self.source.as_ref()
    }

    /// Whether the batch is the whole source.
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// The items, in the order reported.
    pub fn items(&self) -> &[ReportedItem] {
        &self.items
    }
}

/// An item the project has, as the plan compares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownItem {
    /// Its id.
    pub id: ItemId,
    /// Its reference.
    pub reference: Option<ExternalRef>,
    /// Where it stands.
    pub status: ItemStatus,
    /// Its repository.
    pub repository: ItemRepository,
    /// Its facts.
    pub meta: ItemMeta,
    /// The source it was listed by.
    pub source: Option<SourceId>,
}

/// What the plan does with one reported item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemChange {
    /// Creates it.
    Create {
        /// Its new id.
        id: ItemId,
        /// Its repository.
        repository: ItemRepository,
        /// Its facts.
        meta: ItemMeta,
        /// The source listing it.
        source: Option<SourceId>,
    },
    /// Writes facts that differ from the stored ones.
    Update {
        /// The item.
        id: ItemId,
        /// Its facts now.
        meta: ItemMeta,
        /// The source listing it now.
        source: Option<SourceId>,
    },
    /// Changes nothing: the stored item already says what the batch says.
    Unchanged {
        /// The item.
        id: ItemId,
    },
    /// Creates nothing: the source closed an item the project does not know.
    SkippedClosed,
    /// Creates nothing: the caller may not create it.
    Refused(CreateRefused),
}

impl ItemChange {
    /// The item's id, when it exists or is about to.
    pub fn id(&self) -> Option<ItemId> {
        match self {
            Self::Create { id, .. } | Self::Update { id, .. } | Self::Unchanged { id } => Some(*id),
            Self::SkippedClosed | Self::Refused(_) => None,
        }
    }
}

/// One reported item and what the plan does with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedItem {
    /// Its reference.
    pub reference: ExternalRef,
    /// What happens to it.
    pub change: ItemChange,
    /// The areas of its hint left out.
    pub dropped_hints: Vec<DroppedHint>,
}

/// Why one edge of a batch is not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EdgeRefused {
    /// Neither the project nor the batch knows the blocker.
    #[error("the blocker is neither a work item of the project nor in the batch")]
    UnknownBlocker,
    /// The item would wait for itself.
    #[error("an item cannot be blocked by itself")]
    SelfDependency,
    /// The edge would close a cycle.
    #[error("the blocker already waits for this item, directly or through other items")]
    Cycle,
}

/// An edge of a batch that is not recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedEdge {
    /// The item that would wait.
    pub item: ExternalRef,
    /// The item it would wait for.
    pub blocked_by: ExternalRef,
    /// Why not.
    pub reason: EdgeRefused,
}

/// An open item of the source that a complete batch did not mention, closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closure {
    /// The item.
    pub id: ItemId,
    /// Its facts, closed.
    pub meta: ItemMeta,
}

/// Everything a batch changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPlan {
    /// Each reported item, in the order reported.
    pub items: Vec<PlannedItem>,
    /// The source's items closed because a complete batch did not mention them.
    pub closed: Vec<Closure>,
    /// The imported edges to add.
    pub added: Vec<Dependency>,
    /// The imported edges to remove.
    pub removed: Vec<Dependency>,
    /// The edges not recorded, each with why.
    pub refused_edges: Vec<RefusedEdge>,
}

/// What a batch is compared with.
#[derive(Debug, Clone, Copy)]
pub struct ImportContext<'a> {
    /// Every item of the project.
    pub known: &'a [KnownItem],
    /// Every edge of the project, of both sources.
    pub graph: &'a [Dependency],
    /// The project's settings.
    pub settings: &'a ProjectSettings,
    /// The reference schemes.
    pub schemes: &'a RefSchemeRegistry,
    /// The role of the caller sending the batch.
    pub by: Role,
}

/// Plans a batch. `new_id` hands out the id of each item the plan creates.
pub fn plan_import(
    batch: &ImportBatch,
    context: &ImportContext<'_>,
    new_id: &mut dyn FnMut() -> ItemId,
) -> ImportPlan {
    let known: BTreeMap<&ExternalRef, &KnownItem> = context
        .known
        .iter()
        .filter_map(|item| item.reference.as_ref().map(|reference| (reference, item)))
        .collect();
    let mut plan = ImportPlan::default();
    for reported in batch.items() {
        plan.items.push(plan_item(
            reported,
            known.get(&reported.reference).copied(),
            batch,
            context,
            new_id,
        ));
    }
    plan_edges(batch, context, &mut plan);
    if batch.complete()
        && let Some(source) = batch.source()
    {
        let mentioned: BTreeSet<&ExternalRef> =
            batch.items().iter().map(|item| &item.reference).collect();
        plan.closed = context
            .known
            .iter()
            .filter(|item| {
                item.source.as_ref() == Some(source)
                    && !item.meta.closed
                    && matches!(item.status, ItemStatus::Open | ItemStatus::Claimed)
                    && item
                        .reference
                        .as_ref()
                        .is_none_or(|reference| !mentioned.contains(reference))
            })
            .map(|item| Closure {
                id: item.id,
                meta: ItemMeta {
                    closed: true,
                    ..item.meta.clone()
                },
            })
            .collect();
    }
    plan
}

fn plan_item(
    reported: &ReportedItem,
    known: Option<&KnownItem>,
    batch: &ImportBatch,
    context: &ImportContext<'_>,
    new_id: &mut dyn FnMut() -> ItemId,
) -> PlannedItem {
    let reference = reported.reference.clone();
    let Some(known) = known else {
        if reported.closed {
            return PlannedItem {
                reference,
                change: ItemChange::SkippedClosed,
                dropped_hints: Vec::new(),
            };
        }
        let repository = match decide_create_item(
            Some(&reference),
            context.by,
            context.settings,
            context.schemes,
        ) {
            Ok(repository) => repository,
            Err(refused) => {
                return PlannedItem {
                    reference,
                    change: ItemChange::Refused(refused),
                    dropped_hints: Vec::new(),
                };
            }
        };
        let base = ItemMeta::titled(reported.title.clone());
        let (meta, dropped_hints) = reported_meta(reported, &base, &repository, context.settings);
        let source = reported.listing.as_ref().and(batch.source().cloned());
        return PlannedItem {
            reference,
            change: ItemChange::Create {
                id: new_id(),
                repository,
                meta,
                source,
            },
            dropped_hints,
        };
    };
    let (meta, dropped_hints) =
        reported_meta(reported, &known.meta, &known.repository, context.settings);
    let source = match (&reported.listing, batch.source()) {
        (Some(_), Some(source)) => Some(source.clone()),
        _ => known.source.clone(),
    };
    let change = if meta == known.meta && source == known.source {
        ItemChange::Unchanged { id: known.id }
    } else {
        ItemChange::Update {
            id: known.id,
            meta,
            source,
        }
    };
    PlannedItem {
        reference,
        change,
        dropped_hints,
    }
}

/// The facts a reported item has after the batch: a listed item's are all the batch's, a
/// blocker's keep the stored ones but for its title and whether it is closed.
fn reported_meta(
    reported: &ReportedItem,
    stored: &ItemMeta,
    repository: &ItemRepository,
    settings: &ProjectSettings,
) -> (ItemMeta, Vec<DroppedHint>) {
    match &reported.listing {
        Some(listing) => {
            let hint = resolve_hint(&listing.areas_hint, repository, settings);
            let meta = ItemMeta {
                title: reported.title.clone(),
                areas_hint: hint.areas,
                priority: listing.priority,
                source_status: listing.source_status.clone(),
                closed: reported.closed,
            };
            (meta, hint.dropped)
        }
        None => {
            let meta = ItemMeta {
                title: reported.title.clone(),
                closed: reported.closed,
                ..stored.clone()
            };
            (meta, Vec::new())
        }
    }
}

/// Replaces the imported edges of every listed item that exists with the ones the batch reports.
fn plan_edges(batch: &ImportBatch, context: &ImportContext<'_>, plan: &mut ImportPlan) {
    let ids: BTreeMap<&ExternalRef, Option<ItemId>> = plan
        .items
        .iter()
        .map(|planned| (&planned.reference, planned.change.id()))
        .collect();
    let known_ids: BTreeMap<&ExternalRef, ItemId> = context
        .known
        .iter()
        .filter_map(|item| {
            item.reference
                .as_ref()
                .map(|reference| (reference, item.id))
        })
        .collect();
    let mut wanted: Vec<(ItemId, Vec<ItemId>)> = Vec::new();
    let mut refused = Vec::new();
    for (reported, planned) in batch.items().iter().zip(&plan.items) {
        let (Some(listing), Some(item)) = (&reported.listing, planned.change.id()) else {
            continue;
        };
        let mut blockers: Vec<ItemId> = Vec::new();
        for blocker in &listing.blocked_by {
            let refuse = |reason| RefusedEdge {
                item: reported.reference.clone(),
                blocked_by: blocker.clone(),
                reason,
            };
            if *blocker == reported.reference {
                refused.push(refuse(EdgeRefused::SelfDependency));
                continue;
            }
            match ids.get(blocker) {
                Some(Some(id)) => push_once(&mut blockers, *id),
                Some(None) if closed_in_batch(batch, blocker) => {}
                Some(None) => refused.push(refuse(EdgeRefused::UnknownBlocker)),
                None => match known_ids.get(blocker) {
                    Some(id) => push_once(&mut blockers, *id),
                    None => refused.push(refuse(EdgeRefused::UnknownBlocker)),
                },
            }
        }
        wanted.push((item, blockers));
    }
    let mut graph: Vec<Dependency> = context.graph.to_vec();
    for (item, blockers) in &wanted {
        let (gone, kept): (Vec<Dependency>, Vec<Dependency>) =
            graph.into_iter().partition(|edge| {
                edge.item == *item
                    && edge.source == DependencySource::Imported
                    && !blockers.contains(&edge.blocked_by)
            });
        plan.removed.extend(gone);
        graph = kept;
    }
    let reference_of: BTreeMap<ItemId, &ExternalRef> = ids
        .iter()
        .filter_map(|(reference, id)| id.map(|id| (id, *reference)))
        .collect();
    let known_reference: BTreeMap<ItemId, &ExternalRef> = known_ids
        .iter()
        .map(|(reference, id)| (*id, *reference))
        .collect();
    for (item, blockers) in &wanted {
        for blocker in blockers {
            if graph
                .iter()
                .any(|edge| edge.item == *item && edge.blocked_by == *blocker)
            {
                continue;
            }
            let edge = Dependency {
                item: *item,
                blocked_by: *blocker,
                source: DependencySource::Imported,
            };
            match add_dependency(&graph, &edge) {
                Ok(()) => {
                    graph.push(edge);
                    plan.added.push(edge);
                }
                Err(reason) => {
                    let name = |id: &ItemId| {
                        reference_of
                            .get(id)
                            .or_else(|| known_reference.get(id))
                            .map(|reference| (*reference).clone())
                    };
                    if let (Some(item), Some(blocked_by)) = (name(item), name(blocker)) {
                        refused.push(RefusedEdge {
                            item,
                            blocked_by,
                            reason: match reason {
                                DependencyRefused::SelfDependency => EdgeRefused::SelfDependency,
                                DependencyRefused::Cycle => EdgeRefused::Cycle,
                            },
                        });
                    }
                }
            }
        }
    }
    plan.refused_edges = refused;
}

fn push_once(blockers: &mut Vec<ItemId>, id: ItemId) {
    if !blockers.contains(&id) {
        blockers.push(id);
    }
}

/// Whether the batch reports this reference as closed.
fn closed_in_batch(batch: &ImportBatch, reference: &ExternalRef) -> bool {
    batch
        .items()
        .iter()
        .any(|item| item.reference == *reference && item.closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::two_repo_settings;

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
            reference: reference(text),
            title: Title::parse(&format!("Work on {text}")).unwrap(),
            closed,
            listing: None,
        }
    }

    fn board() -> Option<SourceId> {
        Some(SourceId::parse("board").unwrap())
    }

    fn batch(items: Vec<ReportedItem>) -> ImportBatch {
        ImportBatch::new(board(), false, items).unwrap()
    }

    fn known(id: u64, text: &str, source: Option<SourceId>) -> KnownItem {
        KnownItem {
            id: ItemId(id),
            reference: Some(reference(text)),
            status: ItemStatus::Open,
            repository: ItemRepository::Unnamed,
            meta: ItemMeta::titled(Title::parse(&format!("Work on {text}")).unwrap()),
            source,
        }
    }

    struct Planned {
        plan: ImportPlan,
    }

    fn plan(batch: &ImportBatch, known: &[KnownItem], graph: &[Dependency]) -> Planned {
        let settings = crate::testing::settings(
            r#"{"areas":[{"name":"billing","paths":["packages/billing/**"]}]}"#,
        );
        plan_with(batch, known, graph, &settings, Role::Importer)
    }

    fn plan_with(
        batch: &ImportBatch,
        known: &[KnownItem],
        graph: &[Dependency],
        settings: &ProjectSettings,
        by: Role,
    ) -> Planned {
        let schemes = RefSchemeRegistry::standard();
        let mut next = 100;
        let mut ids = || {
            next += 1;
            ItemId(next)
        };
        let context = ImportContext {
            known,
            graph,
            settings,
            schemes: &schemes,
            by,
        };
        Planned {
            plan: plan_import(batch, &context, &mut ids),
        }
    }

    fn edge(item: u64, blocked_by: u64, source: DependencySource) -> Dependency {
        Dependency {
            item: ItemId(item),
            blocked_by: ItemId(blocked_by),
            source,
        }
    }

    #[test]
    fn a_batch_is_bounded_unique_and_complete_only_for_a_named_source() {
        let many: Vec<ReportedItem> = (0..=MAX_BATCH_ITEMS)
            .map(|n| listed(&format!("gh:acme/web#{n}"), &[]))
            .collect();
        assert_eq!(
            ImportBatch::new(board(), false, many),
            Err(BatchRefused::TooMany {
                max: MAX_BATCH_ITEMS
            })
        );
        assert_eq!(
            ImportBatch::new(None, true, Vec::new()),
            Err(BatchRefused::CompleteWithoutSource)
        );
        assert_eq!(
            ImportBatch::new(
                board(),
                false,
                vec![listed("gh:acme/web#1", &[]), listed("gh:Acme/Web#1", &[])]
            ),
            Err(BatchRefused::Duplicate {
                reference: reference("gh:acme/web#1")
            })
        );
        assert!(ImportBatch::new(board(), true, Vec::new()).is_ok());
    }

    #[test]
    fn new_items_are_created_from_the_source_with_its_facts() {
        let mut item = listed("gh:acme/web#1", &[]);
        if let Some(listing) = item.listing.as_mut() {
            listing.priority = Priority::new(0).unwrap();
            listing.areas_hint = vec!["billing".to_owned(), "nowhere".to_owned()];
            listing.source_status = Some(SourceStatus::parse("Todo").unwrap());
        }
        let planned = plan(&batch(vec![item]), &[], &[]).plan;
        let ItemChange::Create {
            id, meta, source, ..
        } = &planned.items[0].change
        else {
            panic!("not a creation: {planned:?}");
        };
        assert_eq!(*id, ItemId(101));
        assert_eq!(meta.priority.get(), 0);
        assert_eq!(
            meta.areas_hint
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["default:billing"]
        );
        assert_eq!(
            meta.source_status.as_ref().map(SourceStatus::as_str),
            Some("Todo")
        );
        assert_eq!(source, &board());
        assert_eq!(planned.items[0].dropped_hints[0].text, "nowhere");
    }

    #[test]
    fn a_batch_equal_to_the_stored_items_changes_nothing() {
        let stored = [
            known(1, "gh:acme/web#1", board()),
            known(2, "gh:acme/web#2", board()),
        ];
        let graph = [edge(1, 2, DependencySource::Imported)];
        let planned = plan(
            &batch(vec![
                listed("gh:acme/web#1", &["gh:acme/web#2"]),
                listed("gh:acme/web#2", &[]),
            ]),
            &stored,
            &graph,
        )
        .plan;
        assert!(
            planned
                .items
                .iter()
                .all(|item| matches!(item.change, ItemChange::Unchanged { .. })),
            "{planned:?}"
        );
        assert!(planned.added.is_empty() && planned.removed.is_empty());
        assert!(planned.refused_edges.is_empty() && planned.closed.is_empty());
    }

    #[test]
    fn a_changed_fact_updates_the_item() {
        let stored = [known(1, "gh:acme/web#1", board())];
        let mut changed = listed("gh:acme/web#1", &[]);
        changed.title = Title::parse("A new title").unwrap();
        let planned = plan(&batch(vec![changed]), &stored, &[]).plan;
        let ItemChange::Update { id, meta, .. } = &planned.items[0].change else {
            panic!("not an update: {planned:?}");
        };
        assert_eq!((*id, meta.title.as_str()), (ItemId(1), "A new title"));
    }

    #[test]
    fn a_closed_item_the_project_does_not_know_is_skipped_and_a_known_one_flips() {
        let mut gone = listed("gh:acme/web#9", &[]);
        gone.closed = true;
        let mut known_closed = listed("gh:acme/web#1", &[]);
        known_closed.closed = true;
        let stored = [known(1, "gh:acme/web#1", board())];
        let planned = plan(&batch(vec![gone, known_closed]), &stored, &[]).plan;
        assert_eq!(planned.items[0].change, ItemChange::SkippedClosed);
        let ItemChange::Update { meta, .. } = &planned.items[1].change else {
            panic!("not an update: {planned:?}");
        };
        assert!(meta.closed);
    }

    #[test]
    fn an_unknown_blocker_is_refused_alone_unless_the_batch_carries_it() {
        let planned = plan(
            &batch(vec![
                listed(
                    "gh:acme/web#1",
                    &["gh:acme/api#7", "gh:acme/web#2", "gh:acme/api#8"],
                ),
                listed("gh:acme/web#2", &[]),
                blocker("gh:acme/api#8", false),
            ]),
            &[],
            &[],
        )
        .plan;
        assert_eq!(
            planned.refused_edges,
            [RefusedEdge {
                item: reference("gh:acme/web#1"),
                blocked_by: reference("gh:acme/api#7"),
                reason: EdgeRefused::UnknownBlocker,
            }]
        );
        let ids: Vec<ItemId> = planned
            .items
            .iter()
            .filter_map(|item| item.change.id())
            .collect();
        assert_eq!(
            planned.added,
            [
                edge(ids[0].get(), ids[1].get(), DependencySource::Imported),
                edge(ids[0].get(), ids[2].get(), DependencySource::Imported),
            ]
        );
        let ItemChange::Create { source, .. } = &planned.items[2].change else {
            panic!("the blocker is created: {planned:?}");
        };
        assert_eq!(source, &None, "a blocker belongs to no source");
    }

    #[test]
    fn a_closed_blocker_the_project_does_not_know_blocks_nothing() {
        let planned = plan(
            &batch(vec![
                listed("gh:acme/web#1", &["gh:acme/api#8"]),
                blocker("gh:acme/api#8", true),
            ]),
            &[],
            &[],
        )
        .plan;
        assert_eq!(planned.items[1].change, ItemChange::SkippedClosed);
        assert!(planned.added.is_empty());
        assert!(planned.refused_edges.is_empty());
    }

    #[test]
    fn a_blocker_keeps_its_stored_facts_but_its_title_and_state() {
        let mut stored = known(2, "gh:acme/api#8", board());
        stored.meta.priority = Priority::new(0).unwrap();
        let mut report = blocker("gh:acme/api#8", true);
        report.title = Title::parse("Renamed").unwrap();
        let planned = plan(&batch(vec![report]), &[stored], &[]).plan;
        let ItemChange::Update { meta, source, .. } = &planned.items[0].change else {
            panic!("not an update: {planned:?}");
        };
        assert_eq!(
            (meta.priority.get(), meta.closed, meta.title.as_str()),
            (0, true, "Renamed")
        );
        assert_eq!(source, &board());
    }

    #[test]
    fn only_imported_edges_are_replaced_and_a_manual_one_survives() {
        let stored = [
            known(1, "gh:acme/web#1", board()),
            known(2, "gh:acme/web#2", board()),
            known(3, "gh:acme/web#3", board()),
        ];
        let graph = [
            edge(1, 2, DependencySource::Imported),
            edge(1, 3, DependencySource::Manual),
        ];
        let planned = plan(&batch(vec![listed("gh:acme/web#1", &[])]), &stored, &graph).plan;
        assert_eq!(planned.removed, [edge(1, 2, DependencySource::Imported)]);
        assert!(planned.added.is_empty());
    }

    #[test]
    fn an_imported_edge_a_manual_one_already_records_adds_nothing() {
        let stored = [
            known(1, "gh:acme/web#1", board()),
            known(2, "gh:acme/web#2", board()),
        ];
        let graph = [edge(1, 2, DependencySource::Manual)];
        let planned = plan(
            &batch(vec![listed("gh:acme/web#1", &["gh:acme/web#2"])]),
            &stored,
            &graph,
        )
        .plan;
        assert!(planned.added.is_empty() && planned.removed.is_empty());
    }

    #[test]
    fn an_edge_closing_a_cycle_is_refused_and_the_rest_applies() {
        let stored = [
            known(1, "gh:acme/web#1", board()),
            known(2, "gh:acme/web#2", board()),
            known(3, "gh:acme/web#3", board()),
        ];
        let graph = [edge(2, 1, DependencySource::Manual)];
        let planned = plan(
            &batch(vec![
                listed("gh:acme/web#1", &["gh:acme/web#2", "gh:acme/web#3"]),
                listed("gh:acme/web#3", &["gh:acme/web#3"]),
            ]),
            &stored,
            &graph,
        )
        .plan;
        assert_eq!(planned.added, [edge(1, 3, DependencySource::Imported)]);
        assert_eq!(
            planned.refused_edges,
            [
                RefusedEdge {
                    item: reference("gh:acme/web#3"),
                    blocked_by: reference("gh:acme/web#3"),
                    reason: EdgeRefused::SelfDependency,
                },
                RefusedEdge {
                    item: reference("gh:acme/web#1"),
                    blocked_by: reference("gh:acme/web#2"),
                    reason: EdgeRefused::Cycle,
                },
            ]
        );
    }

    #[test]
    fn a_complete_batch_closes_the_sources_open_items_it_did_not_mention() {
        let mut done = known(4, "gh:acme/web#4", board());
        done.status = ItemStatus::Done;
        let stored = [
            known(1, "gh:acme/web#1", board()),
            known(2, "gh:acme/web#2", board()),
            known(3, "gh:acme/web#3", None),
            done,
            known(5, "gh:acme/web#5", Some(SourceId::parse("other").unwrap())),
        ];
        let complete = ImportBatch::new(board(), true, vec![listed("gh:acme/web#1", &[])]).unwrap();
        let planned = plan(&complete, &stored, &[]).plan;
        assert_eq!(planned.closed.len(), 1);
        assert_eq!(planned.closed[0].id, ItemId(2));
        assert!(planned.closed[0].meta.closed);
        let partial = batch(vec![listed("gh:acme/web#1", &[])]);
        assert!(plan(&partial, &stored, &[]).plan.closed.is_empty());
    }

    #[test]
    fn a_person_sending_a_batch_is_refused_an_unlisted_repository_item_by_item() {
        let settings = two_repo_settings();
        let planned = plan_with(
            &batch(vec![listed("gh:acme/elsewhere#1", &[])]),
            &[],
            &[],
            &settings,
            Role::Admin,
        )
        .plan;
        assert!(matches!(planned.items[0].change, ItemChange::Refused(_)));
        let imported = plan_with(
            &batch(vec![listed("gh:acme/elsewhere#1", &[])]),
            &[],
            &[],
            &settings,
            Role::Importer,
        )
        .plan;
        assert!(matches!(
            imported.items[0].change,
            ItemChange::Create { .. }
        ));
    }
}
