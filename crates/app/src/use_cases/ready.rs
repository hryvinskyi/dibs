//! The ready list: the work the caller can pick up now, soonest first, and why each other open item
//! cannot be picked up.

use std::collections::BTreeMap;
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, MemberId};
use dibs_domain::ready::{NotReady, ReadyList, ready_items};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::ProjectSettings;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, NamedItem, Names};
use crate::ports::{Clock, Store, StoreError};
use crate::records::{ItemFilter, ItemRecord};
use crate::work::Work;

/// An item the caller can pick up now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyEntry {
    /// The item, with its creator's handle.
    pub item: NamedItem,
    /// The tracker's page for it.
    pub link: Option<String>,
    /// The member whose claim on it went stale; taking it is an explicit takeover.
    pub stale_holder: Option<Handle>,
}

/// Why an open item cannot be picked up now, its members named by handle and its items by
/// reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamedNotReady {
    /// A member's live claim holds it.
    Claimed {
        /// The holder.
        holder: Handle,
    },
    /// Items it waits for are neither done nor closed.
    BlockedBy {
        /// Those items, in id order.
        items: Vec<ItemName>,
    },
    /// An area of its hint is held by another member's live claim.
    AreaHeld {
        /// The area.
        area: QualifiedArea,
        /// The holder.
        holder: Handle,
        /// The item of the holder's claim.
        item: ItemName,
    },
}

/// An open item that is not ready, with every reason why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedEntry {
    /// The item, with its creator's handle.
    pub item: NamedItem,
    /// The tracker's page for it.
    pub link: Option<String>,
    /// Why it is not ready: its claim first, then its blockers, then the held areas.
    pub reasons: Vec<NamedNotReady>,
}

/// The ready list and the open items left out of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyView {
    /// The items the caller can pick up: by priority, then oldest first.
    pub ready: Vec<ReadyEntry>,
    /// The open items that are not ready, in the same order.
    pub excluded: Vec<ExcludedEntry>,
}

/// Shows the ready list as the caller sees it. Every role may read it, the importer included.
pub struct Ready {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl Ready {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// The ready list now.
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<ReadyView, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let names = Names::load(&mut work).await?;
        let view = ReadyScope {
            me: actor.member,
            now,
            settings: &settings,
            schemes: &self.ref_schemes,
        }
        .view(&mut work, &names)
        .await?;
        Ok(view)
    }
}

/// Whose ready list, when, under which settings, with which reference schemes.
pub(crate) struct ReadyScope<'a> {
    /// The member it is for.
    pub(crate) me: MemberId,
    /// The time it is computed at.
    pub(crate) now: OffsetDateTime,
    /// The project's settings.
    pub(crate) settings: &'a ProjectSettings,
    /// The reference schemes giving the trackers' pages.
    pub(crate) schemes: &'a RefSchemeRegistry,
}

impl ReadyScope<'_> {
    /// The ready list, read inside the transaction's project.
    pub(crate) async fn view(&self, work: &mut Work, names: &Names) -> Result<ReadyView, AppError> {
        let project = work.project_id();
        let items = work.items(project, &ItemFilter::default()).await?;
        let claims = work.claims(project).await?;
        let graph = work.dependencies(project).await?;
        let candidates: Vec<_> = items.iter().map(ItemRecord::candidate).collect();
        let list = ready_items(
            &candidates,
            &claims,
            &graph,
            self.me,
            self.now,
            self.settings,
        );
        let by_id: BTreeMap<ItemId, &ItemRecord> =
            items.iter().map(|item| (item.id, item)).collect();
        Naming {
            by_id,
            names,
            schemes: self.schemes,
        }
        .view(list)
    }
}

/// Names the ids of a ready list.
struct Naming<'a> {
    by_id: BTreeMap<ItemId, &'a ItemRecord>,
    names: &'a Names,
    schemes: &'a RefSchemeRegistry,
}

