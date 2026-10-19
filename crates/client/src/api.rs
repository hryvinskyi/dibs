//! The requests the client sends and the answers it reads, typed after the server's JSON. Every
//! answer is parsed into these types before anything else sees it: a value that breaks a rule of
//! its domain type fails the parse, and the call reports an answer it does not understand.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::event::EventKind;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::{HotRuleRegistry, Note};
use dibs_domain::ids::{EventId, GrantId, ItemId, MemberId, MessageId, SessionId, ThreadId};
use dibs_domain::item::{ExternalRef, ItemStatus, Title};
use dibs_domain::member::{DisplayName, ExternalLogin, Role};
use dibs_domain::message::{Body, SessionLabel, Subject};
use dibs_domain::paths::RelPath;
use dibs_domain::project::ProjectSlug;
use dibs_domain::repo::{RemoteUrl, RepoId};
use dibs_domain::session::{AgentLabel, HostName};
use dibs_domain::settings::{ProjectSettings, SettingsDocument, SettingsError};

use crate::token::Token;

/// A member of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// The handle.
    pub handle: Handle,
    /// The name shown.
    pub display_name: DisplayName,
    /// The login elsewhere.
    pub external_login: Option<ExternalLogin>,
    /// The role.
    pub role: Role,
}

/// The caller's project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRef {
    /// Its slug.
    pub slug: ProjectSlug,
}

/// A repository a checkout's configuration may name. The one repository of a project that lists
/// none has no remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRef {
    /// Its id.
    pub id: RepoId,
    /// Its normalised remote.
    pub remote: Option<RemoteUrl>,
}

/// `GET /v1/me`: the caller, their project, its repositories and its settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    /// The caller.
    pub member: Member,
    /// Their project.
    pub project: ProjectRef,
    /// The repositories a checkout may name.
    pub repos: Vec<RepoRef>,
    /// The settings in their normal form.
    pub settings: SettingsDocument,
}

impl Me {
    /// The settings, validated with the hot-file rule kinds this client knows.
    ///
    /// # Errors
    /// [`SettingsError`] when the settings use something this client does not know, such as a
    /// rule kind added to the server after this client was built.
    pub fn project_settings(&self) -> Result<ProjectSettings, SettingsError> {
        ProjectSettings::parse(&self.settings, &HotRuleRegistry::standard())
    }
}

/// A session of the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
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
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
}

/// `POST /v1/sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisterSession {
    /// The machine.
    pub host: HostName,
    /// The label, unique per member; registering a label again keeps its id.
    pub label: SessionLabel,
    /// The kind of agent.
    pub agent: AgentLabel,
    /// Whether the session can be woken while idle.
    pub push: bool,
}

/// `POST /v1/heartbeat`'s answer: the claims the heartbeat kept alive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// Their items.
    pub refreshed: Vec<ItemId>,
}

/// A work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Its local id.
    pub id: ItemId,
    /// Its reference in an outside tracker.
    #[serde(rename = "ref")]
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// Where it stands.
    pub status: ItemStatus,
    /// The repository of the project it belongs to.
    pub repo: Option<RepoId>,
    /// The repository its reference names when the project does not list it.
    pub unlisted_remote: Option<RemoteUrl>,
    /// Who created it.
    pub created_by: Handle,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The filters of `GET /v1/items`; every one given must match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ItemFilter {
    /// Only items in this state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ItemStatus>,
    /// Only items this member holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<Handle>,
    /// Only the item with this reference.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<ExternalRef>,
}

/// `GET /v1/items`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Items {
    /// The items, ordered by id.
    pub items: Vec<Item>,
}

/// `POST /v1/items`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewItem {
    /// Its reference; a local item has none.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
}

/// `POST /v1/items`'s answer: the item, and whether this call created it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedItem {
    /// The item.
    pub item: Item,
    /// `false` when the reference already named it.
    pub created: bool,
}

/// A claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
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
    /// When it was first claimed.
    #[serde(with = "time::serde::rfc3339")]
    pub claimed_at: OffsetDateTime,
    /// When it was last claimed again.
    #[serde(with = "time::serde::rfc3339")]
    pub renewed_at: OffsetDateTime,
    /// The last sign of life.
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
}

