//! The store: one unit of work at a time over every record of every project.
//!
//! A [`Tx`] is a transaction that writes exclusively from its start: while one is open, no other
//! writer runs, so a use case that loads rows, decides and writes inside one `Tx` decides against
//! state nobody else changes — that is what makes every compare-and-set of the domain true. Dropping
//! a `Tx` without [`Tx::commit`] discards everything it wrote.
//!
//! Every method takes the project first, so tenancy is in the signature. The only exceptions find a
//! record by a key unique across projects and return the project with it ([`TokenRows::token_by_hash`],
//! [`UiSessionRows::ui_session`]), create or look up a project itself ([`ProjectRows`]), or sweep
//! expired browser sessions of every project ([`UiSessionRows::delete_expired_ui_sessions`]).

use async_trait::async_trait;
use thiserror::Error;
use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::dependency::Dependency;
use dibs_domain::event::{Event, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{
    EventId, GrantId, ItemId, MemberId, MessageId, ProjectId, SessionId, ThreadId,
};
use dibs_domain::item::{ExternalRef, ItemStatus};
use dibs_domain::message::{Ack, SessionLabel};
use dibs_domain::reservation::SharedPath;
use dibs_domain::settings::SettingsDocument;
use dibs_domain::wide::WideAsk;

use crate::records::{
    CompletionRecord, GrantRecord, InProject, ItemFilter, ItemRecord, MemberRecord, MessageRecord,
    ProjectRecord, ReadMark, SessionRecord, ThreadRecord, ThreadSummary, TokenRecord,
    UiSessionRecord,
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
    + DependencyRows
    + GrantRows
    + ThreadRows
    + AckRows
    + WideAskRows
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
    /// Every member, ordered by handle.
    async fn members(&mut self, project: ProjectId) -> StoreResult<Vec<MemberRecord>>;
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
    /// Stores the claim, replacing the item's previous one.
    async fn save_claim(&mut self, project: ProjectId, claim: &Claim) -> StoreResult<()>;
    /// Removes the item's claim.
    async fn delete_claim(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()>;
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
    /// Records every message of the thread that `reader` did not write as read by them; answers
    /// how many were not before. A member's own messages are never unread, so they are never counted.
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
    /// Every message that asks to be acknowledged, that `member` did not write and has not
    /// acknowledged, newest first. Whether the member is one of its recipients is decided by the
    /// caller.
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
    /// Removes the browser sessions of every project that ended before `now`; answers how many.
    async fn delete_expired_ui_sessions(&mut self, now: OffsetDateTime) -> StoreResult<u64>;
}
