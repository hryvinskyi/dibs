//! What the routes answer: the results of the use cases written as JSON. Members are named by the
//! handle the use case read beside each id, ids are numbers, times are RFC 3339 in UTC, areas are
//! `repo:area`.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use time::OffsetDateTime;

use dibs_app::named::{
    ItemName, NamedClaim, NamedEvent, NamedGrant, NamedItem, NamedMessage, NamedSummary,
    NamedThread, NamedToken, NamedVerdict,
};
use dibs_app::records::{MemberRecord, SessionRecord};
use dibs_app::use_cases::{
    Acknowledged, DeliverableMessage, ExcludedEntry, FollowedEvent, HeldItem, NamedNotReady,
    NamedOverlap, ReadyEntry, ReadyView as ReadyList, RecipientAck, RecipientStatus,
    UnackedMessage,
};
use dibs_domain::area::QualifiedArea;
use dibs_domain::delivery::Delivery;
use dibs_domain::edit::{HeldReason, Verdict};
use dibs_domain::event::EventKind;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::{HotRuleKind, Note};
use dibs_domain::ids::{EventId, GrantId, ItemId, MemberId, MessageId, SessionId, ThreadId};
use dibs_domain::item::{
    ExternalRef, ItemRepository, ItemStatus, Priority, SourceId, SourceStatus, Title,
};
use dibs_domain::member::{DisplayName, ExternalLogin, Role};
use dibs_domain::message::{Body, Importance, Recipients, SessionLabel, Subject};
use dibs_domain::paths::RepoPath;
use dibs_domain::radar::{BranchName, OverlapKind};
use dibs_domain::repo::{RemoteUrl, RepoId};
use dibs_domain::review::{PrRef, Review};
use dibs_domain::session::{AgentLabel, HostName};

use crate::error::ApiError;

pub mod slots;

/// A JSON answer of this status. A value that cannot be written answers the internal error.
pub fn reply<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_string(value) {
        Ok(body) => (
            status,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )],
            body,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "an answer could not be written");
            ApiError::internal().into_response()
        }
    }
}

/// A member.
#[derive(Debug, Serialize)]
pub struct MemberView {
    /// The handle.
    pub handle: Handle,
    /// The name shown.
    pub display_name: DisplayName,
    /// The login elsewhere.
    pub external_login: Option<ExternalLogin>,
    /// The role.
    pub role: Role,
}

impl MemberView {
    /// The view of a member.
    pub fn of(member: &MemberRecord) -> Self {
        Self {
            handle: member.handle.clone(),
            display_name: member.display_name.clone(),
            external_login: member.external_login.clone(),
            role: member.role,
        }
    }
}

/// A session.
#[derive(Debug, Serialize)]
pub struct SessionView {
    /// Its id.
    pub session_id: SessionId,
    /// Its machine.
    pub host: HostName,
    /// Its label.
    pub label: SessionLabel,
    /// The kind of agent it runs.
    pub agent: AgentLabel,
    /// Whether it can be woken while idle.
    pub push: bool,
    /// When it was last seen.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub last_seen: OffsetDateTime,
}

impl SessionView {
    /// The view of a session.
    pub fn of(session: SessionRecord) -> Self {
        Self {
            session_id: session.id,
            host: session.host,
            label: session.label,
            agent: session.agent,
            push: session.push,
            last_seen: session.last_seen,
        }
    }
}

/// A work item.
#[derive(Debug, Serialize)]
pub struct ItemView {
    /// Its local id.
    pub id: ItemId,
    /// Its reference in an outside tracker.
    #[serde(rename = "ref")]
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// Where it stands.
    pub status: ItemStatus,
    /// The project's repository it belongs to, when its reference names a listed one.
    pub repo: Option<RepoId>,
    /// The repository its reference names when the project does not list it.
    pub unlisted_remote: Option<RemoteUrl>,
    /// How soon it should be done: 0 to 4, lower is sooner.
    pub priority: Priority,
    /// The areas the work will probably touch.
    pub areas_hint: BTreeSet<QualifiedArea>,
    /// The outside tracker's own state of it.
    pub source_status: Option<SourceStatus>,
    /// Whether the outside tracker closed it.
    pub closed: bool,
    /// The importer's source that lists it.
    pub source: Option<SourceId>,
    /// Who created it.
    pub created_by: Handle,
    /// When.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub created_at: OffsetDateTime,
}