/// `GET /v1/items/{id}`: an item and its claim, live or stale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemDetail {
    /// The item.
    pub item: Item,
    /// Its claim.
    pub claim: Option<Claim>,
}

/// `POST /v1/items/{id}/claim`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimRequest {
    /// The session taking it.
    pub session_id: SessionId,
    /// The areas, as a person names them: `repo:area`, or a name one repository declares.
    pub areas: Vec<String>,
}

/// Whether a claim was new or renewed the caller's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimOutcome {
    /// A first claim.
    New,
    /// The caller already held the item.
    Renewed,
}

/// `POST /v1/items/{id}/claim`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claimed {
    /// New or renewed.
    pub outcome: ClaimOutcome,
    /// The claim.
    pub claim: Claim,
    /// Areas a renewal no longer holds.
    pub dropped_areas: BTreeSet<QualifiedArea>,
}

/// `POST /v1/items/{id}/takeover`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TakeOverRequest {
    /// The session taking it.
    pub session_id: SessionId,
    /// Why, sent to the previous holder.
    pub reason: Body,
    /// The areas.
    pub areas: Vec<String>,
}

/// `POST /v1/items/{id}/takeover`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakenOver {
    /// The new claim.
    pub claim: Claim,
    /// Who held it.
    pub previous_holder: Handle,
    /// Areas the new claim does not hold.
    pub dropped_areas: BTreeSet<QualifiedArea>,
    /// The thread that told the previous holder the reason.
    pub thread_id: ThreadId,
}

/// The answer of a release, a completion or a forced release: the claim that ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    /// The claim.
    pub claim: Claim,
}

/// An item with its claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldItem {
    /// The item.
    pub item: Item,
    /// Its claim.
    pub claim: Claim,
}

/// An area held by a member's live claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    /// The area.
    pub area: QualifiedArea,
    /// The member.
    pub holder: Handle,
    /// The item of the claim.
    pub item: ItemId,
}

/// `GET /v1/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Live claims.
    pub live: Vec<HeldItem>,
    /// Stale claims.
    pub stale: Vec<HeldItem>,
    /// Areas the live claims hold.
    pub holds: Vec<Hold>,
}

/// `POST /v1/edit-check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditCheckRequest {
    /// The caller's session.
    pub session_id: SessionId,
    /// The repository every path lies in.
    pub repo: RepoId,
    /// The paths, relative to that repository's root.
    pub paths: Vec<RelPath>,
    /// Whether the edits are being made, so what the check plans is recorded.
    pub reserve: bool,
}

/// `POST /v1/edit-check`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditCheck {
    /// One verdict per path, in the order asked.
    pub verdicts: Vec<PathVerdict>,
}

/// The verdict on one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathVerdict {
    /// The path's repository.
    pub repo: RepoId,
    /// The path inside it.
    pub path: RelPath,
    /// The verdict.
    #[serde(flatten)]
    pub verdict: Verdict,
}

/// The name of a hot-file rule's kind (`warn-shared`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleKind(pub String);

/// Another member's live claim that edited the same file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toucher {
    /// The member.
    pub holder: Handle,
    /// The claim's item.
    pub item: ItemId,
}

/// The way on from a held path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldReason {
    /// Message the holder or ask for a grant.
    AskHolder,
    /// A way this client does not know yet.
    #[serde(other)]
    Other,
}

/// A verdict, tagged by `verdict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// The member may edit.
    Allowed,
    /// The member may edit; a hot-file rule has something to say.
    AllowedWithNote {
        /// The rule's kind.
        rule: RuleKind,
        /// The rule's note.
        note: Option<Note>,
        /// Other members' live claims that edited the same file.
        touched_by: Vec<Toucher>,
    },
    /// The path needs a live claim first.
    NeedsClaim,
    /// Another member holds the path's area.
    HeldBy {
        /// The holder.
        holder: Option<Handle>,
        /// The holder's item.
        item: ItemId,
        /// The area.
        area: QualifiedArea,
        /// The way on.
        reason: HeldReason,
    },
    /// The holder granted the member edits in the area.
    Granted {
        /// The holder who granted.
        by: Option<Handle>,
        /// The area.
        area: QualifiedArea,
    },
}

