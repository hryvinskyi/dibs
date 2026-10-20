//! A store held in memory. One transaction runs at a time: `begin` waits for the previous one to end,
//! the transaction works on a copy, and only a commit puts the copy in place — unless a record of the
//! copy refers to one that does not exist, in which case the commit fails as a backend failure and
//! keeps nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_lock::{Mutex, MutexGuardArc};
use async_trait::async_trait;
use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::delivery::{Delivery, DeliverySubject};
use dibs_domain::dependency::Dependency;
use dibs_domain::escalation::settled_by;
use dibs_domain::event::{Event, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{
    EventId, GrantId, ItemId, MemberId, MessageId, ProjectId, SessionId, ThreadId,
};
use dibs_domain::item::{ExternalRef, ItemStatus};
use dibs_domain::message::{Ack, Recipients, SessionLabel, speaks_for};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::radar::{Footprint, FootprintDigest, FootprintEntry, Overlap, OverlapKey};
use dibs_domain::repo::RepoId;
use dibs_domain::reservation::SharedPath;
use dibs_domain::settings::SettingsDocument;
use dibs_domain::slot::SlotBoard;
use dibs_domain::wide::WideAsk;

use crate::ports::{
    AckRows, ClaimRows, DeliveryRows, DependencyRows, EscalationRows, EventRows, GrantRows,
    ItemRows, MemberRows, ProjectRows, RadarRows, RaisedRows, SessionRows, SettingsRows, SlotRows,
    Store, StoreError, StoreResult, ThreadRows, TokenRows, Tx, UiSessionRows, WideAskRows,
};
use crate::records::{
    CompletionRecord, EscalationRecord, GrantRecord, InProject, ItemFilter, ItemRecord,
    MemberRecord, MessageRecord, ProjectRecord, PrunedEvents, ReadMark, SessionRecord,
    ThreadActivity, ThreadRecord, ThreadSummary, TokenRecord, UiSessionRecord,
};
use crate::token::{TokenHash, TokenPrefix};
use dibs_domain::project::ProjectSlug;

/// Every record of every project.
#[derive(Debug, Clone, Default)]
struct State {
    projects: BTreeMap<ProjectId, ProjectRecord>,
    members: BTreeMap<(ProjectId, MemberId), MemberRecord>,
    tokens: BTreeMap<TokenHash, InProject<TokenRecord>>,
    sessions: BTreeMap<(ProjectId, SessionId), SessionRecord>,
    items: BTreeMap<(ProjectId, ItemId), ItemRecord>,
    claims: BTreeMap<(ProjectId, ItemId), Claim>,
    dependencies: BTreeMap<(ProjectId, ItemId, ItemId), Dependency>,
    shared: BTreeSet<(ProjectId, SharedPath)>,
    footprints: BTreeMap<(ProjectId, ItemId, RepoId), (Footprint, BTreeSet<RelPath>)>,
    overlaps: BTreeMap<(ProjectId, OverlapKey), Overlap>,
    grants: BTreeMap<(ProjectId, GrantId), GrantRecord>,
    threads: BTreeMap<(ProjectId, ThreadId), ThreadRecord>,
    messages: BTreeMap<(ProjectId, MessageId), MessageRecord>,
    reads: BTreeMap<(ProjectId, MemberId, MessageId), OffsetDateTime>,
    acks: BTreeMap<(ProjectId, MessageId, MemberId), Ack>,
    wide_asks: BTreeMap<(ProjectId, MessageId), WideAsk>,
    escalations: BTreeMap<(ProjectId, MessageId), EscalationRecord>,
    slots: BTreeMap<ProjectId, SlotBoard>,
    deliveries: BTreeMap<(ProjectId, MemberId, DeliverySubject), Delivery>,
    raised: BTreeMap<(ProjectId, MessageId), (MemberId, OffsetDateTime)>,
    calls: BTreeMap<(ProjectId, MemberId), OffsetDateTime>,
    events: Vec<Event>,
    last_event: u64,
    pruned_through: BTreeMap<ProjectId, EventId>,
    settings: BTreeMap<ProjectId, SettingsDocument>,
    ui_sessions: BTreeMap<TokenHash, InProject<UiSessionRecord>>,
}

/// The in-memory store.
#[derive(Debug, Default)]
pub struct MemoryStore {
    state: Arc<Mutex<State>>,
    fail_next_commit: Arc<AtomicBool>,
}

impl MemoryStore {
    /// Makes the next commit fail as a store outage would, keeping nothing of that transaction.
    pub fn fail_next_commit(&self) {
        self.fail_next_commit.store(true, Ordering::SeqCst);
    }

    /// Every event of every project, in log order.
    pub async fn all_events(&self) -> Vec<Event> {
        self.state.lock().await.events.clone()
    }
}

#[async_trait]
impl Store for MemoryStore {
    async fn begin(&self) -> StoreResult<Box<dyn Tx>> {
        let guard = self.state.lock_arc().await;
        let work = guard.clone();
        Ok(Box::new(MemoryTx {
            guard,
            work,
            fail_commit: self.fail_next_commit.clone(),
        }))
    }
}

/// A transaction of the in-memory store.
struct MemoryTx {
    guard: MutexGuardArc<State>,
    work: State,
    fail_commit: Arc<AtomicBool>,
}

fn outage() -> StoreError {
    StoreError::Backend(Box::new(std::io::Error::other("the store went away")))
}

fn dangling(what: &str) -> StoreError {
    StoreError::Backend(Box::new(std::io::Error::other(format!(
        "{what} refers to a record that does not exist"
    ))))
}

impl State {
    /// The first record that refers to a record of its project that does not exist, if any. Every
    /// reference is to a record of the same project; references are checked as a whole when a
    /// transaction commits, so a transaction may write its records in any order.
    fn dangling_reference(&self) -> Option<&'static str> {
        let project = |p: &ProjectId| self.projects.contains_key(p);
        let member = |p: ProjectId, m: MemberId| self.members.contains_key(&(p, m));
        let item = |p: ProjectId, i: ItemId| self.items.contains_key(&(p, i));
        let thread = |p: ProjectId, t: ThreadId| self.threads.contains_key(&(p, t));
        let checks: [(&'static str, bool); 24] = [
            ("a member", self.members.keys().all(|(p, _)| project(p))),
            (
                "a token",
                self.tokens
                    .values()
                    .all(|t| project(&t.project) && member(t.project, t.record.member)),
            ),
            (
                "a session",
                self.sessions.iter().all(|((p, _), s)| member(*p, s.member)),
            ),
            (
                "an item",
                self.items
                    .iter()
                    .all(|((p, _), i)| member(*p, i.created_by)),
            ),
            (
                "a claim",
                self.claims.iter().all(|((p, i), c)| {
                    item(*p, *i)
                        && member(*p, c.member)
                        && self.sessions.contains_key(&(*p, c.session))
                }),
            ),
            (
                "a dependency",
                self.dependencies
                    .keys()
                    .all(|(p, waiting, blocker)| item(*p, *waiting) && item(*p, *blocker)),
            ),
            (
                "a shared path",
                self.shared.iter().all(|(p, s)| item(*p, s.claim)),
            ),
            (
                "a footprint",
                self.footprints.keys().all(|(p, i, _)| item(*p, *i)),
            ),
            (
                "an overlap",
                self.overlaps
                    .iter()
                    .all(|((p, _), o)| o.items().iter().all(|i| item(*p, *i))),
            ),
            (
                "a grant",
                self.grants.iter().all(|((p, _), g)| {
                    item(*p, g.grant.item) && member(*p, g.grant.from) && member(*p, g.grant.to)
                }),
            ),
            (
                "a thread",
                self.threads.iter().all(|((p, _), t)| {
                    t.author.member().is_none_or(|m| member(*p, m))
                        && t.item.is_none_or(|i| item(*p, i))
                }),
            ),
            (
                "a thread's recipient",
                self.threads.iter().all(|((p, _), t)| match &t.recipients {
                    Recipients::Project => true,
                    Recipients::Members(members) => members.iter().all(|m| member(*p, *m)),
                }),
            ),
            (
                "a message",
                self.messages.iter().all(|((p, _), m)| {
                    thread(*p, m.thread) && m.author.member().is_none_or(|a| member(*p, a))
                }),
            ),
            (
                "a read mark",
                self.reads.keys().all(|(p, m, message)| {
                    member(*p, *m) && self.messages.contains_key(&(*p, *message))
                }),
            ),
            (
                "an acknowledgement",
                self.acks.keys().all(|(p, message, m)| {
                    member(*p, *m) && self.messages.contains_key(&(*p, *message))
                }),
            ),
            (
                "a wide claim's announcement",
                self.wide_asks.iter().all(|((p, message), ask)| {
                    self.messages.contains_key(&(*p, *message))
                        && item(*p, ask.item)
                        && member(*p, ask.from)
                        && member(*p, ask.to)
                }),
            ),
            (
                "an escalation",
                self.escalations.iter().all(|((p, message), e)| {
                    self.messages.contains_key(&(*p, *message))
                        && thread(*p, e.thread)
                        && member(*p, e.by)
                        && e.arbiters.iter().all(|a| member(*p, *a))
                }),
            ),
            (
                "a slot hold, waiter or offer",
                self.slots.iter().all(|(p, board)| {
                    board.holds.iter().all(|h| {
                        member(*p, h.member)
                            && self.sessions.contains_key(&(*p, h.session))
                            && h.item.is_none_or(|i| item(*p, i))
                    }) && board.waiters.iter().all(|w| member(*p, w.member))
                        && board.offers.iter().all(|o| member(*p, o.member))
                }),
            ),
            (
                "a delivery",
                self.deliveries.iter().all(|((p, m, _), d)| {
                    member(*p, *m) && self.sessions.contains_key(&(*p, d.session))
                }),
            ),
            (
                "a counted message",
                self.raised.iter().all(|((p, message), (m, _))| {
                    member(*p, *m) && self.messages.contains_key(&(*p, *message))
                }),
            ),
            (
                "a recorded call",
                self.calls.keys().all(|(p, m)| member(*p, *m)),
            ),
            ("an event", self.events.iter().all(|e| project(&e.project))),
            ("a settings document", self.settings.keys().all(project)),
            (
                "a browser session",
                self.ui_sessions.values().all(|s| {
                    member(s.project, s.record.member)
                        && self
                            .tokens
                            .get(&s.record.token)
                            .is_some_and(|t| t.project == s.project)
                }),
            ),
        ];
        checks
            .into_iter()
            .find(|(_, holds)| !holds)
            .map(|(what, _)| what)
    }
}

#[async_trait]
impl Tx for MemoryTx {
    async fn commit(self: Box<Self>) -> StoreResult<()> {
        let mut this = *self;
        if this.fail_commit.swap(false, Ordering::SeqCst) {
            return Err(outage());
        }
        if let Some(what) = this.work.dangling_reference() {
            return Err(dangling(what));
        }
        *this.guard = this.work;
        Ok(())
    }
}

#[async_trait]
impl ProjectRows for MemoryTx {
    async fn insert_project(&mut self, project: &ProjectRecord) -> StoreResult<()> {
        let taken = self.work.projects.contains_key(&project.id)
            || self.work.projects.values().any(|p| p.slug == project.slug);
        if taken {
            return Err(StoreError::Duplicate);
        }
        self.work.projects.insert(project.id, project.clone());
        Ok(())
    }

    async fn project(&mut self, id: ProjectId) -> StoreResult<Option<ProjectRecord>> {
        Ok(self.work.projects.get(&id).cloned())
    }

    async fn project_by_slug(&mut self, slug: &ProjectSlug) -> StoreResult<Option<ProjectRecord>> {
        Ok(self
            .work
            .projects
            .values()
            .find(|p| p.slug == *slug)
            .cloned())
    }

    async fn project_ids(&mut self) -> StoreResult<Vec<ProjectId>> {
        Ok(self.work.projects.keys().copied().collect())
    }
}

#[async_trait]
impl MemberRows for MemoryTx {
    async fn insert_member(
        &mut self,
        project: ProjectId,
        member: &MemberRecord,
    ) -> StoreResult<()> {
        let taken = self.work.members.contains_key(&(project, member.id))
            || self
                .work
                .members
                .iter()
                .any(|((p, _), m)| *p == project && m.handle == member.handle);
        if taken {
            return Err(StoreError::Duplicate);
        }
        self.work
            .members
            .insert((project, member.id), member.clone());
        Ok(())
    }

    async fn member(
        &mut self,
        project: ProjectId,
        id: MemberId,
    ) -> StoreResult<Option<MemberRecord>> {
        Ok(self.work.members.get(&(project, id)).cloned())
    }

    async fn member_by_handle(
        &mut self,
        project: ProjectId,
        handle: &Handle,
    ) -> StoreResult<Option<MemberRecord>> {
        Ok(self
            .work
            .members
            .iter()
            .find(|((p, _), m)| *p == project && m.handle == *handle)
            .map(|(_, m)| m.clone()))
    }

    async fn members(&mut self, project: ProjectId) -> StoreResult<Vec<MemberRecord>> {
        let mut members: Vec<MemberRecord> = self
            .work
            .members
            .iter()
            .filter(|((p, _), _)| *p == project)
            .map(|(_, m)| m.clone())
            .collect();
        members.sort_by(|a, b| a.handle.cmp(&b.handle));
        Ok(members)
    }

    async fn remove_member(
        &mut self,
        project: ProjectId,
        member: MemberId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        if let Some(record) = self.work.members.get_mut(&(project, member))
            && record.removed_at.is_none()
        {
            record.removed_at = Some(at);
        }
        Ok(())
    }

    async fn last_call(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Option<OffsetDateTime>> {
        Ok(self.work.calls.get(&(project, member)).copied())
    }

    async fn record_call(
        &mut self,
        project: ProjectId,
        member: MemberId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        self.work.calls.insert((project, member), at);
        Ok(())
    }

    async fn last_calls(
        &mut self,
        project: ProjectId,
    ) -> StoreResult<Vec<(MemberId, OffsetDateTime)>> {
        Ok(self
            .work
            .calls
            .iter()
            .filter(|((p, _), _)| *p == project)
            .map(|((_, member), at)| (*member, *at))
            .collect())
    }
}

#[async_trait]
impl TokenRows for MemoryTx {
    async fn insert_token(&mut self, project: ProjectId, token: &TokenRecord) -> StoreResult<()> {
        let taken = self.work.tokens.contains_key(&token.hash)
            || self
                .work
                .tokens
                .values()
                .any(|t| t.project == project && t.record.prefix == token.prefix);
        if taken {
            return Err(StoreError::Duplicate);
        }
        self.work.tokens.insert(
            token.hash.clone(),
            InProject {
                project,
                record: token.clone(),
            },
        );
        Ok(())
    }

    async fn token_by_hash(
        &mut self,
        hash: &TokenHash,
    ) -> StoreResult<Option<InProject<TokenRecord>>> {
        Ok(self.work.tokens.get(hash).cloned())
    }

    async fn token_by_prefix(
        &mut self,
        project: ProjectId,
        prefix: &TokenPrefix,
    ) -> StoreResult<Option<TokenRecord>> {
        Ok(self
            .work
            .tokens
            .values()
            .find(|t| t.project == project && t.record.prefix == *prefix)
            .map(|t| t.record.clone()))
    }

    async fn tokens_of(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Vec<TokenRecord>> {
        let mut tokens: Vec<TokenRecord> = self
            .work
            .tokens
            .values()
            .filter(|t| t.project == project && t.record.member == member)
            .map(|t| t.record.clone())
            .collect();
        tokens.sort_by(|a, b| (a.created_at, &a.hash).cmp(&(b.created_at, &b.hash)));
        Ok(tokens)
    }

    async fn revoke_token(
        &mut self,
        project: ProjectId,
        hash: &TokenHash,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        if let Some(token) = self.work.tokens.get_mut(hash)
            && token.project == project
            && token.record.revoked_at.is_none()
        {
            token.record.revoked_at = Some(at);
        }
        Ok(())
    }
}

#[async_trait]
impl SessionRows for MemoryTx {
    async fn session(
        &mut self,
        project: ProjectId,
        id: SessionId,
    ) -> StoreResult<Option<SessionRecord>> {
        Ok(self.work.sessions.get(&(project, id)).cloned())
    }

    async fn session_by_label(
        &mut self,
        project: ProjectId,
        member: MemberId,
        label: &SessionLabel,
    ) -> StoreResult<Option<SessionRecord>> {
        Ok(self
            .work
            .sessions
            .iter()
            .find(|((p, _), s)| *p == project && s.member == member && s.label == *label)
            .map(|(_, s)| s.clone()))
    }

    async fn save_session(
        &mut self,
        project: ProjectId,
        session: &SessionRecord,
    ) -> StoreResult<()> {
        let label_taken = self.work.sessions.iter().any(|((p, id), s)| {
            *p == project
                && *id != session.id
                && s.member == session.member
                && s.label == session.label
        });
        if label_taken {
            return Err(StoreError::Duplicate);
        }
        self.work
            .sessions
            .insert((project, session.id), session.clone());
        Ok(())
    }
}

#[async_trait]
impl ItemRows for MemoryTx {
    async fn insert_item(&mut self, project: ProjectId, item: &ItemRecord) -> StoreResult<()> {
        let taken = self.work.items.contains_key(&(project, item.id))
            || item.external_ref.as_ref().is_some_and(|reference| {
                self.work
                    .items
                    .iter()
                    .any(|((p, _), i)| *p == project && i.external_ref.as_ref() == Some(reference))
            });
        if taken {
            return Err(StoreError::Duplicate);
        }
        self.work.items.insert((project, item.id), item.clone());
        Ok(())
    }

    async fn item(&mut self, project: ProjectId, id: ItemId) -> StoreResult<Option<ItemRecord>> {
        Ok(self.work.items.get(&(project, id)).cloned())
    }

    async fn item_by_ref(
        &mut self,
        project: ProjectId,
        reference: &ExternalRef,
    ) -> StoreResult<Option<ItemRecord>> {
        Ok(self
            .work
            .items
            .iter()
            .find(|((p, _), i)| *p == project && i.external_ref.as_ref() == Some(reference))
            .map(|(_, i)| i.clone()))
    }

    async fn items_of(
        &mut self,
        project: ProjectId,
        ids: &BTreeSet<ItemId>,
    ) -> StoreResult<Vec<ItemRecord>> {
        Ok(ids
            .iter()
            .filter_map(|id| self.work.items.get(&(project, *id)).cloned())
            .collect())
    }

    async fn items(
        &mut self,
        project: ProjectId,
        filter: &ItemFilter,
    ) -> StoreResult<Vec<ItemRecord>> {
        let held_by = |item: ItemId, member: MemberId| {
            self.work
                .claims
                .get(&(project, item))
                .is_some_and(|claim| claim.member == member)
        };
        Ok(self
            .work
            .items
            .iter()
            .filter(|((p, _), _)| *p == project)
            .map(|(_, item)| item)
            .filter(|item| filter.status.is_none_or(|status| item.status == status))
            .filter(|item| filter.holder.is_none_or(|member| held_by(item.id, member)))
            .filter(|item| {
                filter
                    .external_ref
                    .as_ref()
                    .is_none_or(|reference| item.external_ref.as_ref() == Some(reference))
            })
            .cloned()
            .collect())
    }

    async fn set_item_status(
        &mut self,
        project: ProjectId,
        id: ItemId,
        status: ItemStatus,
    ) -> StoreResult<()> {
        if let Some(item) = self.work.items.get_mut(&(project, id)) {
            item.status = status;
        }
        Ok(())
    }

    async fn update_item(&mut self, project: ProjectId, item: &ItemRecord) -> StoreResult<()> {
        if let Some(stored) = self.work.items.get_mut(&(project, item.id)) {
            stored.set_meta(item.meta());
            stored.source = item.source.clone();
        }
        Ok(())
    }
}

#[async_trait]
impl DependencyRows for MemoryTx {
    async fn dependencies(&mut self, project: ProjectId) -> StoreResult<Vec<Dependency>> {
        Ok(self
            .work
            .dependencies
            .iter()
            .filter(|((p, _, _), _)| *p == project)
            .map(|(_, edge)| *edge)
            .collect())
    }

    async fn save_dependency(&mut self, project: ProjectId, edge: &Dependency) -> StoreResult<()> {
        self.work
            .dependencies
            .insert((project, edge.item, edge.blocked_by), *edge);
        Ok(())
    }

    async fn delete_dependency(
        &mut self,
        project: ProjectId,
        item: ItemId,
        blocked_by: ItemId,
    ) -> StoreResult<()> {
        self.work.dependencies.remove(&(project, item, blocked_by));
        Ok(())
    }
}

#[async_trait]
impl ClaimRows for MemoryTx {
    async fn claim(&mut self, project: ProjectId, item: ItemId) -> StoreResult<Option<Claim>> {
        Ok(self.work.claims.get(&(project, item)).cloned())
    }

    async fn claims(&mut self, project: ProjectId) -> StoreResult<Vec<Claim>> {
        Ok(self
            .work
            .claims
            .iter()
            .filter(|((p, _), _)| *p == project)
            .map(|(_, claim)| claim.clone())
            .collect())
    }

    async fn save_claim(&mut self, project: ProjectId, claim: &Claim) -> StoreResult<()> {
        self.work
            .claims
            .insert((project, claim.item), claim.clone());
        Ok(())
    }

    async fn delete_claim(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        self.work.claims.remove(&(project, item));
        Ok(())
    }

    async fn shared_paths(&mut self, project: ProjectId) -> StoreResult<Vec<SharedPath>> {
        Ok(self
            .work
            .shared
            .iter()
            .filter(|(p, _)| *p == project)
            .map(|(_, shared)| shared.clone())
            .collect())
    }

    async fn insert_shared_path(
        &mut self,
        project: ProjectId,
        shared: &SharedPath,
    ) -> StoreResult<()> {
        self.work.shared.insert((project, shared.clone()));
        Ok(())
    }

    async fn delete_shared_paths(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        self.work
            .shared
            .retain(|(p, shared)| !(*p == project && shared.claim == item));
        Ok(())
    }
}

#[async_trait]
impl RadarRows for MemoryTx {
    async fn footprints(&mut self, project: ProjectId) -> StoreResult<Vec<Footprint>> {
        Ok(self
            .work
            .footprints
            .iter()
            .filter(|((p, _, _), _)| *p == project)
            .map(|(_, (footprint, _))| footprint.clone())
            .collect())
    }

    async fn footprint(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
    ) -> StoreResult<Option<(Footprint, BTreeSet<RelPath>)>> {
        Ok(self
            .work
            .footprints
            .get(&(project, item, repo.clone()))
            .cloned())
    }

    async fn save_footprint(
        &mut self,
        project: ProjectId,
        footprint: &Footprint,
        paths: &BTreeSet<RelPath>,
    ) -> StoreResult<()> {
        let stored = Footprint {
            path_count: paths.len(),
            ..footprint.clone()
        };
        self.work.footprints.insert(
            (project, footprint.item, footprint.repo.clone()),
            (stored, paths.clone()),
        );
        Ok(())
    }

    async fn set_footprint_digest(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
        digest: &FootprintDigest,
    ) -> StoreResult<()> {
        if let Some((footprint, _)) = self.work.footprints.get_mut(&(project, item, repo.clone())) {
            footprint.digest = digest.clone();
        }
        Ok(())
    }

    async fn delete_footprint(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
    ) -> StoreResult<()> {
        self.work.footprints.remove(&(project, item, repo.clone()));
        Ok(())
    }

    async fn delete_footprints(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        self.work
            .footprints
            .retain(|(p, i, _), _| !(*p == project && *i == item));
        Ok(())
    }

    async fn footprint_entries(
        &mut self,
        project: ProjectId,
        repo: &RepoId,
        paths: &BTreeSet<RelPath>,
    ) -> StoreResult<Vec<FootprintEntry>> {
        Ok(self
            .work
            .footprints
            .iter()
            .filter(|((p, _, r), _)| *p == project && r == repo)
            .flat_map(|((_, item, _), (_, stored))| {
                stored
                    .intersection(paths)
                    .map(|path| FootprintEntry {
                        item: *item,
                        path: RepoPath {
                            repo: repo.clone(),
                            path: path.clone(),
                        },
                    })
                    .collect::<Vec<_>>()
            })
            .collect())
    }

    async fn overlaps(&mut self, project: ProjectId) -> StoreResult<Vec<Overlap>> {
        Ok(self
            .work
            .overlaps
            .iter()
            .filter(|((p, _), _)| *p == project)
            .map(|(_, overlap)| overlap.clone())
            .collect())
    }

    async fn save_overlap(&mut self, project: ProjectId, overlap: &Overlap) -> StoreResult<()> {
        self.work
            .overlaps
            .insert((project, overlap.key()), overlap.clone());
        Ok(())
    }

    async fn delete_overlap(&mut self, project: ProjectId, key: &OverlapKey) -> StoreResult<()> {
        self.work.overlaps.remove(&(project, key.clone()));
        Ok(())
    }

    async fn delete_overlaps_of(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        self.work
            .overlaps
            .retain(|(p, _), overlap| !(*p == project && overlap.involves(item)));
        Ok(())
    }
}

#[async_trait]
impl GrantRows for MemoryTx {
    async fn grants(&mut self, project: ProjectId) -> StoreResult<Vec<GrantRecord>> {
        Ok(self
            .work
            .grants
            .iter()
            .filter(|((p, _), _)| *p == project)
            .map(|(_, grant)| grant.clone())
            .collect())
    }

    async fn grant(&mut self, project: ProjectId, id: GrantId) -> StoreResult<Option<GrantRecord>> {
        Ok(self.work.grants.get(&(project, id)).cloned())
    }

    async fn insert_grant(&mut self, project: ProjectId, grant: &GrantRecord) -> StoreResult<()> {
        if self.work.grants.contains_key(&(project, grant.id)) {
            return Err(StoreError::Duplicate);
        }
        self.work.grants.insert((project, grant.id), grant.clone());
        Ok(())
    }

    async fn delete_grant(&mut self, project: ProjectId, id: GrantId) -> StoreResult<()> {
        self.work.grants.remove(&(project, id));
        Ok(())
    }
}

#[async_trait]
impl ThreadRows for MemoryTx {
    async fn insert_thread(
        &mut self,
        project: ProjectId,
        thread: &ThreadRecord,
    ) -> StoreResult<()> {
        if self.work.threads.contains_key(&(project, thread.id)) {
            return Err(StoreError::Duplicate);
        }
        self.work
            .threads
            .insert((project, thread.id), thread.clone());
        Ok(())
    }

    async fn thread(
        &mut self,
        project: ProjectId,
        id: ThreadId,
    ) -> StoreResult<Option<ThreadRecord>> {
        Ok(self.work.threads.get(&(project, id)).cloned())
    }

    async fn insert_message(
        &mut self,
        project: ProjectId,
        message: &MessageRecord,
    ) -> StoreResult<()> {
        if self.work.messages.contains_key(&(project, message.id)) {
            return Err(StoreError::Duplicate);
        }
        self.work
            .messages
            .insert((project, message.id), message.clone());
        Ok(())
    }

    async fn message(
        &mut self,
        project: ProjectId,
        id: MessageId,
    ) -> StoreResult<Option<MessageRecord>> {
        Ok(self.work.messages.get(&(project, id)).cloned())
    }

    async fn messages(
        &mut self,
        project: ProjectId,
        thread: ThreadId,
    ) -> StoreResult<Vec<MessageRecord>> {
        let mut messages: Vec<MessageRecord> = self
            .work
            .messages
            .iter()
            .filter(|((p, _), m)| *p == project && m.thread == thread)
            .map(|(_, m)| m.clone())
            .collect();
        messages.sort_by_key(|m| (m.created_at, m.id));
        Ok(messages)
    }

    async fn thread_summaries(
        &mut self,
        project: ProjectId,
        reader: MemberId,
        item: Option<ItemId>,
    ) -> StoreResult<Vec<ThreadSummary>> {
        let mut summaries: Vec<ThreadSummary> = self
            .work
            .threads
            .iter()
            .filter(|((p, _), t)| *p == project && item.is_none_or(|item| t.item == Some(item)))
            .map(|(_, thread)| {
                let messages: Vec<&MessageRecord> = self
                    .work
                    .messages
                    .iter()
                    .filter(|((p, _), m)| *p == project && m.thread == thread.id)
                    .map(|(_, m)| m)
                    .collect();
                let unread = messages
                    .iter()
                    .filter(|m| {
                        speaks_for(thread.author, m.author) != Some(reader)
                            && !self.work.reads.contains_key(&(project, reader, m.id))
                    })
                    .count();
                ThreadSummary {
                    thread: thread.clone(),
                    messages: u64::try_from(messages.len()).unwrap_or(u64::MAX),
                    unread: u64::try_from(unread).unwrap_or(u64::MAX),
                    last_message_at: messages.iter().map(|m| m.created_at).max(),
                    importance: messages
                        .iter()
                        .map(|m| m.importance)
                        .max()
                        .unwrap_or_default(),
                }
            })
            .collect();
        summaries.sort_by(|a, b| {
            let activity = |s: &ThreadSummary| {
                (
                    s.last_message_at.unwrap_or(s.thread.created_at),
                    s.thread.id,
                )
            };
            activity(b).cmp(&activity(a))
        });
        Ok(summaries)
    }

    async fn quiet_threads(
        &mut self,
        project: ProjectId,
        before: OffsetDateTime,
    ) -> StoreResult<Vec<ThreadActivity>> {
        Ok(self
            .work
            .threads
            .iter()
            .filter(|((p, _), _)| *p == project)
            .map(|(_, t)| ThreadActivity {
                thread: t.id,
                item: t.item,
                last_activity: self
                    .work
                    .messages
                    .iter()
                    .filter(|((p, _), m)| *p == project && m.thread == t.id)
                    .map(|(_, m)| m.created_at)
                    .max()
                    .unwrap_or(t.created_at),
            })
            .filter(|activity| activity.last_activity < before)
            .collect())
    }

    async fn delete_thread(&mut self, project: ProjectId, thread: ThreadId) -> StoreResult<u64> {
        let gone: BTreeSet<MessageId> = self
            .work
            .messages
            .iter()
            .filter(|((p, _), m)| *p == project && m.thread == thread)
            .map(|((_, id), _)| *id)
            .collect();
        let work = &mut self.work;
        work.messages
            .retain(|(p, id), _| !(*p == project && gone.contains(id)));
        work.reads
            .retain(|(p, _, id), _| !(*p == project && gone.contains(id)));
        work.acks
            .retain(|(p, id, _), _| !(*p == project && gone.contains(id)));
        work.wide_asks
            .retain(|(p, id), _| !(*p == project && gone.contains(id)));
        work.escalations
            .retain(|(p, id), _| !(*p == project && gone.contains(id)));
        work.raised
            .retain(|(p, id), _| !(*p == project && gone.contains(id)));
        work.deliveries.retain(|(p, _, subject), _| {
            !(*p == project && matches!(subject, DeliverySubject::Message(id) if gone.contains(id)))
        });
        work.threads.remove(&(project, thread));
        Ok(u64::try_from(gone.len()).unwrap_or(u64::MAX))
    }

    async fn mark_read(
        &mut self,
        project: ProjectId,
        reader: MemberId,
        thread: ThreadId,
        at: OffsetDateTime,
    ) -> StoreResult<u64> {
        let Some(thread_author) = self.work.threads.get(&(project, thread)).map(|t| t.author)
        else {
            return Ok(0);
        };
        let ids: Vec<MessageId> = self
            .work
            .messages
            .iter()
            .filter(|((p, _), m)| {
                *p == project
                    && m.thread == thread
                    && speaks_for(thread_author, m.author) != Some(reader)
            })
            .map(|(_, m)| m.id)
            .collect();
        let mut newly = 0;
        for id in ids {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                self.work.reads.entry((project, reader, id))
            {
                entry.insert(at);
                newly += 1;
            }
        }
        Ok(newly)
    }
}

#[async_trait]
impl AckRows for MemoryTx {
    async fn insert_ack(&mut self, project: ProjectId, ack: &Ack) -> StoreResult<()> {
        let key = (project, ack.message, ack.member);
        if self.work.acks.contains_key(&key) {
            return Err(StoreError::Duplicate);
        }
        self.work.acks.insert(key, *ack);
        Ok(())
    }

    async fn acks(&mut self, project: ProjectId, message: MessageId) -> StoreResult<Vec<Ack>> {
        let mut acks: Vec<Ack> = self
            .work
            .acks
            .iter()
            .filter(|((p, m, _), _)| *p == project && *m == message)
            .map(|(_, ack)| *ack)
            .collect();
        acks.sort_by_key(|ack| (ack.at, ack.member));
        Ok(acks)
    }

    async fn thread_acks(&mut self, project: ProjectId, thread: ThreadId) -> StoreResult<Vec<Ack>> {
        let mut acks: Vec<Ack> = self
            .work
            .acks
            .iter()
            .filter(|((p, message, _), _)| {
                *p == project
                    && self
                        .work
                        .messages
                        .get(&(project, *message))
                        .is_some_and(|m| m.thread == thread)
            })
            .map(|(_, ack)| *ack)
            .collect();
        acks.sort_by_key(|ack| (ack.at, ack.message, ack.member));
        Ok(acks)
    }

    async fn ack_requests(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Vec<MessageRecord>> {
        let mut asked: Vec<MessageRecord> = self
            .work
            .messages
            .iter()
            .filter(|((p, id), m)| {
                let sender = self
                    .work
                    .threads
                    .get(&(project, m.thread))
                    .and_then(|t| speaks_for(t.author, m.author));
                *p == project
                    && m.ack_required
                    && sender != Some(member)
                    && !self.work.acks.contains_key(&(project, *id, member))
            })
            .map(|(_, m)| m.clone())
            .collect();
        asked.sort_by_key(|m| std::cmp::Reverse((m.created_at, m.id)));
        Ok(asked)
    }

    async fn reads_of(
        &mut self,
        project: ProjectId,
        message: MessageId,
    ) -> StoreResult<Vec<ReadMark>> {
        Ok(self
            .work
            .reads
            .iter()
            .filter(|((p, _, m), _)| *p == project && *m == message)
            .map(|((_, member, _), at)| ReadMark {
                member: *member,
                at: *at,
            })
            .collect())
    }
}

#[async_trait]
impl WideAskRows for MemoryTx {
    async fn insert_wide_ask(&mut self, project: ProjectId, ask: &WideAsk) -> StoreResult<()> {
        let key = (project, ask.message);
        if self.work.wide_asks.contains_key(&key) {
            return Err(StoreError::Duplicate);
        }
        self.work.wide_asks.insert(key, ask.clone());
        Ok(())
    }

    async fn wide_ask(
        &mut self,
        project: ProjectId,
        message: MessageId,
    ) -> StoreResult<Option<WideAsk>> {
        Ok(self.work.wide_asks.get(&(project, message)).cloned())
    }

    async fn open_wide_asks(&mut self, project: ProjectId) -> StoreResult<Vec<WideAsk>> {
        Ok(self
            .work
            .wide_asks
            .iter()
            .filter(|((p, message), ask)| {
                *p == project && !self.work.acks.contains_key(&(project, *message, ask.to))
            })
            .map(|(_, ask)| ask.clone())
            .collect())
    }
}

#[async_trait]
impl EscalationRows for MemoryTx {
    async fn insert_escalation(
        &mut self,
        project: ProjectId,
        escalation: &EscalationRecord,
    ) -> StoreResult<()> {
        let key = (project, escalation.message);
        if self.work.escalations.contains_key(&key) {
            return Err(StoreError::Duplicate);
        }
        self.work.escalations.insert(key, escalation.clone());
        Ok(())
    }

    async fn open_escalations(&mut self, project: ProjectId) -> StoreResult<Vec<EscalationRecord>> {
        let mut found: Vec<EscalationRecord> = self
            .work
            .escalations
            .iter()
            .filter(|((p, message), e)| {
                let acks: Vec<Ack> = self
                    .work
                    .acks
                    .iter()
                    .filter(|((ap, am, _), _)| ap == p && am == message)
                    .map(|(_, ack)| *ack)
                    .collect();
                *p == project && settled_by(&e.arbiters, &acks).is_none()
            })
            .map(|(_, e)| e.clone())
            .collect();
        found.sort_by_key(|e| std::cmp::Reverse((e.created_at, e.message)));
        Ok(found)
    }
}

#[async_trait]
impl SlotRows for MemoryTx {
    async fn slot_board(&mut self, project: ProjectId) -> StoreResult<SlotBoard> {
        Ok(self.work.slots.get(&project).cloned().unwrap_or_default())
    }

    async fn put_slot_board(&mut self, project: ProjectId, board: &SlotBoard) -> StoreResult<()> {
        let mut board = board.clone();
        board.holds.sort_by(|a, b| a.slot.cmp(&b.slot));
        board.offers.sort_by(|a, b| a.slot.cmp(&b.slot));
        board.waiters.sort_by_key(|w| (w.since, w.id));
        let unique_slots = |slots: Vec<&dibs_domain::slot::SlotName>| {
            slots.windows(2).all(|pair| pair[0] != pair[1])
        };
        let mut places: Vec<_> = board.waiters.iter().map(|w| (w.member, &w.slot)).collect();
        places.sort();
        let mut ids: Vec<_> = board.waiters.iter().map(|w| w.id).collect();
        ids.sort();
        let unique = unique_slots(board.holds.iter().map(|h| &h.slot).collect())
            && unique_slots(board.offers.iter().map(|o| &o.slot).collect())
            && places.windows(2).all(|pair| pair[0] != pair[1])
            && ids.windows(2).all(|pair| pair[0] != pair[1]);
        if !unique {
            return Err(StoreError::Duplicate);
        }
        if board.is_empty() {
            self.work.slots.remove(&project);
        } else {
            self.work.slots.insert(project, board);
        }
        Ok(())
    }

    async fn busy_slot_projects(&mut self) -> StoreResult<Vec<ProjectId>> {
        Ok(self.work.slots.keys().copied().collect())
    }
}

#[async_trait]
impl DeliveryRows for MemoryTx {
    async fn insert_delivery(
        &mut self,
        project: ProjectId,
        delivery: &Delivery,
    ) -> StoreResult<()> {
        let key = (project, delivery.member, delivery.subject.clone());
        if self.work.deliveries.contains_key(&key) {
            return Err(StoreError::Duplicate);
        }
        self.work.deliveries.insert(key, delivery.clone());
        Ok(())
    }

    async fn delivery(
        &mut self,
        project: ProjectId,
        member: MemberId,
        subject: &DeliverySubject,
    ) -> StoreResult<Option<Delivery>> {
        Ok(self
            .work
            .deliveries
            .get(&(project, member, subject.clone()))
            .cloned())
    }

    async fn deliveries(
        &mut self,
        project: ProjectId,
        member: MemberId,
        subjects: &[DeliverySubject],
    ) -> StoreResult<Vec<Delivery>> {
        Ok(subjects
            .iter()
            .filter_map(|subject| {
                self.work
                    .deliveries
                    .get(&(project, member, subject.clone()))
                    .cloned()
            })
            .collect())
    }
}

#[async_trait]
impl RaisedRows for MemoryTx {
    async fn insert_raised(
        &mut self,
        project: ProjectId,
        member: MemberId,
        message: MessageId,
        at: OffsetDateTime,
    ) -> StoreResult<()> {
        if self.work.raised.contains_key(&(project, message)) {
            return Err(StoreError::Duplicate);
        }
        self.work.raised.insert((project, message), (member, at));
        Ok(())
    }

    async fn raised_since(
        &mut self,
        project: ProjectId,
        member: MemberId,
        since: OffsetDateTime,
    ) -> StoreResult<Vec<OffsetDateTime>> {
        let mut times: Vec<OffsetDateTime> = self
            .work
            .raised
            .iter()
            .filter(|((p, _), (m, at))| *p == project && *m == member && *at >= since)
            .map(|(_, (_, at))| *at)
            .collect();
        times.sort_unstable();
        Ok(times)
    }
}

#[async_trait]
impl EventRows for MemoryTx {
    async fn append_event(
        &mut self,
        project: ProjectId,
        at: OffsetDateTime,
        kind: &EventKind,
    ) -> StoreResult<EventId> {
        self.work.last_event += 1;
        let id = EventId(self.work.last_event);
        self.work.events.push(Event {
            id,
            project,
            at,
            kind: kind.clone(),
        });
        Ok(id)
    }

    async fn events_after(
        &mut self,
        project: ProjectId,
        after: Option<EventId>,
        limit: usize,
    ) -> StoreResult<Vec<Event>> {
        Ok(self
            .work
            .events
            .iter()
            .filter(|e| e.project == project && after.is_none_or(|after| e.id > after))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn latest_event(&mut self, project: ProjectId) -> StoreResult<Option<EventId>> {
        Ok(self
            .work
            .events
            .iter()
            .rev()
            .find(|e| e.project == project)
            .map(|e| e.id))
    }

    async fn delete_events_before(
        &mut self,
        project: ProjectId,
        before: OffsetDateTime,
    ) -> StoreResult<PrunedEvents> {
        let gone: Vec<EventId> = self
            .work
            .events
            .iter()
            .filter(|e| e.project == project && e.at < before)
            .map(|e| e.id)
            .collect();
        self.work
            .events
            .retain(|e| !(e.project == project && e.at < before));
        let through = gone.iter().max().copied();
        if let Some(through) = through {
            let mark = self.work.pruned_through.entry(project).or_insert(through);
            *mark = (*mark).max(through);
        }
        Ok(PrunedEvents {
            count: u64::try_from(gone.len()).unwrap_or(u64::MAX),
            through,
        })
    }

    async fn events_pruned_through(&mut self, project: ProjectId) -> StoreResult<Option<EventId>> {
        Ok(self.work.pruned_through.get(&project).copied())
    }

    async fn completions_since(
        &mut self,
        project: ProjectId,
        since: OffsetDateTime,
        limit: usize,
    ) -> StoreResult<Vec<CompletionRecord>> {
        let mut found: Vec<(EventId, CompletionRecord)> = self
            .work
            .events
            .iter()
            .filter(|e| e.project == project && e.at >= since)
            .filter_map(|e| {
                let (item, member) = e.kind.completion()?;
                let record = CompletionRecord {
                    item,
                    member,
                    at: e.at,
                };
                Some((e.id, record))
            })
            .collect();
        found.sort_by(|(a_id, a), (b_id, b)| (b.at, b_id).cmp(&(a.at, a_id)));
        found.truncate(limit);
        Ok(found.into_iter().map(|(_, record)| record).collect())
    }
}

#[async_trait]
impl SettingsRows for MemoryTx {
    async fn settings(&mut self, project: ProjectId) -> StoreResult<Option<SettingsDocument>> {
        Ok(self.work.settings.get(&project).cloned())
    }

    async fn put_settings(
        &mut self,
        project: ProjectId,
        document: &SettingsDocument,
    ) -> StoreResult<()> {
        self.work.settings.insert(project, document.clone());
        Ok(())
    }
}

#[async_trait]
impl UiSessionRows for MemoryTx {
    async fn insert_ui_session(
        &mut self,
        project: ProjectId,
        session: &UiSessionRecord,
    ) -> StoreResult<()> {
        if self.work.ui_sessions.contains_key(&session.hash) {
            return Err(StoreError::Duplicate);
        }
        self.work.ui_sessions.insert(
            session.hash.clone(),
            InProject {
                project,
                record: session.clone(),
            },
        );
        Ok(())
    }

    async fn ui_session(
        &mut self,
        hash: &TokenHash,
    ) -> StoreResult<Option<InProject<UiSessionRecord>>> {
        Ok(self.work.ui_sessions.get(hash).cloned())
    }

    async fn extend_ui_session(
        &mut self,
        project: ProjectId,
        hash: &TokenHash,
        expires_at: OffsetDateTime,
    ) -> StoreResult<()> {
        if let Some(session) = self.work.ui_sessions.get_mut(hash)
            && session.project == project
        {
            session.record.expires_at = expires_at;
        }
        Ok(())
    }

    async fn delete_ui_session(&mut self, project: ProjectId, hash: &TokenHash) -> StoreResult<()> {
        if self
            .work
            .ui_sessions
            .get(hash)
            .is_some_and(|session| session.project == project)
        {
            self.work.ui_sessions.remove(hash);
        }
        Ok(())
    }

    async fn delete_ui_sessions_of(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<u64> {
        let before = self.work.ui_sessions.len();
        self.work
            .ui_sessions
            .retain(|_, session| !(session.project == project && session.record.member == member));
        Ok(u64::try_from(before - self.work.ui_sessions.len()).unwrap_or(u64::MAX))
    }

    async fn delete_expired_ui_sessions(&mut self, now: OffsetDateTime) -> StoreResult<u64> {
        let before = self.work.ui_sessions.len();
        self.work
            .ui_sessions
            .retain(|_, session| session.record.expires_at >= now);
        Ok(u64::try_from(before - self.work.ui_sessions.len()).unwrap_or(u64::MAX))
    }
}
