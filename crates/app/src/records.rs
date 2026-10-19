//! The records the store keeps and the use cases read and write. Each holds validated values only, so
//! a record read back from the store is as valid as the one written.

use time::OffsetDateTime;

use dibs_domain::grant::Grant;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{GrantId, ItemId, MemberId, MessageId, ProjectId, SessionId, ThreadId};
use dibs_domain::item::{ExternalRef, ItemRepository, ItemStatus, Title};
use dibs_domain::member::Role;
use dibs_domain::message::{Body, Recipients, SessionLabel, Subject};
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
    /// Whether the item is done or dropped, so nobody may claim it any more.
    pub fn is_closed(&self) -> bool {
        matches!(self.status, ItemStatus::Done | ItemStatus::Dropped)
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
    /// Who started it.
    pub author: MemberId,
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
    /// Who wrote it.
    pub author: MemberId,
    /// The label of the session it was written from.
    pub label: SessionLabel,
    /// Its text.
    pub body: Body,
    /// When it was written.
    pub created_at: OffsetDateTime,
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
