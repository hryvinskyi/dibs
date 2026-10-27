//! The records the store keeps and the use cases read and write. Each holds validated values only, so
//! a record read back from the store is as valid as the one written.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::escalation::EscalationSubject;
use dibs_domain::grant::Grant;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{
    EventId, GrantId, ItemId, MemberId, MessageId, ProjectId, SessionId, ThreadId,
};
use dibs_domain::import::KnownItem;
use dibs_domain::item::{
    ClaimOpening, ExternalRef, ItemMeta, ItemRepository, ItemShut, ItemStatus, Priority, SourceId,
    SourceStatus, Title, claim_opening,
};
use dibs_domain::member::Role;
use dibs_domain::message::{Author, Body, Importance, Recipients, SessionLabel, Subject};
use dibs_domain::ready::Candidate;
use dibs_domain::session::HostName;

use crate::token::{TokenHash, TokenPrefix};
use dibs_domain::member::DisplayName;
use dibs_domain::member::ExternalLogin;
use dibs_domain::project::ProjectSlug;
use dibs_domain::session::AgentLabel;

/// A record found without knowing its project beforehand, together with the project it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InProject<T> {
    /// The project the record belongs to.
    pub project: ProjectId,
    /// The record.
    pub record: T,
}

/// A project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRecord {
    /// Its id.
    pub id: ProjectId,
    /// The slug people address it by; unique among projects.
    pub slug: ProjectSlug,
    /// When it was created.
    pub created_at: OffsetDateTime,
}

/// A member of a project: a person, or an integration bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRecord {
    /// Its id.
    pub id: MemberId,
    /// The handle, unique within the project.
    pub handle: Handle,
    /// The name it is shown by.
    pub display_name: DisplayName,
    /// Its login in an outside system, when it has one.
    pub external_login: Option<ExternalLogin>,
    /// What it may do.
    pub role: Role,
    /// When it joined.
    pub created_at: OffsetDateTime,
    /// When it was removed from the project, if it was. A removed member is kept, so the records
    /// that name them still read, but takes part in nothing any more.
    pub removed_at: Option<OffsetDateTime>,
}

impl MemberRecord {
    /// Whether the member is still in the project.
    pub fn is_present(&self) -> bool {
        self.removed_at.is_none()
    }
}

/// An API token of a member, kept as its hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    /// The hash of the token; unique among all tokens.
    pub hash: TokenHash,
    /// The token's prefix; unique within the project.
    pub prefix: TokenPrefix,
    /// The member it authenticates.
    pub member: MemberId,
    /// When it was issued.
    pub created_at: OffsetDateTime,
    /// When it was revoked, if it was.
    pub revoked_at: Option<OffsetDateTime>,
}

impl TokenRecord {
    /// Whether the token still authenticates.
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
}

/// One running agent process, MCP server or browser of a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Its id.
    pub id: SessionId,
    /// Its member.
    pub member: MemberId,
    /// The machine it runs on.
    pub host: HostName,
    /// Its label; unique per member.
    pub label: SessionLabel,
    /// The kind of agent it runs, as reported.
    pub agent: AgentLabel,
    /// Whether it can be woken while idle.
    pub push: bool,
    /// When it last registered or sent a heartbeat.
    pub last_seen: OffsetDateTime,
}

/// A work item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemRecord {
    /// Its local id.
    pub id: ItemId,
    /// Its reference in an outside tracker; unique within the project.
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// Where it stands.
    pub status: ItemStatus,
    /// The repository it belongs to, as its reference said when it was created.
    pub repository: ItemRepository,
    /// The areas the work will probably touch.
    pub areas_hint: BTreeSet<QualifiedArea>,
    /// How soon it should be done.
    pub priority: Priority,
    /// The outside tracker's own state of it.
    pub source_status: Option<SourceStatus>,
    /// Whether the outside tracker closed it.
    pub closed: bool,
    /// The source an importer listed it from.
    pub source: Option<SourceId>,
    /// Who created it.
    pub created_by: MemberId,
    /// When it was created.
    pub created_at: OffsetDateTime,
}

impl AsRef<Grant> for GrantRecord {
    fn as_ref(&self) -> &Grant {
        &self.grant
    }
}

impl ItemRecord {
    /// A new open item with only a title: no hint, the default priority, no tracker state, listed
    /// by no source.
    pub fn new(
        id: ItemId,
        external_ref: Option<ExternalRef>,
        title: Title,
        repository: ItemRepository,
        created_by: MemberId,
        created_at: OffsetDateTime,
    ) -> Self {
        Self {
            id,
            external_ref,
            title,
            status: ItemStatus::Open,
            repository,
            areas_hint: BTreeSet::new(),
            priority: Priority::default(),
            source_status: None,
            closed: false,
            source: None,
            created_by,
            created_at,
        }
    }

    /// Whether the item is done or dropped.
    pub fn is_closed(&self) -> bool {
        matches!(self.status, ItemStatus::Done | ItemStatus::Dropped)
    }