impl ItemView {
    /// The view of an item.
    pub fn of(named: NamedItem) -> Self {
        let item = named.item;
        let (repo, unlisted_remote) = match item.repository {
            ItemRepository::Listed(repo) => (Some(repo), None),
            ItemRepository::Unlisted(remote) => (None, Some(remote)),
            ItemRepository::Unnamed => (None, None),
        };
        Self {
            id: item.id,
            external_ref: item.external_ref,
            title: item.title,
            status: item.status,
            repo,
            unlisted_remote,
            priority: item.priority,
            areas_hint: item.areas_hint,
            source_status: item.source_status,
            closed: item.closed,
            source: item.source,
            created_by: named.created_by,
            created_at: item.created_at,
        }
    }
}

/// An item another answer mentions: its id, reference, title and tracker page.
#[derive(Debug, Serialize)]
pub struct ItemNameView {
    /// Its local id.
    pub id: ItemId,
    /// Its reference in an outside tracker.
    #[serde(rename = "ref")]
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// The tracker's page for it.
    pub link: Option<String>,
}

impl ItemNameView {
    /// The view of a mentioned item.
    pub fn of(name: ItemName) -> Self {
        Self {
            id: name.id,
            external_ref: name.external_ref,
            title: name.title,
            link: name.link,
        }
    }
}

/// An item of the ready list.
#[derive(Debug, Serialize)]
pub struct ReadyItemView {
    /// The item.
    pub item: ItemView,
    /// The tracker's page for it.
    pub link: Option<String>,
    /// The member whose claim on it went stale, when one did.
    pub stale_holder: Option<Handle>,
}

/// Why an open item is not ready, tagged by `reason`.
#[derive(Debug, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NotReadyView {
    /// A member's live claim holds it.
    Claimed {
        /// The holder.
        holder: Handle,
    },
    /// Items it waits for are neither done nor closed.
    BlockedBy {
        /// Those items.
        items: Vec<ItemNameView>,
    },
    /// An area of its hint is held by another member's live claim.
    AreaHeld {
        /// The area.
        area: QualifiedArea,
        /// The holder.
        holder: Handle,
        /// The holder's item.
        item: ItemNameView,
    },
}

impl NotReadyView {
    /// The view of a reason.
    pub fn of(reason: NamedNotReady) -> Self {
        match reason {
            NamedNotReady::Claimed { holder } => Self::Claimed { holder },
            NamedNotReady::BlockedBy { items } => Self::BlockedBy {
                items: items.into_iter().map(ItemNameView::of).collect(),
            },
            NamedNotReady::AreaHeld { area, holder, item } => Self::AreaHeld {
                area,
                holder,
                item: ItemNameView::of(item),
            },
        }
    }
}

/// An open item left out of the ready list.
#[derive(Debug, Serialize)]
pub struct ExcludedItemView {
    /// The item.
    pub item: ItemView,
    /// The tracker's page for it.
    pub link: Option<String>,
    /// Every reason why it is not ready.
    pub reasons: Vec<NotReadyView>,
}

/// The ready list and the open items left out of it.
#[derive(Debug, Serialize)]
pub struct ReadyView {
    /// The items the caller can pick up, soonest first.
    pub ready: Vec<ReadyItemView>,
    /// The open items that are not ready, in the same order.
    pub excluded: Vec<ExcludedItemView>,
}

impl ReadyView {
    /// The view of a ready list.
    pub fn of(list: ReadyList) -> Self {
        Self {
            ready: list
                .ready
                .into_iter()
                .map(|entry: ReadyEntry| ReadyItemView {
                    item: ItemView::of(entry.item),
                    link: entry.link,
                    stale_holder: entry.stale_holder,
                })
                .collect(),
            excluded: list
                .excluded
                .into_iter()
                .map(|entry: ExcludedEntry| ExcludedItemView {
                    item: ItemView::of(entry.item),
                    link: entry.link,
                    reasons: entry.reasons.into_iter().map(NotReadyView::of).collect(),
                })
                .collect(),
        }
    }
}

