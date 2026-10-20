//! The store: one unit of work at a time over every record of every project.
//!
//! A [`Tx`] is a transaction that writes exclusively from its start: while one is open, no other
//! writer runs, so a use case that loads rows, decides and writes inside one `Tx` decides against
//! state nobody else changes — that is what makes every compare-and-set of the domain true. Dropping
//! a `Tx` without [`Tx::commit`] discards everything it wrote.
//!
//! Every method takes the project first, so tenancy is in the signature. The only exceptions find a
//! record by a key unique across projects and return the project with it ([`TokenRows::token_by_hash`],
//! [`UiSessionRows::ui_session`]), create, look up or list projects themselves ([`ProjectRows`]),
//! sweep expired browser sessions of every project ([`UiSessionRows::delete_expired_ui_sessions`]),
//! or find the projects whose slots are in use ([`SlotRows::busy_slot_projects`]) or whose claims
//! wait for areas ([`ClaimRows::pending_projects`]).

use std::collections::BTreeSet;

use async_trait::async_trait;
use thiserror::Error;
use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::delivery::{Delivery, DeliverySubject};
use dibs_domain::dependency::Dependency;
use dibs_domain::event::{Event, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{
    EventId, GrantId, ItemId, MemberId, MessageId, ProjectId, SessionId, ThreadId,
};
use dibs_domain::item::{ExternalRef, ItemStatus};
use dibs_domain::message::{Ack, SessionLabel};
use dibs_domain::paths::RelPath;
use dibs_domain::radar::{Footprint, FootprintDigest, FootprintEntry, Overlap, OverlapKey};
use dibs_domain::repo::RepoId;
use dibs_domain::reservation::SharedPath;
use dibs_domain::settings::SettingsDocument;
use dibs_domain::slot::SlotBoard;
use dibs_domain::wide::WideAsk;

use crate::records::{
    CompletionRecord, EscalationRecord, GrantRecord, InProject, ItemFilter, ItemRecord,
    MemberRecord, MessageRecord, ProjectRecord, PrunedEvents, ReadMark, SessionRecord,
    ThreadActivity, ThreadRecord, ThreadSummary, TokenRecord, UiSessionRecord,
};
use crate::token::{TokenHash, TokenPrefix};
use dibs_domain::project::ProjectSlug;

/// Why the store failed. The text is for logs; people are told only that the request failed.
#[derive(Debug, Error)]
pub enum StoreError {
    /// A write broke a uniqueness rule of the store.
    #[error("a record with the same key already exists")]
    Duplicate,
    /// A stored row does not hold valid values.
    #[error("a stored record is not valid: {what}")]
    Corrupt {
        /// Which record, and what is wrong with it.
        what: String,
    },
    /// The store failed or could not be reached.
    #[error("the store failed")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// The result of a store call.
pub type StoreResult<T> = Result<T, StoreError>;

/// Opens units of work.
#[async_trait]
pub trait Store: Send + Sync {
    /// Opens a transaction that writes exclusively from its start.
    ///
    /// While another transaction is open it waits for the write lock, but only for a bounded time
    /// the adapter sets; when that runs out it fails with
    /// [`StoreError::Backend`]. There is one kind of transaction, so a use case that only reads
    /// takes the same lock as one that writes.
    async fn begin(&self) -> StoreResult<Box<dyn Tx>>;
}

/// One unit of work over every kind of record.
#[async_trait]
pub trait Tx:
    ProjectRows
    + MemberRows
    + TokenRows
    + SessionRows
    + ItemRows
    + ClaimRows
    + RadarRows
    + DependencyRows
    + GrantRows
    + ThreadRows
    + AckRows
    + WideAskRows
    + EscalationRows
    + SlotRows
    + DeliveryRows
    + RaisedRows
    + EventRows
    + SettingsRows
    + UiSessionRows
    + Send
{
    /// Makes every write of the transaction durable and visible, all at once. A transaction whose
    /// records refer to a record of their project that does not exist commits nothing and fails
    /// with [`StoreError::Backend`].
    async fn commit(self: Box<Self>) -> StoreResult<()>;
}

/// Projects.
#[async_trait]
pub trait ProjectRows: Send {
    /// Stores a new project.
    async fn insert_project(&mut self, project: &ProjectRecord) -> StoreResult<()>;
    /// The project of this id.
    async fn project(&mut self, id: ProjectId) -> StoreResult<Option<ProjectRecord>>;
    /// The project of this slug.
    async fn project_by_slug(&mut self, slug: &ProjectSlug) -> StoreResult<Option<ProjectRecord>>;
    /// Every project, ordered by id.
    async fn project_ids(&mut self) -> StoreResult<Vec<ProjectId>>;
}

/// Members.
#[async_trait]
pub trait MemberRows: Send {
    /// Stores a new member.
    async fn insert_member(&mut self, project: ProjectId, member: &MemberRecord)
    -> StoreResult<()>;
    /// The member of this id.
    async fn member(
        &mut self,
        project: ProjectId,
        id: MemberId,
    ) -> StoreResult<Option<MemberRecord>>;
    /// The member of this handle.
    async fn member_by_handle(
        &mut self,
        project: ProjectId,
        handle: &Handle,
    ) -> StoreResult<Option<MemberRecord>>;
    /// Every member, removed ones included, ordered by handle.
    async fn members(&mut self, project: ProjectId) -> StoreResult<Vec<MemberRecord>>;
    /// Marks the member removed at `at`; a member removed before keeps their first time.
    async fn remove_member(
        &mut self,
        project: ProjectId,
        member: MemberId,
        at: OffsetDateTime,
    ) -> StoreResult<()>;
    /// When the member's last call was recorded, if one ever was.
    async fn last_call(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Option<OffsetDateTime>>;
    /// Records that the member called the service at `at`, in place of any earlier call.
    async fn record_call(
        &mut self,
        project: ProjectId,
        member: MemberId,
        at: OffsetDateTime,
    ) -> StoreResult<()>;
    /// When each member that ever had a call recorded last called, ordered by member.
    async fn last_calls(
        &mut self,
        project: ProjectId,
    ) -> StoreResult<Vec<(MemberId, OffsetDateTime)>>;
}

/// API tokens.
#[async_trait]
pub trait TokenRows: Send {
    /// Stores a new token.
    async fn insert_token(&mut self, project: ProjectId, token: &TokenRecord) -> StoreResult<()>;
    /// The token of this hash, in whichever project it is.
    async fn token_by_hash(
        &mut self,
        hash: &TokenHash,
    ) -> StoreResult<Option<InProject<TokenRecord>>>;
    /// The token of this prefix.
    async fn token_by_prefix(
        &mut self,
        project: ProjectId,
        prefix: &TokenPrefix,
    ) -> StoreResult<Option<TokenRecord>>;
    /// Every token of a member, revoked ones included, oldest first.
    async fn tokens_of(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Vec<TokenRecord>>;
    /// Marks the token revoked at `at`; a token already revoked keeps its first time.
    async fn revoke_token(
        &mut self,
        project: ProjectId,
        hash: &TokenHash,
        at: OffsetDateTime,
    ) -> StoreResult<()>;
}

/// Sessions of members.
#[async_trait]
pub trait SessionRows: Send {
    /// The session of this id.
    async fn session(
        &mut self,
        project: ProjectId,
        id: SessionId,
    ) -> StoreResult<Option<SessionRecord>>;
    /// The member's session of this label.
    async fn session_by_label(
        &mut self,
        project: ProjectId,
        member: MemberId,
        label: &SessionLabel,
    ) -> StoreResult<Option<SessionRecord>>;
    /// Stores the session, replacing the one of the same id.
    async fn save_session(
        &mut self,
        project: ProjectId,
        session: &SessionRecord,
    ) -> StoreResult<()>;
}

/// Work items.
#[async_trait]
pub trait ItemRows: Send {
    /// Stores a new item.
    async fn insert_item(&mut self, project: ProjectId, item: &ItemRecord) -> StoreResult<()>;
    /// The item of this local id.
    async fn item(&mut self, project: ProjectId, id: ItemId) -> StoreResult<Option<ItemRecord>>;
    /// The item of this reference.
    async fn item_by_ref(
        &mut self,
        project: ProjectId,
        reference: &ExternalRef,
    ) -> StoreResult<Option<ItemRecord>>;
    /// The items of these ids, ordered by id; an id the project has no item of is left out.
    async fn items_of(
        &mut self,
        project: ProjectId,
        ids: &BTreeSet<ItemId>,
    ) -> StoreResult<Vec<ItemRecord>>;
    /// The items matching every filter given, ordered by id.
    async fn items(
        &mut self,
        project: ProjectId,
        filter: &ItemFilter,
    ) -> StoreResult<Vec<ItemRecord>>;
    /// Sets an item's status.
    async fn set_item_status(
        &mut self,
        project: ProjectId,
        id: ItemId,
        status: ItemStatus,
    ) -> StoreResult<()>;
    /// Replaces the facts of a stored item an importer supplies — title, hint, priority, tracker
    /// state, whether the tracker closed it, and its source — leaving its reference, status,
    /// repository and creation as they are.
    async fn update_item(&mut self, project: ProjectId, item: &ItemRecord) -> StoreResult<()>;
}

/// The project's work graph: which item is blocked by which.
#[async_trait]
pub trait DependencyRows: Send {
    /// Every edge, ordered by item and then by blocker.
    async fn dependencies(&mut self, project: ProjectId) -> StoreResult<Vec<Dependency>>;
    /// Stores an edge, replacing the one of the same pair (and so its source).
    async fn save_dependency(&mut self, project: ProjectId, edge: &Dependency) -> StoreResult<()>;
    /// Removes the edge of this pair, if there is one.
    async fn delete_dependency(
        &mut self,
        project: ProjectId,
        item: ItemId,
        blocked_by: ItemId,
    ) -> StoreResult<()>;
}

/// Claims and the paths recorded as shared under them.
#[async_trait]
pub trait ClaimRows: Send {
    /// The item's claim, live or stale.
    async fn claim(&mut self, project: ProjectId, item: ItemId) -> StoreResult<Option<Claim>>;
    /// Every claim, live and stale, ordered by item.
    async fn claims(&mut self, project: ProjectId) -> StoreResult<Vec<Claim>>;
    /// Stores the claim, replacing the item's previous one, with the areas it waits for. A claim
    /// waiting for an area another claim of the project waits for fails with
    /// [`StoreError::Duplicate`].
    async fn save_claim(&mut self, project: ProjectId, claim: &Claim) -> StoreResult<()>;
    /// Removes the item's claim, with the areas it waits for.
    async fn delete_claim(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()>;
    /// Every project with a claim that waits for an area, ordered by id.
    async fn pending_projects(&mut self) -> StoreResult<Vec<ProjectId>>;
    /// Every path recorded as shared, ordered by claim and path.
    async fn shared_paths(&mut self, project: ProjectId) -> StoreResult<Vec<SharedPath>>;
    /// Records a path as shared under a claim; recording it twice keeps one record.
    async fn insert_shared_path(
        &mut self,
        project: ProjectId,
        shared: &SharedPath,
    ) -> StoreResult<()>;
    /// Removes every path recorded under the item's claim.
    async fn delete_shared_paths(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()>;
}

/// What claims' branches change against their base, and the overlaps found between them.
#[async_trait]
pub trait RadarRows: Send {
    /// Every footprint, without its paths, ordered by item and repository.
    async fn footprints(&mut self, project: ProjectId) -> StoreResult<Vec<Footprint>>;
    /// The footprint of the item's claim in the repository, with its paths.
    async fn footprint(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
    ) -> StoreResult<Option<(Footprint, BTreeSet<RelPath>)>>;
    /// Stores the footprint with these paths, replacing the item's footprint of that repository
    /// and every path it had.
    async fn save_footprint(
        &mut self,
        project: ProjectId,
        footprint: &Footprint,
        paths: &BTreeSet<RelPath>,
    ) -> StoreResult<()>;
    /// Replaces what the radar reads of a stored footprint's paths, leaving the paths as they are;
    /// a footprint that does not exist is left alone.
    async fn set_footprint_digest(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
        digest: &FootprintDigest,
    ) -> StoreResult<()>;
    /// Removes the item's footprint of the repository, with its paths, if there is one.
    async fn delete_footprint(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
    ) -> StoreResult<()>;
    /// Removes every footprint of the item, with its paths.
    async fn delete_footprints(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()>;
    /// The paths of the repository's footprints, of any item, that are among `paths`, ordered by
    /// item and path. The cost follows how many are asked for, not how many are stored.
    async fn footprint_entries(
        &mut self,
        project: ProjectId,
        repo: &RepoId,
        paths: &BTreeSet<RelPath>,
    ) -> StoreResult<Vec<FootprintEntry>>;
    /// Every overlap stored, ordered by key.
    async fn overlaps(&mut self, project: ProjectId) -> StoreResult<Vec<Overlap>>;
    /// Stores the overlap, replacing the one of the same key.
    async fn save_overlap(&mut self, project: ProjectId, overlap: &Overlap) -> StoreResult<()>;
    /// Removes the overlap of this key, if there is one.
    async fn delete_overlap(&mut self, project: ProjectId, key: &OverlapKey) -> StoreResult<()>;
    /// Removes every overlap that names the item.
    async fn delete_overlaps_of(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()>;
}

/// Grants.
#[async_trait]
pub trait GrantRows: Send {
    /// Every grant, ordered by id.
    async fn grants(&mut self, project: ProjectId) -> StoreResult<Vec<GrantRecord>>;
    /// The grant of this id.
    async fn grant(&mut self, project: ProjectId, id: GrantId) -> StoreResult<Option<GrantRecord>>;
    /// Stores a new grant.
    async fn insert_grant(&mut self, project: ProjectId, grant: &GrantRecord) -> StoreResult<()>;
    /// Removes a grant.
    async fn delete_grant(&mut self, project: ProjectId, id: GrantId) -> StoreResult<()>;
}

/// Threads, their messages, and what each member has read.
#[async_trait]
pub trait ThreadRows: Send {
    /// Stores a new thread.
    async fn insert_thread(&mut self, project: ProjectId, thread: &ThreadRecord)
    -> StoreResult<()>;
    /// The thread of this id.
    async fn thread(
        &mut self,
        project: ProjectId,
        id: ThreadId,
    ) -> StoreResult<Option<ThreadRecord>>;
    /// Stores a new message.
    async fn insert_message(
        &mut self,
        project: ProjectId,
        message: &MessageRecord,
    ) -> StoreResult<()>;
    /// The message of this id.
    async fn message(
        &mut self,
        project: ProjectId,
        id: MessageId,
    ) -> StoreResult<Option<MessageRecord>>;
    /// The thread's messages, oldest first.
    async fn messages(
        &mut self,
        project: ProjectId,
        thread: ThreadId,
    ) -> StoreResult<Vec<MessageRecord>>;
    /// Every thread of the project — about `item` only, when given — with its counts as `reader`
    /// sees them, newest activity first. Whether the reader may read each thread is decided by the
    /// caller.
    async fn thread_summaries(
        &mut self,
        project: ProjectId,
        reader: MemberId,
        item: Option<ItemId>,
    ) -> StoreResult<Vec<ThreadSummary>>;
    /// Every thread whose newest message, or whose start when it has none, is older than
    /// `before`, ordered by id.
    async fn quiet_threads(
        &mut self,
        project: ProjectId,
        before: OffsetDateTime,
    ) -> StoreResult<Vec<ThreadActivity>>;
    /// Removes a thread with its recipients and its messages, and with each message what refers to
    /// it: read marks, acknowledgements, a wide claim's announcement, the record that it counted
    /// toward its author's limit, its deliveries and its escalation. Answers how many messages
    /// went; a thread that does not exist removes nothing.
    async fn delete_thread(&mut self, project: ProjectId, thread: ThreadId) -> StoreResult<u64>;
    /// Records every message of the thread that does not speak for `reader` as read by them;
    /// answers how many were not before. A member's own messages, and what dibs wrote on their
    /// behalf, are never unread, so they are never counted.
    async fn mark_read(
        &mut self,
        project: ProjectId,
        reader: MemberId,
        thread: ThreadId,
        at: OffsetDateTime,
    ) -> StoreResult<u64>;
}

/// The acknowledgements recipients recorded for messages, and when each member read them.
#[async_trait]
pub trait AckRows: Send {
    /// Stores an acknowledgement. A second one by the same member for the same message fails with
    /// [`StoreError::Duplicate`].
    async fn insert_ack(&mut self, project: ProjectId, ack: &Ack) -> StoreResult<()>;
    /// The acknowledgements of a message, oldest first.
    async fn acks(&mut self, project: ProjectId, message: MessageId) -> StoreResult<Vec<Ack>>;
    /// The acknowledgements of every message of a thread, oldest first.
    async fn thread_acks(&mut self, project: ProjectId, thread: ThreadId) -> StoreResult<Vec<Ack>>;
    /// Every message that asks to be acknowledged, that does not speak for `member` and that
    /// `member` has not acknowledged, newest first. Whether the member is one of its recipients is
    /// decided by the caller.
    async fn ack_requests(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Vec<MessageRecord>>;
    /// Who read a message and when, ordered by member.
    async fn reads_of(
        &mut self,
        project: ProjectId,
        message: MessageId,
    ) -> StoreResult<Vec<ReadMark>>;
}

/// The announcements wide claims sent to the holders of their areas.
#[async_trait]
pub trait WideAskRows: Send {
    /// Stores an announcement. A second one for the same message fails with
    /// [`StoreError::Duplicate`].
    async fn insert_wide_ask(&mut self, project: ProjectId, ask: &WideAsk) -> StoreResult<()>;
    /// The announcement the message carries, if it carries one.
    async fn wide_ask(
        &mut self,
        project: ProjectId,
        message: MessageId,
    ) -> StoreResult<Option<WideAsk>>;
    /// Every announcement its holder has not acknowledged yet, ordered by message.
    async fn open_wide_asks(&mut self, project: ProjectId) -> StoreResult<Vec<WideAsk>>;
}

/// The escalations members sent to the arbiters.
#[async_trait]
pub trait EscalationRows: Send {
    /// Stores an escalation. A second one for the same message fails with
    /// [`StoreError::Duplicate`].
    async fn insert_escalation(
        &mut self,
        project: ProjectId,
        escalation: &EscalationRecord,
    ) -> StoreResult<()>;
    /// The escalations that still wait — none of the arbiters stored with each has acknowledged its
    /// message — newest first. A settled one is never read, so the cost follows what is open, not
    /// the history.
    async fn open_escalations(&mut self, project: ProjectId) -> StoreResult<Vec<EscalationRecord>>;
}

/// What reached a member while they worked, one delivery per member and subject.
#[async_trait]
pub trait DeliveryRows: Send {
    /// Stores a delivery. A second one of the same subject to the same member fails with
    /// [`StoreError::Duplicate`], whichever session claims it.
    async fn insert_delivery(&mut self, project: ProjectId, delivery: &Delivery)
    -> StoreResult<()>;
    /// The member's delivery of the subject, if one was claimed.
    async fn delivery(
        &mut self,
        project: ProjectId,
        member: MemberId,
        subject: &DeliverySubject,
    ) -> StoreResult<Option<Delivery>>;
    /// The member's deliveries of any of these subjects, in the order of `subjects`.
    async fn deliveries(
        &mut self,
        project: ProjectId,
        member: MemberId,
        subjects: &[DeliverySubject],
    ) -> StoreResult<Vec<Delivery>>;
}

/// The high and urgent messages members wrote themselves, which the project limits per hour.
#[async_trait]
pub trait RaisedRows: Send {
    /// Records that `member` wrote `message`, counted toward their limit, at `at`. A message is
    /// recorded once; a second record of it fails with [`StoreError::Duplicate`].
    async fn insert_raised(
        &mut self,
        project: ProjectId,
        member: MemberId,
        message: MessageId,
        at: OffsetDateTime,
    ) -> StoreResult<()>;
    /// When the member's counted messages written at or after `since` were written, oldest
    /// first.
    async fn raised_since(
        &mut self,
        project: ProjectId,
        member: MemberId,
        since: OffsetDateTime,
    ) -> StoreResult<Vec<OffsetDateTime>>;
}

/// The holds, waiters and offers of the project's slots.
#[async_trait]
pub trait SlotRows: Send {
    /// Every hold, waiter and offer, holds and offers ordered by slot, waiters by place.
    async fn slot_board(&mut self, project: ProjectId) -> StoreResult<SlotBoard>;
    /// Replaces every hold, waiter and offer of the project with these.
    async fn put_slot_board(&mut self, project: ProjectId, board: &SlotBoard) -> StoreResult<()>;
    /// Every project with a hold, a waiter or an offer, ordered by id.
    async fn busy_slot_projects(&mut self) -> StoreResult<Vec<ProjectId>>;
}

/// The project's append-only event log.
#[async_trait]
pub trait EventRows: Send {
    /// Appends an event; its id is larger than every id the project's log had.
    async fn append_event(
        &mut self,
        project: ProjectId,
        at: OffsetDateTime,
        kind: &EventKind,
    ) -> StoreResult<EventId>;
    /// Up to `limit` events after `after` (from the first when `None`), oldest first.
    async fn events_after(
        &mut self,
        project: ProjectId,
        after: Option<EventId>,
        limit: usize,
    ) -> StoreResult<Vec<Event>>;
    /// The id of the newest event.
    async fn latest_event(&mut self, project: ProjectId) -> StoreResult<Option<EventId>>;
    /// Removes the project's events appended before `before` and remembers the newest id among
    /// them as the point a reader's cursor must not be behind; answers how many went.
    async fn delete_events_before(
        &mut self,
        project: ProjectId,
        before: OffsetDateTime,
    ) -> StoreResult<PrunedEvents>;
    /// The newest id removed from the project's log, `None` while nothing was ever removed.
    async fn events_pruned_through(&mut self, project: ProjectId) -> StoreResult<Option<EventId>>;
    /// Up to `limit` completions the log records at or after `since`, newest first (by time, then
    /// by position in the log).
    async fn completions_since(
        &mut self,
        project: ProjectId,
        since: OffsetDateTime,
        limit: usize,
    ) -> StoreResult<Vec<CompletionRecord>>;
}

/// The project's settings document.
#[async_trait]
pub trait SettingsRows: Send {
    /// The stored settings, `None` when the project never had any.
    async fn settings(&mut self, project: ProjectId) -> StoreResult<Option<SettingsDocument>>;
    /// Stores the settings, replacing the previous ones.
    async fn put_settings(
        &mut self,
        project: ProjectId,
        document: &SettingsDocument,
    ) -> StoreResult<()>;
}

/// Browser sessions.
#[async_trait]
pub trait UiSessionRows: Send {
    /// Stores a new browser session.
    async fn insert_ui_session(
        &mut self,
        project: ProjectId,
        session: &UiSessionRecord,
    ) -> StoreResult<()>;
    /// The browser session of this hash, in whichever project it is.
    async fn ui_session(
        &mut self,
        hash: &TokenHash,
    ) -> StoreResult<Option<InProject<UiSessionRecord>>>;
    /// Moves a browser session's end.
    async fn extend_ui_session(
        &mut self,
        project: ProjectId,
        hash: &TokenHash,
        expires_at: OffsetDateTime,
    ) -> StoreResult<()>;
    /// Removes a browser session.
    async fn delete_ui_session(&mut self, project: ProjectId, hash: &TokenHash) -> StoreResult<()>;
    /// Removes every browser session of a member; answers how many.
    async fn delete_ui_sessions_of(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<u64>;
    /// Removes the browser sessions of every project that ended before `now`; answers how many.
    async fn delete_expired_ui_sessions(&mut self, now: OffsetDateTime) -> StoreResult<u64>;
}