impl Verdict {
    /// Whether the member may go ahead with the edit.
    pub fn permits(&self) -> bool {
        matches!(
            self,
            Self::Allowed | Self::AllowedWithNote { .. } | Self::Granted { .. }
        )
    }
}

/// `POST /v1/grants`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrantRequest {
    /// The grantee's work item.
    pub item: ItemId,
    /// The area, as a person names it.
    pub area: String,
    /// The grantee.
    pub to: Handle,
}

/// A grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
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
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// `POST /v1/grants`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Granted {
    /// The grant.
    pub grant: Grant,
    /// `false` when the same grant existed.
    pub created: bool,
}

/// `DELETE /v1/grants/{id}`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revoked {
    /// The grant withdrawn.
    pub grant: Grant,
}

/// A conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    /// Its id.
    pub id: ThreadId,
    /// Its subject.
    pub subject: Subject,
    /// The work item it is about.
    pub item: Option<ItemId>,
    /// Who started it.
    pub author: Handle,
    /// Whether it is addressed to every person in the project.
    pub to_project: bool,
    /// The members it is addressed to.
    pub to: Vec<Handle>,
    /// When it started.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Its id.
    pub id: MessageId,
    /// Its thread.
    pub thread: ThreadId,
    /// Who wrote it.
    pub author: Handle,
    /// The label of the session it was written from.
    pub label: SessionLabel,
    /// Its text.
    pub body: Body,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Whom a new thread goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipients {
    /// These members.
    Members(Vec<Handle>),
    /// Every person in the project.
    Project,
}

/// `POST /v1/threads`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartThread {
    /// The subject.
    pub subject: Subject,
    /// The recipients.
    pub to: Recipients,
    /// The work item it is about.
    pub item: Option<ItemId>,
    /// The label of the session it is written from.
    pub label: SessionLabel,
    /// The first message.
    pub body: Body,
}

/// The JSON body of [`StartThread`].
#[derive(Serialize)]
pub(crate) struct StartThreadBody<'a> {
    subject: &'a Subject,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<&'a [Handle]>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    to_project: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    item: Option<ItemId>,
    label: &'a SessionLabel,
    body: &'a Body,
}

impl<'a> StartThreadBody<'a> {
    pub(crate) fn of(start: &'a StartThread) -> Self {
        let (to, to_project) = match &start.to {
            Recipients::Members(handles) => (Some(handles.as_slice()), false),
            Recipients::Project => (None, true),
        };
        Self {
            subject: &start.subject,
            to,
            to_project,
            item: start.item,
            label: &start.label,
            body: &start.body,
        }
    }
}

/// `POST /v1/threads`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    /// The thread.
    pub thread: Thread,
    /// Its first message.
    pub message: Message,
}

/// `POST /v1/threads/{id}/messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PostMessage {
    /// The label of the session it is written from.
    pub label: SessionLabel,
    /// The text.
    pub body: Body,
}

/// `POST /v1/threads/{id}/messages`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posted {
    /// The message.
    pub message: Message,
}

/// A conversation with its counts as the caller sees them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// The thread.
    pub thread: Thread,
    /// How many messages it has.
    pub messages: u64,
    /// How many the caller has not read and did not write.
    pub unread: u64,
    /// When its newest message was written.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_message_at: Option<OffsetDateTime>,
}

/// `GET /v1/threads` and `GET /v1/inbox`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Threads {
    /// Newest activity first.
    pub threads: Vec<ThreadSummary>,
}

/// `GET /v1/threads/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDetail {
    /// The thread.
    pub thread: Thread,
    /// Its messages, oldest first.
    pub messages: Vec<Message>,
}

/// `POST /v1/threads/{id}/read`'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkedRead {
    /// How many messages were unread before.
    pub newly_read: u64,
}

/// An event of the project's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Its position in the log.
    pub id: EventId,
    /// When it happened.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// `kind` and `payload`.
    #[serde(flatten)]
    pub kind: EventKind,
    /// The handle of every member the payload names, keyed by id.
    pub members: BTreeMap<MemberId, Handle>,
    /// The message a `message_posted` event announces, on the event stream only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
}