/// A claim.
#[derive(Debug, Serialize)]
pub struct ClaimView {
    /// The item held.
    pub item: ItemId,
    /// The member holding it.
    pub holder: Handle,
    /// The session that took or last renewed it.
    pub session_id: SessionId,
    /// That session's machine.
    pub host: HostName,
    /// The areas it declared.
    pub areas: BTreeSet<QualifiedArea>,
    /// For a wide claim, the areas it names and holds none of, and why; `null` otherwise.
    pub wide: Option<WideScopeView>,
    /// When it was first claimed.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub claimed_at: OffsetDateTime,
    /// When it was last claimed again.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub renewed_at: OffsetDateTime,
    /// The last sign of life.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub last_seen: OffsetDateTime,
    /// `working` while its holder works on it, `in_review` while its work waits in pull requests.
    pub state: &'static str,
    /// While it is in review, the pull requests it recorded; `null` otherwise.
    pub review: Option<ReviewView>,
    /// The branch its work is on in each repository, by repository id, as its holder's checkout
    /// was last seen.
    pub branches: BTreeMap<RepoId, BranchName>,
}

/// The review of a claim in review.
#[derive(Debug, Serialize)]
pub struct ReviewView {
    /// The pull requests it recorded, in order.
    pub prs: BTreeSet<PrRef>,
    /// Since when it is in review.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub since: OffsetDateTime,
    /// Whether more pull requests of the item follow these.
    pub more: bool,
}

impl ReviewView {
    /// The view of a review.
    pub fn of(review: &Review) -> Self {
        Self {
            prs: review.prs().clone(),
            since: review.since(),
            more: review.more(),
        }
    }
}

impl ClaimView {
    /// The view of a claim.
    pub fn of(named: NamedClaim) -> Self {
        let claim = named.claim;
        Self {
            item: claim.item,
            holder: named.holder,
            session_id: claim.session,
            host: claim.host,
            areas: claim.areas,
            wide: claim.wide.map(|scope| WideScopeView {
                areas: scope.areas().clone(),
                reason: scope.reason().as_str().to_owned(),
            }),
            claimed_at: claim.claimed_at,
            renewed_at: claim.renewed_at,
            last_seen: claim.last_seen,
            state: claim.state.code(),
            review: claim.state.review().map(ReviewView::of),
            branches: claim.branches,
        }
    }
}

/// An overlap between live claims, or a moved base under one.
#[derive(Debug, Serialize)]
pub struct OverlapView {
    /// `same_file`, `contract` or `main_moved`.
    pub kind: OverlapKind,
    /// The items it names, each with its holder.
    pub items: Vec<OverlapItemView>,
    /// The merged pull request of a moved base; `null` otherwise.
    pub pr: Option<PrRef>,
    /// The paths it is on, `repo:path`, in order.
    pub paths: BTreeSet<RepoPath>,
}

/// One item an overlap names.
#[derive(Debug, Serialize)]
pub struct OverlapItemView {
    /// The item.
    #[serde(flatten)]
    pub item: ItemNameView,
    /// The holder of its claim.
    pub holder: Handle,
}

impl OverlapView {
    /// The view of an overlap.
    pub fn of(named: NamedOverlap) -> Self {
        Self {
            kind: named.overlap.kind(),
            pr: named.overlap.pr().cloned(),
            paths: named.overlap.paths().clone(),
            items: named
                .items
                .into_iter()
                .map(|(item, holder)| OverlapItemView {
                    item: ItemNameView::of(item),
                    holder,
                })
                .collect(),
        }
    }
}

/// The scope of a wide claim.
#[derive(Debug, Serialize)]
pub struct WideScopeView {
    /// The areas it names.
    pub areas: BTreeSet<QualifiedArea>,
    /// Why.
    pub reason: String,
}

/// An item with its claim.
#[derive(Debug, Serialize)]
pub struct HeldItemView {
    /// The item.
    pub item: ItemView,
    /// Its claim.
    pub claim: ClaimView,
}

impl HeldItemView {
    /// The view of a held item.
    pub fn of(held: HeldItem) -> Self {
        Self {
            item: ItemView::of(held.item),
            claim: ClaimView::of(held.claim),
        }
    }
}