impl Naming<'_> {
    fn view(&self, list: ReadyList) -> Result<ReadyView, AppError> {
        let ready = list
            .ready
            .into_iter()
            .map(|ready| {
                let record = self.record(ready.item)?;
                Ok(ReadyEntry {
                    link: self.link(record),
                    item: self.names.item(record.clone())?,
                    stale_holder: ready
                        .stale_claim
                        .map(|member| self.names.handle(member))
                        .transpose()?,
                })
            })
            .collect::<Result<_, AppError>>()?;
        let excluded = list
            .excluded
            .into_iter()
            .map(|excluded| {
                let record = self.record(excluded.item)?;
                Ok(ExcludedEntry {
                    link: self.link(record),
                    item: self.names.item(record.clone())?,
                    reasons: excluded
                        .reasons
                        .into_iter()
                        .map(|reason| self.reason(reason))
                        .collect::<Result<_, AppError>>()?,
                })
            })
            .collect::<Result<_, AppError>>()?;
        Ok(ReadyView { ready, excluded })
    }

    fn reason(&self, reason: NotReady) -> Result<NamedNotReady, AppError> {
        Ok(match reason {
            NotReady::Claimed { member } => NamedNotReady::Claimed {
                holder: self.names.handle(member)?,
            },
            NotReady::BlockedBy { items } => NamedNotReady::BlockedBy {
                items: items
                    .into_iter()
                    .map(|id| self.name(id))
                    .collect::<Result<_, _>>()?,
            },
            NotReady::AreaHeld { area, member, item } => NamedNotReady::AreaHeld {
                area,
                holder: self.names.handle(member)?,
                item: self.name(item)?,
            },
        })
    }

    fn record(&self, id: ItemId) -> Result<&ItemRecord, AppError> {
        self.by_id.get(&id).copied().ok_or_else(|| {
            AppError::Store(StoreError::Corrupt {
                what: format!("the ready list names item {id}, which does not exist"),
            })
        })
    }

    fn name(&self, id: ItemId) -> Result<ItemName, AppError> {
        Ok(ItemName::of(self.record(id)?, self.schemes))
    }

    fn link(&self, record: &ItemRecord) -> Option<String> {
        ItemName::of(record, self.schemes).link
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::ids::ItemId;
    use dibs_domain::import::{ImportBatch, Listing, ReportedItem};
    use dibs_domain::item::{ExternalRef, Priority, SourceId, Title};

    use super::*;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::{ClaimItem, ClaimItemInput, Status, UpsertItems};

    fn reported(text: &str, priority: u8, hint: &[&str], blocked_by: &[&str]) -> ReportedItem {
        ReportedItem {
            reference: ExternalRef::parse(text).unwrap(),
            title: Title::parse(&format!("Work on {text}")).unwrap(),
            closed: false,
            listing: Some(Listing {
                areas_hint: hint.iter().map(|h| (*h).to_owned()).collect(),
                priority: Priority::new(priority).unwrap(),
                source_status: None,
                blocked_by: blocked_by
                    .iter()
                    .map(|b| ExternalRef::parse(b).unwrap())
                    .collect(),
            }),
        }
    }

    async fn import(env: &Env, items: Vec<ReportedItem>) {
        let batch =
            ImportBatch::new(Some(SourceId::parse("board").unwrap()), false, items).unwrap();
        UpsertItems::new(&env.deps)
            .execute(&env.importer, batch)
            .await
            .unwrap();
    }

    async fn claim(env: &Env, who: &Person, item: ItemId, areas: &[&str]) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    wide: None,
                    item,
                    session: who.session,
                    areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                },
            )
            .await
            .unwrap();
    }

    fn refs(entries: &[ReadyEntry]) -> Vec<String> {
        entries
            .iter()
            .filter_map(|entry| {
                entry
                    .item
                    .item
                    .external_ref
                    .as_ref()
                    .map(ToString::to_string)
            })
            .collect()
    }

    #[tokio::test]
    async fn the_ready_list_is_soonest_first_and_names_why_the_rest_is_not_ready() {
        let env = Env::new().await;
        import(
            &env,
            vec![
                reported("gh:acme/web#1", 2, &[], &[]),
                reported("gh:acme/web#2", 0, &[], &["gh:acme/web#1"]),
                reported("gh:acme/web#3", 1, &["billing"], &[]),
                reported("gh:acme/web#4", 3, &[], &[]),
            ],
        )
        .await;
        let held = env.item("gh:acme/web#9").await;
        claim(&env, &env.bob, held, &["billing"]).await;
        let view = Ready::new(&env.deps)
            .execute(&env.alice.actor)
            .await
            .unwrap();
        assert_eq!(refs(&view.ready), ["gh:acme/web#1", "gh:acme/web#4"]);
        let link = view.ready[0].link.as_deref();
        assert_eq!(link, Some("https://github.com/acme/web/issues/1"));
        let reasons: Vec<(String, Vec<NamedNotReady>)> = view
            .excluded
            .iter()
            .map(|entry| {
                (
                    entry.item.item.external_ref.as_ref().unwrap().to_string(),
                    entry.reasons.clone(),
                )
            })
            .collect();
        assert_eq!(reasons.len(), 3);
        assert_eq!(reasons[0].0, "gh:acme/web#2");
        let NamedNotReady::BlockedBy { items } = &reasons[0].1[0] else {
            panic!("not blocked: {reasons:?}");
        };
        assert_eq!(
            items[0].external_ref.as_ref().unwrap().to_string(),
            "gh:acme/web#1"
        );
        assert_eq!(reasons[1].0, "gh:acme/web#3");
        let NamedNotReady::AreaHeld { holder, item, .. } = &reasons[1].1[0] else {
            panic!("not held: {reasons:?}");
        };
        assert_eq!(holder.as_str(), "bob");
        assert_eq!(item.id, held);
        assert_eq!(reasons[2].0, "gh:acme/web#9");
        assert_eq!(
            reasons[2].1,
            [NamedNotReady::Claimed {
                holder: holder.clone()
            }]
        );
    }

    #[tokio::test]
    async fn each_member_sees_the_list_from_where_they_stand() {
        let env = Env::new().await;
        import(&env, vec![reported("gh:acme/web#3", 1, &["billing"], &[])]).await;
        let held = env.item("gh:acme/web#9").await;
        claim(&env, &env.bob, held, &["billing"]).await;
        let ready = Ready::new(&env.deps);
        let bobs = ready.execute(&env.bob.actor).await.unwrap();
        assert_eq!(
            refs(&bobs.ready),
            ["gh:acme/web#3"],
            "bob holds the area himself"
        );
        let alices = ready.execute(&env.alice.actor).await.unwrap();
        assert!(alices.ready.is_empty());
    }

    #[tokio::test]
    async fn the_importer_reads_the_ready_list_and_the_status_carries_it() {
        let env = Env::new().await;
        import(&env, vec![reported("gh:acme/web#1", 2, &[], &[])]).await;
        let view = Ready::new(&env.deps).execute(&env.importer).await.unwrap();
        assert_eq!(refs(&view.ready), ["gh:acme/web#1"]);
        let status = Status::new(&env.deps)
            .execute(&env.alice.actor)
            .await
            .unwrap();
        assert_eq!(status.ready, view);
    }
}