/// `GET /v1/events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPage {
    /// Oldest first.
    pub events: Vec<Event>,
    /// The cursor for the next page; `None` while the log is empty.
    pub next: Option<EventId>,
}

/// `POST /v1/admin/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewMember {
    /// The handle.
    pub handle: Handle,
    /// The name shown.
    pub display_name: DisplayName,
    /// The login elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_login: Option<ExternalLogin>,
    /// The role.
    pub role: Role,
}

/// A member with the token just issued to them; the token is shown in this answer only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrolled {
    /// The member.
    pub member: Member,
    /// Their new token.
    pub token: Token,
}

/// A token, named by its prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenInfo {
    /// Its prefix.
    pub prefix: String,
    /// The member it authenticates.
    pub member: Handle,
    /// When it was issued.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When it was revoked.
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// `DELETE /v1/admin/tokens/{prefix}`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRevoked {
    /// The token.
    pub token: TokenInfo,
}

/// `PUT /v1/admin/settings`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsApplied {
    /// The settings in force, in their normal form.
    pub settings: SettingsDocument,
    /// Whether they differ from the ones before.
    pub changed: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_verdict_reads_its_tag_and_flattened_fields() {
        let verdict: PathVerdict = serde_json::from_value(json!({
            "repo": "api", "path": "src/billing/a.rs", "verdict": "held_by",
            "holder": "bob", "item": 3, "area": "api:billing", "reason": "ask_holder"
        }))
        .unwrap();
        assert_eq!(verdict.path.as_str(), "src/billing/a.rs");
        assert!(!verdict.verdict.permits());
        assert!(matches!(
            verdict.verdict,
            Verdict::HeldBy {
                item: ItemId(3),
                reason: HeldReason::AskHolder,
                ..
            }
        ));
        let unknown_reason: Verdict = serde_json::from_value(json!({
            "verdict": "held_by", "holder": null, "item": 3, "area": "api:billing",
            "reason": "wait_for_review"
        }))
        .unwrap();
        assert!(matches!(
            unknown_reason,
            Verdict::HeldBy {
                reason: HeldReason::Other,
                ..
            }
        ));
    }

    #[test]
    fn a_value_that_breaks_its_rule_fails_the_parse() {
        let bad_handle = serde_json::from_value::<Hold>(json!({
            "area": "api:billing", "holder": "Not A Handle", "item": 1
        }));
        assert!(bad_handle.is_err());
        let bad_path = serde_json::from_value::<PathVerdict>(json!({
            "repo": "api", "path": "../escape", "verdict": "allowed"
        }));
        assert!(bad_path.is_err());
    }

    #[test]
    fn an_event_reads_its_kind_payload_and_the_handles_it_names() {
        let event: Event = serde_json::from_value(json!({
            "id": 12, "at": "2026-01-02T03:04:05Z", "kind": "message_posted",
            "payload": {"thread": 4, "message": 5, "author": 7},
            "members": {"7": "alice"}
        }))
        .unwrap();
        assert_eq!(event.id, EventId(12));
        assert_eq!(event.members[&MemberId(7)].as_str(), "alice");
        assert!(matches!(event.kind, EventKind::MessagePosted(_)));
        assert!(event.message.is_none());
    }

    #[test]
    fn a_thread_goes_either_to_members_or_to_the_project() {
        let start = StartThread {
            subject: Subject::parse("hello").unwrap(),
            to: Recipients::Members(vec![Handle::parse("bob").unwrap()]),
            item: None,
            label: SessionLabel::parse("laptop/cli").unwrap(),
            body: Body::parse("hi").unwrap(),
        };
        assert_eq!(
            serde_json::to_value(StartThreadBody::of(&start)).unwrap(),
            json!({"subject": "hello", "to": ["bob"], "label": "laptop/cli", "body": "hi"})
        );
        let project = StartThread {
            to: Recipients::Project,
            item: Some(ItemId(3)),
            ..start
        };
        assert_eq!(
            serde_json::to_value(StartThreadBody::of(&project)).unwrap(),
            json!({"subject": "hello", "to_project": true, "item": 3, "label": "laptop/cli", "body": "hi"})
        );
    }
}