/// A grant.
#[derive(Debug, Serialize)]
pub struct GrantView {
    /// Its id.
    pub id: GrantId,
    /// The grantee's work item.
    pub item: ItemId,
    /// The area.
    pub area: QualifiedArea,
    /// The holder who granted.
    pub from: Handle,
    /// The member who may edit.
    pub to: Handle,
    /// When.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub created_at: OffsetDateTime,
}

impl GrantView {
    /// The view of a grant.
    pub fn of(named: NamedGrant) -> Self {
        Self {
            id: named.record.id,
            item: named.record.grant.item,
            area: named.record.grant.area,
            from: named.from,
            to: named.to,
            created_at: named.record.created_at,
        }
    }
}

/// A member's live claim on an item.
#[derive(Debug, Serialize)]
pub struct HolderView {
    /// The member.
    pub holder: Handle,
    /// The item.
    pub item: ItemId,
    /// The item's reference, when it has one.
    pub item_ref: Option<ExternalRef>,
}

/// The verdict on one path.
#[derive(Debug, Serialize)]
pub struct PathVerdictView {
    /// The path's repository.
    pub repo: RepoId,
    /// The path inside it.
    pub path: String,
    /// The verdict.
    #[serde(flatten)]
    pub verdict: VerdictView,
}

/// A verdict, tagged by `verdict`.
#[derive(Debug, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum VerdictView {
    /// The member may edit.
    Allowed,
    /// The member may edit; a hot-file rule has something to say.
    AllowedWithNote {
        /// The rule's kind.
        rule: HotRuleKind,
        /// The rule's note.
        note: Option<Note>,
        /// Other members' live claims that edited the same file.
        touched_by: Vec<HolderView>,
    },
    /// The path needs a live claim first.
    NeedsClaim,
    /// Another member holds the path's area.
    HeldBy {
        /// The holder.
        holder: Option<Handle>,
        /// The holder's item.
        item: ItemId,
        /// The holder's item's reference, when it has one.
        item_ref: Option<ExternalRef>,
        /// The area.
        area: QualifiedArea,
        /// The way on: `in_review`, the holder's claim is in review and the area is free once its
        /// pull requests merge; `ask_holder`, message the holder or ask for a grant;
        /// `awaiting_ack`, the caller's wide claim names the area and the holder has not
        /// acknowledged its announcement.
        reason: &'static str,
        /// For `in_review`, the pull requests the holder's claim recorded; empty otherwise.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        prs: Vec<PrRef>,
    },
    /// The holder granted the member edits in the area.
    Granted {
        /// The holder who granted.
        by: Option<Handle>,
        /// The area.
        area: QualifiedArea,
    },
}

impl PathVerdictView {
    /// The view of one path's verdict.
    pub fn of(named: NamedVerdict) -> Self {
        let path = named.verdict.path;
        let view = match named.verdict.verdict {
            Verdict::Allowed => VerdictView::Allowed,
            Verdict::AllowedWithNote {
                rule,
                note,
                touched_by,
            } => VerdictView::AllowedWithNote {
                rule,
                note,
                touched_by: touched_by
                    .into_iter()
                    .zip(named.touched_by)
                    .zip(
                        named
                            .touched_refs
                            .into_iter()
                            .chain(std::iter::repeat(None)),
                    )
                    .map(|((holder, handle), item_ref)| HolderView {
                        holder: handle,
                        item: holder.item,
                        item_ref,
                    })
                    .collect(),
            },
            Verdict::NeedsClaim => VerdictView::NeedsClaim,
            Verdict::HeldBy {
                item, area, reason, ..
            } => VerdictView::HeldBy {
                holder: named.member,
                item,
                item_ref: named.item_ref,
                area,
                reason: reason.code(),
                prs: match reason {
                    HeldReason::InReview { prs } => prs,
                    HeldReason::AskHolder | HeldReason::AwaitingAck => Vec::new(),
                },
            },
            Verdict::Granted { area, .. } => VerdictView::Granted {
                by: named.member,
                area,
            },
        };
        Self {
            repo: path.repo,
            path: path.path.as_str().to_owned(),
            verdict: view,
        }
    }
}

