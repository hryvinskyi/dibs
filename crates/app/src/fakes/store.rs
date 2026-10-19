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
use dibs_domain::event::{Event, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{
    EventId, GrantId, ItemId, MemberId, MessageId, ProjectId, SessionId, ThreadId,
};
use dibs_domain::item::{ExternalRef, ItemStatus};
use dibs_domain::message::{Recipients, SessionLabel};
use dibs_domain::reservation::SharedPath;
use dibs_domain::settings::SettingsDocument;

use crate::ports::{
    ClaimRows, EventRows, GrantRows, ItemRows, MemberRows, ProjectRows, SessionRows, SettingsRows,
    Store, StoreError, StoreResult, ThreadRows, TokenRows, Tx, UiSessionRows,
};
use crate::records::{
    CompletionRecord, GrantRecord, InProject, ItemFilter, ItemRecord, MemberRecord, MessageRecord,
    ProjectRecord, SessionRecord, ThreadRecord, ThreadSummary, TokenRecord, UiSessionRecord,
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
    shared: BTreeSet<(ProjectId, SharedPath)>,
    grants: BTreeMap<(ProjectId, GrantId), GrantRecord>,
    threads: BTreeMap<(ProjectId, ThreadId), ThreadRecord>,
    messages: BTreeMap<(ProjectId, MessageId), MessageRecord>,
    reads: BTreeSet<(ProjectId, MemberId, MessageId)>,
    events: Vec<Event>,
    last_event: u64,
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
        let checks: [(&'static str, bool); 14] = [
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
                "a shared path",
                self.shared.iter().all(|(p, s)| item(*p, s.claim)),
            ),
            (
                "a grant",
                self.grants.iter().all(|((p, _), g)| {
                    item(*p, g.grant.item) && member(*p, g.grant.from) && member(*p, g.grant.to)
                }),
            ),
            (
                "a thread",
                self.threads
                    .iter()
                    .all(|((p, _), t)| member(*p, t.author) && t.item.is_none_or(|i| item(*p, i))),
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
                self.messages
                    .iter()
                    .all(|((p, _), m)| thread(*p, m.thread) && member(*p, m.author)),
            ),
            (
                "a read mark",
                self.reads.iter().all(|(p, m, message)| {
                    member(*p, *m) && self.messages.contains_key(&(*p, *message))
                }),
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
                        m.author != reader && !self.work.reads.contains(&(project, reader, m.id))
                    })
                    .count();
                ThreadSummary {
                    thread: thread.clone(),
                    messages: u64::try_from(messages.len()).unwrap_or(u64::MAX),
                    unread: u64::try_from(unread).unwrap_or(u64::MAX),
                    last_message_at: messages.iter().map(|m| m.created_at).max(),
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

    async fn mark_read(
        &mut self,
        project: ProjectId,
        reader: MemberId,
        thread: ThreadId,
        _at: OffsetDateTime,
    ) -> StoreResult<u64> {
        let ids: Vec<MessageId> = self
            .work
            .messages
            .iter()
            .filter(|((p, _), m)| *p == project && m.thread == thread && m.author != reader)
            .map(|(_, m)| m.id)
            .collect();
        let mut newly = 0;
        for id in ids {
            if self.work.reads.insert((project, reader, id)) {
                newly += 1;
            }
        }
        Ok(newly)
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
                let EventKind::Completed(completed) = &e.kind else {
                    return None;
                };
                let record = CompletionRecord {
                    item: completed.item,
                    member: completed.member,
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

    async fn delete_expired_ui_sessions(&mut self, now: OffsetDateTime) -> StoreResult<u64> {
        let before = self.work.ui_sessions.len();
        self.work
            .ui_sessions
            .retain(|_, session| session.record.expires_at >= now);
        Ok(u64::try_from(before - self.work.ui_sessions.len()).unwrap_or(u64::MAX))
    }
}