    /// How a claim may be made on the item, given where it stands and what its tracker says.
    ///
    /// # Errors
    /// [`ItemShut`] when the item is done or dropped and no tracker has it open.
    pub const fn claim_opening(&self) -> Result<ClaimOpening, ItemShut> {
        claim_opening(self.status, self.source.is_some(), self.closed)
    }

    /// The facts an importer supplies about the item.
    pub fn meta(&self) -> ItemMeta {
        ItemMeta {
            title: self.title.clone(),
            areas_hint: self.areas_hint.clone(),
            priority: self.priority,
            source_status: self.source_status.clone(),
            closed: self.closed,
        }
    }

    /// Replaces the facts an importer supplies about the item.
    pub fn set_meta(&mut self, meta: ItemMeta) {
        self.title = meta.title;
        self.areas_hint = meta.areas_hint;
        self.priority = meta.priority;
        self.source_status = meta.source_status;
        self.closed = meta.closed;
    }

    /// The item as an import compares it.
    pub fn known(&self) -> KnownItem {
        KnownItem {
            id: self.id,
            reference: self.external_ref.clone(),
            status: self.status,
            repository: self.repository.clone(),
            meta: self.meta(),
            source: self.source.clone(),
        }
    }

    /// The item as the ready list weighs it.
    pub fn candidate(&self) -> Candidate<'_> {
        Candidate {
            id: self.id,
            status: self.status,
            closed: self.closed,
            priority: self.priority,
            areas_hint: &self.areas_hint,
            created_at: self.created_at,
        }
    }
}

/// Which items to list. Every filter given must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemFilter {
    /// Only items with this status.
    pub status: Option<ItemStatus>,
    /// Only items this member holds a claim on, live or stale.
    pub holder: Option<MemberId>,
    /// Only the item with this reference.
    pub external_ref: Option<ExternalRef>,
}

/// A stored grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRecord {
    /// Its id.
    pub id: GrantId,
    /// What was granted.
    pub grant: Grant,
    /// When.
    pub created_at: OffsetDateTime,
}

/// A conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRecord {
    /// Its id.
    pub id: ThreadId,
    /// Its subject.
    pub subject: Subject,
    /// The work item it is about, if any.
    pub item: Option<ItemId>,
    /// Who started it: a member, or dibs.
    pub author: Author,
    /// Whom it is addressed to.
    pub recipients: Recipients,
    /// When it started.
    pub created_at: OffsetDateTime,
}

/// One message of a conversation. Messages never change once written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRecord {
    /// Its id.
    pub id: MessageId,
    /// The thread.
    pub thread: ThreadId,
    /// Who wrote it: a member, or dibs.
    pub author: Author,
    /// The label of the session it was written from.
    pub label: SessionLabel,
    /// Its text.
    pub body: Body,
    /// How much it asks for its recipients' attention.
    pub importance: Importance,
    /// Whether each of its recipients is asked to acknowledge it.
    pub ack_required: bool,
    /// When it was written.
    pub created_at: OffsetDateTime,
}

/// That a member read a message, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadMark {
    /// The reader.
    pub member: MemberId,
    /// When they read it.
    pub at: OffsetDateTime,
}

/// A thread with its message counts, as one reader sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSummary {
    /// The thread.
    pub thread: ThreadRecord,
    /// How many messages it has.
    pub messages: u64,
    /// How many of them the reader has not read and did not write.
    pub unread: u64,
    /// When its newest message was written.
    pub last_message_at: Option<OffsetDateTime>,
    /// The highest importance among its messages.
    pub importance: Importance,
}

/// An item completed, as the log records it: the item, the member who held it, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRecord {
    /// The item.
    pub item: ItemId,
    /// The member whose claim completed it.
    pub member: MemberId,
    /// When.
    pub at: OffsetDateTime,
}

/// A browser session, kept as the hash of its id and bound to the token that opened it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiSessionRecord {
    /// The hash of the session id; unique among all browser sessions.
    pub hash: TokenHash,
    /// The member signed in.
    pub member: MemberId,
    /// The hash of the token that opened it.
    pub token: TokenHash,
    /// When it ends unless it is used before then.
    pub expires_at: OffsetDateTime,
}

/// An escalation: the message that asked the arbiters, what it is about, and whom it asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationRecord {
    /// The message, written by dibs, that asks for an acknowledgement.
    pub message: MessageId,
    /// Its thread.
    pub thread: ThreadId,
    /// What it is about.
    pub subject: EscalationSubject,
    /// The member who escalated.
    pub by: MemberId,
    /// The arbiters it reached; one of them acknowledging it settles it.
    pub arbiters: BTreeSet<MemberId>,
    /// When.
    pub created_at: OffsetDateTime,
}

/// When a conversation was last active, and the work item it is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadActivity {
    /// The thread.
    pub thread: ThreadId,
    /// The work item it is about, if any.
    pub item: Option<ItemId>,
    /// When its newest message was written, or when it started if it has none.
    pub last_activity: OffsetDateTime,
}

/// What removing a project's old events did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrunedEvents {
    /// How many went.
    pub count: u64,
    /// The newest id among them, if any went.
    pub through: Option<EventId>,
}