/// A conversation.
#[derive(Debug, Serialize)]
pub struct ThreadView {
    /// Its id.
    pub id: ThreadId,
    /// Its subject.
    pub subject: Subject,
    /// The work item it is about.
    pub item: Option<ItemId>,
    /// Who started it: a member's handle, or `dibs`.
    pub author: String,
    /// Whether dibs started it, in the project's name.
    pub system: bool,
    /// Whether it is addressed to every person in the project.
    pub to_project: bool,
    /// The members it is addressed to; empty when it is addressed to the project.
    pub to: Vec<Handle>,
    /// When it started.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub created_at: OffsetDateTime,
}

impl ThreadView {
    /// The view of a thread.
    pub fn of(named: NamedThread) -> Self {
        let thread = named.record;
        Self {
            id: thread.id,
            subject: thread.subject,
            item: thread.item,
            system: named.author.is_dibs(),
            author: named.author.to_string(),
            to_project: matches!(thread.recipients, Recipients::Project),
            to: named.recipients,
            created_at: thread.created_at,
        }
    }
}

/// A message.
#[derive(Debug, Serialize)]
pub struct MessageView {
    /// Its id.
    pub id: MessageId,
    /// Its thread.
    pub thread: ThreadId,
    /// Who wrote it: a member's handle, or `dibs`.
    pub author: String,
    /// Whether dibs wrote it, in the project's name: it counts toward no limit and is shown as
    /// from dibs.
    pub system: bool,
    /// The label of the session it was written from.
    pub label: SessionLabel,
    /// Its text.
    pub body: Body,
    /// How much it asks for its recipients' attention.
    pub importance: Importance,
    /// Whether each recipient is asked to acknowledge it.
    pub ack_required: bool,
    /// When it was written.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub created_at: OffsetDateTime,
    /// In a thread read, for a message that asks to be acknowledged: each recipient and when they
    /// acknowledged it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acks: Option<Vec<RecipientAckView>>,
}

impl MessageView {
    /// The view of a message.
    pub fn of(named: NamedMessage) -> Self {
        let message = named.record;
        Self {
            id: message.id,
            thread: message.thread,
            system: named.author.is_dibs(),
            author: named.author.to_string(),
            label: message.label,
            body: message.body,
            importance: message.importance,
            ack_required: message.ack_required,
            created_at: message.created_at,
            acks: None,
        }
    }

    /// The view of a message of a thread read, with its recipients' acknowledgements when it asks
    /// for them.
    pub fn in_thread(named: NamedMessage, acks: Option<Vec<RecipientAck>>) -> Self {
        Self {
            acks: acks.map(|acks| acks.into_iter().map(RecipientAckView::of).collect()),
            ..Self::of(named)
        }
    }
}

/// One recipient of a message that asks to be acknowledged.
#[derive(Debug, Serialize)]
pub struct RecipientAckView {
    /// The recipient.
    pub member: Handle,
    /// When they acknowledged it; `null` while they have not.
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    pub acked_at: Option<OffsetDateTime>,
}

impl RecipientAckView {
    /// The view of one recipient's acknowledgement.
    pub fn of(ack: RecipientAck) -> Self {
        Self {
            member: ack.handle,
            acked_at: ack.acked_at,
        }
    }
}

/// An acknowledgement recorded.
#[derive(Debug, Serialize)]
pub struct AckView {
    /// The message.
    pub message: MessageId,
    /// Its thread.
    pub thread: ThreadId,
    /// The recipient who acknowledged it.
    pub member: Handle,
    /// When.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub at: OffsetDateTime,
}

impl AckView {
    /// The view of an acknowledgement.
    pub fn of(acknowledged: Acknowledged) -> Self {
        Self {
            message: acknowledged.ack.message,
            thread: acknowledged.thread,
            member: acknowledged.member,
            at: acknowledged.ack.at,
        }
    }
}

/// One recipient of a message its author sent: when they read it and acknowledged it.
#[derive(Debug, Serialize)]
pub struct RecipientStatusView {
    /// The recipient.
    pub member: Handle,
    /// When they read it.
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    pub read_at: Option<OffsetDateTime>,
    /// When they acknowledged it.
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    pub acked_at: Option<OffsetDateTime>,
}

impl RecipientStatusView {
    /// The view of one recipient's standing.
    pub fn of(status: RecipientStatus) -> Self {
        Self {
            member: status.member,
            read_at: status.read_at,
            acked_at: status.acked_at,
        }
    }
}

/// A message awaiting the caller's acknowledgement, with its thread.
#[derive(Debug, Serialize)]
pub struct UnackedView {
    /// The thread.
    pub thread: ThreadView,
    /// The message.
    pub message: MessageView,
}

impl UnackedView {
    /// The view of an unacknowledged message.
    pub fn of(unacked: UnackedMessage) -> Self {
        Self {
            thread: ThreadView::of(unacked.thread),
            message: MessageView::of(unacked.message),
        }
    }
}

/// A message still to be delivered to the caller's agent, with its thread.
#[derive(Debug, Serialize)]
pub struct DeliverableView {
    /// The thread.
    pub thread: ThreadView,
    /// The message.
    pub message: MessageView,
    /// Whether it awaits the caller's acknowledgement.
    pub awaiting_ack: bool,
}

impl DeliverableView {
    /// The view of a deliverable message.
    pub fn of(entry: DeliverableMessage) -> Self {
        Self {
            thread: ThreadView::of(entry.thread),
            message: MessageView::of(entry.message),
            awaiting_ack: entry.awaiting_ack,
        }
    }
}

/// One delivery claimed: what, by which session, and when.
#[derive(Debug, Serialize)]
pub struct DeliveryView {
    /// The subject's key: a message id, or `offer:<slot>:<deadline>`.
    pub subject: String,
    /// The session that delivers it.
    pub session_id: SessionId,
    /// When it was claimed.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub at: OffsetDateTime,
}

impl DeliveryView {
    /// The view of a delivery.
    pub fn of(delivery: &Delivery) -> Self {
        Self {
            subject: delivery.subject.key(),
            session_id: delivery.session,
            at: delivery.at,
        }
    }
}

/// A conversation with its counts as the caller sees them.
#[derive(Debug, Serialize)]
pub struct ThreadSummaryView {
    /// The thread.
    pub thread: ThreadView,
    /// How many messages it has.
    pub messages: u64,
    /// How many of them the caller has not read and did not write.
    pub unread: u64,
    /// When its newest message was written.
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    pub last_message_at: Option<OffsetDateTime>,
    /// The highest importance among its messages.
    pub importance: Importance,
    /// How many of its messages ask the caller for an acknowledgement they have not given.
    pub awaiting_ack: u64,
}

impl ThreadSummaryView {
    /// The view of a summary.
    pub fn of(named: NamedSummary) -> Self {
        Self {
            thread: ThreadView::of(named.thread),
            messages: named.messages,
            unread: named.unread,
            last_message_at: named.last_message_at,
            importance: named.importance,
            awaiting_ack: named.awaiting_ack,
        }
    }
}

/// An event of the log: its id, time, kind and payload, the handles of the members the payload
/// names, and for a follower who may read it the message a `message_posted` event announces.
#[derive(Debug, Serialize)]
pub struct EventView {
    /// Its position in the log.
    pub id: EventId,
    /// When it happened.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub at: OffsetDateTime,
    /// `kind` and `payload`.
    #[serde(flatten)]
    pub kind: EventKind,
    /// The handle of every member the payload names, keyed by id.
    pub members: BTreeMap<MemberId, Handle>,
    /// The message announced, on the event stream only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageView>,
}

impl EventView {
    /// The view of an event of the log.
    pub fn of(named: NamedEvent) -> Self {
        Self {
            id: named.event.id,
            at: named.event.at,
            kind: named.event.kind,
            members: named.members,
            message: None,
        }
    }

    /// The view of an event as a follower receives it.
    pub fn followed(followed: FollowedEvent) -> Self {
        Self {
            message: followed.message.map(MessageView::of),
            ..Self::of(followed.event)
        }
    }
}

/// A token, named by its prefix; never the secret itself.
#[derive(Debug, Serialize)]
pub struct TokenView {
    /// Its prefix.
    pub prefix: String,
    /// The member it authenticates.
    pub member: Handle,
    /// When it was issued.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub created_at: OffsetDateTime,
    /// When it was revoked.
    #[serde(serialize_with = "time::serde::rfc3339::option::serialize")]
    pub revoked_at: Option<OffsetDateTime>,
}

impl TokenView {
    /// The view of a token.
    pub fn of(named: NamedToken) -> Self {
        Self {
            prefix: named.record.prefix.as_str().to_owned(),
            member: named.member,
            created_at: named.record.created_at,
            revoked_at: named.record.revoked_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;

    use dibs_domain::claim::{Claim, Holder};
    use dibs_domain::edit::PathVerdict;
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::ids::MemberId;
    use dibs_domain::paths::RepoPath;

    use super::*;

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    #[test]
    fn a_claim_names_its_holder_and_writes_times_as_rfc3339() {
        let claim = Claim {
            item: ItemId(5),
            member: MemberId(1),
            session: SessionId(9),
            host: HostName::parse("laptop").unwrap(),
            areas: BTreeSet::from([QualifiedArea::parse("api:billing").unwrap()]),
            wide: None,
            claimed_at: datetime!(2026-01-02 03:04:05 UTC),
            renewed_at: datetime!(2026-01-02 03:04:05 UTC),
            last_seen: datetime!(2026-01-02 03:05:00 UTC),
            state: dibs_domain::review::ClaimState::Working,
            branches: std::collections::BTreeMap::from([(
                RepoId::parse("api").unwrap(),
                BranchName::parse("alice/5-rates").unwrap(),
            )]),
        };
        let view = serde_json::to_value(ClaimView::of(NamedClaim {
            claim: claim.clone(),
            holder: handle("alice"),
        }))
        .unwrap();
        assert_eq!(
            view,
            json!({
                "item": 5, "holder": "alice", "session_id": 9, "host": "laptop",
                "areas": ["api:billing"],
                "wide": null,
                "claimed_at": "2026-01-02T03:04:05Z",
                "renewed_at": "2026-01-02T03:04:05Z",
                "last_seen": "2026-01-02T03:05:00Z",
                "state": "working",
                "review": null,
                "branches": {"api": "alice/5-rates"}
            })
        );
        let reviewing = Claim {
            state: dibs_domain::review::ClaimState::InReview(
                Review::new(
                    BTreeSet::from([PrRef::parse("acme/api#12").unwrap()]),
                    datetime!(2026-01-03 10:00 UTC),
                    true,
                )
                .unwrap(),
            ),
            ..claim
        };
        let view = serde_json::to_value(ClaimView::of(NamedClaim {
            claim: reviewing,
            holder: handle("alice"),
        }))
        .unwrap();
        assert_eq!(view["state"], "in_review");
        assert_eq!(
            view["review"],
            json!({"prs": ["acme/api#12"], "since": "2026-01-03T10:00:00Z", "more": true})
        );
    }

    #[test]
    fn an_event_shows_its_kind_payload_and_the_handles_it_names() {
        use dibs_app::records::MessageRecord;
        use dibs_domain::event::{Event, MessagePosted};
        use dibs_domain::ids::ProjectId;

        let event = Event {
            id: EventId(12),
            project: ProjectId(1),
            at: datetime!(2026-01-02 03:04:05 UTC),
            kind: EventKind::MessagePosted(MessagePosted {
                thread: ThreadId(4),
                message: MessageId(5),
                author: Some(MemberId(7)),
            }),
        };
        let named = NamedEvent {
            event,
            members: BTreeMap::from([(MemberId(7), handle("alice"))]),
        };
        let feed = serde_json::to_value(EventView::of(named.clone())).unwrap();
        assert_eq!(
            feed,
            json!({
                "id": 12, "at": "2026-01-02T03:04:05Z", "kind": "message_posted",
                "payload": {"thread": 4, "message": 5, "author": 7},
                "members": {"7": "alice"}
            })
        );
        let message = NamedMessage {
            record: MessageRecord {
                id: MessageId(5),
                thread: ThreadId(4),
                author: dibs_domain::message::Author::Member(MemberId(7)),
                label: SessionLabel::parse("web").unwrap(),
                body: Body::parse("line one\nline two").unwrap(),
                importance: Importance::High,
                ack_required: true,
                created_at: datetime!(2026-01-02 03:04:05 UTC),
            },
            author: dibs_app::named::AuthorName::Member(handle("alice")),
        };
        let followed = serde_json::to_value(EventView::followed(FollowedEvent {
            event: named,
            message: Some(message.clone()),
        }))
        .unwrap();
        assert_eq!(
            followed["message"],
            json!({
                "id": 5, "thread": 4, "author": "alice", "system": false, "label": "web",
                "body": "line one\nline two", "importance": "high", "ack_required": true,
                "created_at": "2026-01-02T03:04:05Z"
            })
        );
        assert!(!serde_json::to_string(&followed).unwrap().contains('\n'));

        let from_dibs = NamedMessage {
            record: MessageRecord {
                author: dibs_domain::message::Author::System,
                label: SessionLabel::parse("dibs").unwrap(),
                ..message.record
            },
            author: dibs_app::named::AuthorName::Dibs,
        };
        let shown = serde_json::to_value(MessageView::of(from_dibs)).unwrap();
        assert_eq!(
            (&shown["author"], &shown["system"], &shown["label"]),
            (&json!("dibs"), &json!(true), &json!("dibs")),
            "what dibs wrote is from dibs and from no member"
        );
    }

    #[test]
    fn a_verdict_is_tagged_and_flattened() {
        let named = NamedVerdict {
            verdict: PathVerdict {
                path: RepoPath::parse("api:src/billing/a.rs").unwrap(),
                verdict: Verdict::HeldBy {
                    member: MemberId(2),
                    item: ItemId(3),
                    area: QualifiedArea::parse("api:billing").unwrap(),
                    reason: HeldReason::AskHolder,
                },
            },
            member: Some(handle("bob")),
            item_ref: Some(ExternalRef::parse("gh:acme/api#3").unwrap()),
            touched_by: Vec::new(),
            touched_refs: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(PathVerdictView::of(named)).unwrap(),
            json!({
                "repo": "api", "path": "src/billing/a.rs", "verdict": "held_by",
                "holder": "bob", "item": 3, "item_ref": "gh:acme/api#3",
                "area": "api:billing", "reason": "ask_holder"
            })
        );
        let reviewing = NamedVerdict {
            verdict: PathVerdict {
                path: RepoPath::parse("api:src/billing/a.rs").unwrap(),
                verdict: Verdict::HeldBy {
                    member: MemberId(2),
                    item: ItemId(3),
                    area: QualifiedArea::parse("api:billing").unwrap(),
                    reason: HeldReason::InReview {
                        prs: vec![PrRef::parse("acme/api#12").unwrap()],
                    },
                },
            },
            member: Some(handle("bob")),
            item_ref: None,
            touched_by: Vec::new(),
            touched_refs: Vec::new(),
        };
        let view = serde_json::to_value(PathVerdictView::of(reviewing)).unwrap();
        assert_eq!(
            (&view["reason"], &view["prs"]),
            (&json!("in_review"), &json!(["acme/api#12"]))
        );
    }

    #[test]
    fn the_editors_of_a_shared_file_are_named_in_order() {
        let kind = HotRuleRegistry::standard().kind("warn-shared").unwrap();
        let named = NamedVerdict {
            verdict: PathVerdict {
                path: RepoPath::parse("api:Cargo.lock").unwrap(),
                verdict: Verdict::AllowedWithNote {
                    rule: kind,
                    note: None,
                    touched_by: vec![
                        Holder {
                            member: MemberId(1),
                            item: ItemId(4),
                        },
                        Holder {
                            member: MemberId(2),
                            item: ItemId(6),
                        },
                    ],
                },
            },
            member: None,
            item_ref: None,
            touched_by: vec![handle("alice"), handle("bob")],
            touched_refs: vec![Some(ExternalRef::parse("gh:acme/api#4").unwrap()), None],
        };
        assert_eq!(
            serde_json::to_value(PathVerdictView::of(named)).unwrap()["touched_by"],
            json!([
                {"holder": "alice", "item": 4, "item_ref": "gh:acme/api#4"},
                {"holder": "bob", "item": 6, "item_ref": null}
            ])
        );
    }
}
