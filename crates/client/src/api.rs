//! The requests the client sends and the answers it reads, typed after the server's JSON. Every
//! answer is parsed into these types before anything else sees it: a value that breaks a rule of
//! its domain type fails the parse, and the call reports an answer it does not understand.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::escalation::{EscalationReason, EscalationSubject};
use dibs_domain::event::EventKind;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::{HotRuleRegistry, Note};
use dibs_domain::ids::{EventId, GrantId, ItemId, MemberId, MessageId, SessionId, ThreadId};
use dibs_domain::item::{ExternalRef, ItemStatus, Priority, SourceId, SourceStatus, Title};
use dibs_domain::member::{DisplayName, ExternalLogin, Role};
use dibs_domain::message::{Body, Importance, SessionLabel, Subject};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::project::ProjectSlug;
use dibs_domain::radar::{BranchName, OverlapKind};
use dibs_domain::repo::{RemoteUrl, RepoId};
use dibs_domain::report::{Feedback, FeedbackReason, RefusalId, ReportDay, Tally};
use dibs_domain::review::PrRef;
use dibs_domain::session::{AgentLabel, HostName};
use dibs_domain::settings::{ProjectSettings, SettingsDocument, SettingsError};
use dibs_domain::slot::SlotName;

use crate::token::Token;

mod slots;

pub use slots::{
    ForceReleaseSlotRequest, HoldMode, PendingOffer, ReleaseSlotRequest, Slot, SlotAnswer,
    SlotForced, SlotHold, SlotOffer, SlotReleased, SlotTakenOver, SlotWaiter, Slots, TakeOutcome,
    TakeOverSlotRequest, TakeSlotRequest, Took,
};

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

/// `GET /v1/version`: the server's version, the oldest client it serves and the client it
/// recommends, each when set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    /// The server's own version.
    pub server: semver::Version,
    /// The oldest client the server serves.
    #[serde(default)]
    pub min_client: Option<semver::Version>,
    /// The client the server recommends.
    #[serde(default)]
    pub latest_client: Option<semver::Version>,
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

/// `POST /v1/heartbeat`'s answer: the claims the heartbeat kept alive, the days whose report it
/// carried changed what the server kept, and the days it accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// Their items.
    pub refreshed: Vec<ItemId>,
    /// The days.
    #[serde(default)]
    pub reported: Vec<ReportDay>,
    /// The days whose report the server read and keeps; the others are to be sent again.
    #[serde(default)]
    pub reports_accepted: Vec<ReportDay>,
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
    /// How soon it should be done: 0 to 4, lower is sooner.
    #[serde(default)]
    pub priority: Priority,
    /// The areas the work will probably touch.
    #[serde(default)]
    pub areas_hint: BTreeSet<QualifiedArea>,
    /// The outside tracker's own state of it.
    #[serde(default)]
    pub source_status: Option<SourceStatus>,
    /// Whether the outside tracker closed it.
    #[serde(default)]
    pub closed: bool,
    /// The importer's source that lists it.
    #[serde(default)]
    pub source: Option<SourceId>,
    /// Who created it.
    pub created_by: Handle,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// An item another answer mentions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemName {
    /// Its local id.
    pub id: ItemId,
    /// Its reference in an outside tracker.
    #[serde(rename = "ref")]
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// The tracker's page for it.
    #[serde(default)]
    pub link: Option<String>,
}

impl ItemName {
    /// How the item is named to a person: its reference, or `#<id>` for a local item.
    pub fn label(&self) -> String {
        self.external_ref
            .as_ref()
            .map_or_else(|| format!("#{}", self.id), ToString::to_string)
    }
}

/// An item of the ready list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyItem {
    /// The item.
    pub item: Item,
    /// The tracker's page for it.
    #[serde(default)]
    pub link: Option<String>,
    /// The member whose claim on it went stale.
    #[serde(default)]
    pub stale_holder: Option<Handle>,
}

/// Why an open item is not ready, tagged by `reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NotReady {
    /// A member's live claim holds it.
    Claimed {
        /// The holder.
        holder: Handle,
    },
    /// Items it waits for are neither done nor closed.
    BlockedBy {
        /// Those items.
        items: Vec<ItemName>,
    },
    /// An area of its hint is held by another member's live claim.
    AreaHeld {
        /// The area.
        area: QualifiedArea,
        /// The holder.
        holder: Handle,
        /// The holder's item.
        item: ItemName,
    },
    /// A reason this client does not know yet.
    #[serde(other)]
    Other,
}

/// An open item left out of the ready list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedItem {
    /// The item.
    pub item: Item,
    /// The tracker's page for it.
    #[serde(default)]
    pub link: Option<String>,
    /// Why it is not ready.
    pub reasons: Vec<NotReady>,
}

/// `GET /v1/ready`: the ready list and the open items left out of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyList {
    /// The items the caller can pick up, soonest first.
    pub ready: Vec<ReadyItem>,
    /// The open items that are not ready.
    pub excluded: Vec<ExcludedItem>,
}

/// `POST /v1/items/{id}/dependencies`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AddDependency {
    /// The item it waits for.
    pub blocked_by: ItemId,
}

/// The answer of adding or removing a dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyChange {
    /// The item that waits.
    pub item: Item,
    /// The item it waits for.
    pub blocked_by: Item,
    /// Whether the call changed the work graph.
    pub changed: bool,
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
    /// For a wide claim, the areas it names and holds none of, and why.
    #[serde(default)]
    pub wide: Option<WideScope>,
    /// When it was first claimed.
    #[serde(with = "time::serde::rfc3339")]
    pub claimed_at: OffsetDateTime,
    /// When it was last claimed again.
    #[serde(with = "time::serde::rfc3339")]
    pub renewed_at: OffsetDateTime,
    /// The last sign of life.
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
    /// Whether its holder works on it or it waits in review.
    #[serde(default)]
    pub state: WorkState,
    /// While it is in review, the pull requests it recorded.
    #[serde(default)]
    pub review: Option<ReviewInfo>,
    /// The branch its work is on in each repository, by repository id.
    #[serde(default)]
    pub branches: BTreeMap<RepoId, BranchName>,
    /// Whether its work is a feature or debt work.
    #[serde(default)]
    pub kind: ClaimKind,
    /// The areas it waits for another member's debt claim to hand over.
    #[serde(default)]
    pub pending: BTreeMap<QualifiedArea, PendingArea>,
}

impl Claim {
    /// Whether the claim waits in review.
    pub fn in_review(&self) -> bool {
        self.state == WorkState::InReview
    }
}

/// What a claim's work is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// A feature: it may take an area from debt work.
    #[default]
    Feature,
    /// Debt work only: it gives its areas up to a feature that needs them.
    Debt,
    /// A kind this client does not know yet.
    #[serde(other)]
    Other,
}

/// An area a claim waits for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingArea {
    /// The item of the debt claim it waits for.
    pub from: ItemId,
    /// When that claim's holder was asked to hand it over.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub asked_at: Option<OffsetDateTime>,
    /// When the unanswered request went to the arbiters.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub escalated_at: Option<OffsetDateTime>,
}

/// Where a claim's work stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// Its holder works on it.
    #[default]
    Working,
    /// Its work waits in pull requests.
    InReview,
    /// A state this client does not know yet.
    #[serde(other)]
    Other,
}

/// The review of a claim in review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewInfo {
    /// The pull requests it recorded, in order.
    pub prs: BTreeSet<PrRef>,
    /// Since when it is in review.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
    /// Whether more pull requests of the item follow these.
    pub more: bool,
}

/// `POST /v1/items/{id}/review`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewRequest {
    /// The pull request to record.
    pub pr: PrRef,
    /// Whether more pull requests of the item follow this one.
    pub more: bool,
}

/// `POST /v1/items/{id}/review/drop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DropPrRequest {
    /// The pull request recorded by mistake.
    pub pr: PrRef,
}

/// The answer to a review request: the claim in review and its item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reviewed {
    /// The claim.
    pub claim: Claim,
    /// The item.
    pub item: ItemName,
}

/// The scope of a wide claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WideScope {
    /// The areas it names.
    pub areas: BTreeSet<QualifiedArea>,
    /// Why.
    pub reason: String,
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
    /// For a wide claim, the areas it names and why.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wide: Option<WideRequest>,
    /// The kind of a new claim; the server makes it a feature when this is absent, and a renewal
    /// keeps the kind it has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ClaimKind>,
}

/// The `wide` part of a claim: the areas to announce a change over, holding none of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WideRequest {
    /// The areas, named like [`ClaimRequest::areas`].
    pub areas: Vec<String>,
    /// Why, as the holders read it: 1 to 300 characters.
    pub reason: String,
}

/// An announcement a wide claim sent to a holder of its areas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// The holder asked.
    pub to: Handle,
    /// The areas of theirs it asks about.
    pub areas: BTreeSet<QualifiedArea>,
    /// Its message, which the holder acknowledges.
    pub message_id: MessageId,
    /// Its thread.
    pub thread_id: ThreadId,
}

/// An area a claim holds that another member's live wide claim names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsideWide {
    /// The area.
    pub area: QualifiedArea,
    /// The wide claim's member.
    pub holder: Handle,
    /// The wide claim's item.
    pub item: ItemName,
    /// The announcement asking the caller about the area, when one waits for them.
    #[serde(default)]
    pub message_id: Option<MessageId>,
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
    /// What the claimed item waits for that is neither done nor closed.
    #[serde(default)]
    pub blocked_by: Vec<ItemName>,
    /// The item claimed.
    #[serde(default)]
    pub item: Option<ItemName>,
    /// For a wide claim, the announcements it sent.
    #[serde(default)]
    pub announced: Vec<Announcement>,
    /// The areas the claim holds that other members' live wide claims name.
    #[serde(default)]
    pub inside_wide: Vec<InsideWide>,
    /// The areas the claim waits for another member's debt claim to hand over.
    #[serde(default)]
    pub pending: Vec<PendingNote>,
}

/// An area a claim waits for, with the debt claim that holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingNote {
    /// The area.
    pub area: QualifiedArea,
    /// The debt claim's holder.
    pub holder: Handle,
    /// The debt claim's item.
    pub item: ItemName,
    /// Whether the holder was asked to hand it over; not while their claim is in review.
    pub asked: bool,
}

/// `POST /v1/items/{id}/claim/narrow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NarrowRequest {
    /// The areas to give up, as a person names them.
    pub areas: Vec<String>,
}

/// An area a narrowing handed over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandedOver {
    /// The area.
    pub area: QualifiedArea,
    /// The item of the claim that holds it now.
    pub to: ItemName,
    /// That claim's holder.
    pub holder: Handle,
}

/// `POST /v1/items/{id}/claim/narrow`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrowed {
    /// The claim as it stands now.
    pub claim: Claim,
    /// The item.
    pub item: ItemName,
    /// The areas it gave up.
    pub areas: BTreeSet<QualifiedArea>,
    /// The areas given up that another claim waited for, each with where it went.
    #[serde(default)]
    pub handed_over: Vec<HandedOver>,
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
    /// The new claim's kind; the server carries on the previous claim's kind when this is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ClaimKind>,
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
    /// The item taken over.
    #[serde(default)]
    pub item: Option<ItemName>,
    /// The announcements the carried-on wide scope sent, in the caller's name.
    #[serde(default)]
    pub announced: Vec<Announcement>,
}

/// The answer of a release, a completion or a forced release: the claim that ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    /// The claim.
    pub claim: Claim,
    /// The item it was on.
    #[serde(default)]
    pub item: Option<ItemName>,
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
    /// The live wide claims, with where each of their areas stands.
    #[serde(default)]
    pub wide: Vec<WideClaim>,
    /// The caller's ready list.
    #[serde(default)]
    pub ready: ReadyList,
    /// Every declared slot with its group's hold, queue and offer.
    #[serde(default)]
    pub slots: Vec<Slot>,
    /// The escalations the caller may read that no arbiter has acknowledged yet, newest first.
    #[serde(default)]
    pub escalations: Vec<OpenEscalation>,
    /// Since when the project's integrations have not called, once that counts as silent.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub integration_silent_since: Option<OffsetDateTime>,
    /// The overlaps between live claims and the moved bases under them.
    #[serde(default)]
    pub overlaps: Vec<OverlapInfo>,
}

/// An overlap the radar found between live claims, or a base a merge moved under one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlapInfo {
    /// Its kind.
    pub kind: OverlapKind,
    /// The items it names, each with its holder.
    pub items: Vec<OverlapParty>,
    /// The merged pull request of a moved base.
    #[serde(default)]
    pub pr: Option<PrRef>,
    /// The paths it is on.
    pub paths: BTreeSet<RepoPath>,
}

/// One item an overlap names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlapParty {
    /// The item's local id.
    pub id: ItemId,
    /// Its reference in an outside tracker.
    #[serde(rename = "ref", default)]
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// The holder of its claim.
    pub holder: Handle,
}

/// An escalation that waits for an arbiter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenEscalation {
    /// What it is about.
    pub subject: EscalationSubject,
    /// How the subject reads.
    pub about: String,
    /// Who escalated.
    pub by: Handle,
    /// The arbiters it waits for.
    pub arbiters: Vec<Handle>,
    /// Its conversation.
    pub thread_id: ThreadId,
    /// Its message, which an arbiter acknowledges.
    pub message_id: MessageId,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// What `POST /v1/feedback` is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FeedbackAbout {
    /// A refusal the agent found wrong.
    FalseRefusal {
        /// The id it was shown with.
        refusal: RefusalId,
        /// Its kind, when this machine still remembers it.
        #[serde(skip_serializing_if = "Option::is_none")]
        tally: Option<Tally>,
    },
    /// A rebase conflict the agent resolved by hand.
    Conflict {
        /// The repository.
        repo: RepoId,
        /// The file, relative to the repository's root.
        path: RelPath,
    },
}

/// `POST /v1/feedback`'s body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedbackRequest {
    /// What it is about.
    #[serde(flatten)]
    pub about: FeedbackAbout,
    /// Why.
    pub reason: FeedbackReason,
}

/// `POST /v1/feedback`'s answer: the feedback as the server kept it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackGiven {
    /// It.
    pub feedback: Feedback,
}

/// `POST /v1/escalations`'s body: one subject and the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EscalationRequest {
    /// A work item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// A slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<SlotName>,
    /// A conversation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<ThreadId>,
    /// Why.
    pub reason: EscalationReason,
}

impl EscalationRequest {
    /// The request about `subject`.
    pub fn about(subject: EscalationSubject, reason: EscalationReason) -> Self {
        let mut request = Self {
            item: None,
            slot: None,
            thread: None,
            reason,
        };
        match subject {
            EscalationSubject::Item(item) => request.item = Some(item),
            EscalationSubject::Slot(slot) => request.slot = Some(slot),
            EscalationSubject::Thread(thread) => request.thread = Some(thread),
        }
        request
    }
}

/// An escalation sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationSent {
    /// Its conversation.
    pub thread_id: ThreadId,
    /// Its message.
    pub message_id: MessageId,
    /// The arbiters it reached.
    pub arbiters: Vec<Handle>,
    /// Everyone it reached.
    pub reached: Vec<Handle>,
}

/// `POST /v1/escalations`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalated {
    /// The escalation.
    pub escalation: EscalationSent,
}

/// What removing a member took from them among the slots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeftSlots {
    /// The slots they held.
    pub held: Vec<SlotName>,
    /// The slots they waited for.
    pub waited: Vec<SlotName>,
    /// The slots on offer to them.
    pub offered: Vec<SlotName>,
}

/// `DELETE /v1/admin/members/{handle}`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRemoved {
    /// The member.
    pub member: Member,
    /// When they were removed.
    #[serde(with = "time::serde::rfc3339::option")]
    pub removed_at: Option<OffsetDateTime>,
    /// The items whose claims were released.
    pub released: Vec<ItemName>,
    /// What they had among the slots.
    pub slots: LeftSlots,
    /// How many tokens were revoked.
    pub tokens_revoked: u64,
    /// How many browser sessions ended.
    pub ui_sessions_ended: u64,
}

/// `DELETE /v1/admin/threads/{id}`'s answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDeleted {
    /// The conversation.
    pub thread_id: ThreadId,
    /// How many messages went with it.
    pub messages: u64,
}

/// A live wide claim, its item, and where each area it names stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WideClaim {
    /// The item.
    pub item: Item,
    /// The claim.
    pub claim: Claim,
    /// Its areas, in area order.
    pub areas: Vec<WideArea>,
}

/// Where one area of a wide claim stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WideArea {
    /// The area.
    pub area: QualifiedArea,
    /// `free`, `granted` or `waiting`.
    pub state: WideAreaState,
    /// The other member holding it, when one does.
    #[serde(default)]
    pub holder: Option<Handle>,
    /// That member's item.
    #[serde(default)]
    pub item: Option<ItemId>,
}

/// Where an area of a wide claim stands for its member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WideAreaState {
    /// Nobody else holds it; the wide claim's member may edit there.
    Free,
    /// Its holder acknowledged the announcement or granted the edits.
    Granted,
    /// Its holder has not acknowledged yet.
    Waiting,
    /// A state this client does not know yet.
    #[serde(other)]
    Other,
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
    /// The branch the checkout is on; none on a detached head.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<BranchName>,
}

/// The checkout a request comes from and the branch it is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OnBranch {
    /// The repository.
    pub repo: RepoId,
    /// The branch.
    pub branch: BranchName,
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
    /// That item's reference, when it has one.
    #[serde(default)]
    pub item_ref: Option<ExternalRef>,
}

impl Toucher {
    /// How the item is named to a person: its reference, or `#<id>` for a local item.
    pub fn item_label(&self) -> String {
        self.item_ref
            .as_ref()
            .map_or_else(|| format!("#{}", self.item), ToString::to_string)
    }
}

/// The way on from a held path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldReason {
    /// The holder's claim is in review: the area is free once its pull requests merge.
    InReview,
    /// Message the holder or ask for a grant.
    AskHolder,
    /// The caller's wide claim names the area: wait for the holder to acknowledge its
    /// announcement.
    AwaitingAck,
    /// The caller's claim waits for the holder's debt claim to hand the area over.
    AwaitingHandover,
    /// The holder's claim is debt work: declaring the area on the caller's feature claim asks for
    /// it.
    AskHandover,
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
        /// The holder's item's reference, when it has one.
        #[serde(default)]
        item_ref: Option<ExternalRef>,
        /// The area.
        area: QualifiedArea,
        /// The way on.
        reason: HeldReason,
        /// For a holder in review, the pull requests their claim waits for.
        #[serde(default)]
        prs: Vec<PrRef>,
        /// For a debt holder, the item of the caller's feature claim to declare the area on.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claim: Option<ItemId>,
        /// Another member's claim that already waits for the holder to hand the area over.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        waiting: Option<Toucher>,
        /// When the caller's request goes to the arbiters if the area is not handed over by then.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "time::serde::rfc3339::option"
        )]
        escalates_at: Option<OffsetDateTime>,
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
    /// The grantee's item.
    #[serde(default)]
    pub item: Option<ItemName>,
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
    /// Who started it: a member's handle, or `dibs`.
    pub author: Handle,
    /// Whether dibs started it, in the project's name.
    #[serde(default)]
    pub system: bool,
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
    /// Who wrote it: a member's handle, or `dibs`.
    pub author: Handle,
    /// Whether dibs wrote it, in the project's name.
    #[serde(default)]
    pub system: bool,
    /// The label of the session it was written from.
    pub label: SessionLabel,
    /// Its text.
    pub body: Body,
    /// How much it asks for its recipients' attention.
    #[serde(default)]
    pub importance: Importance,
    /// Whether each recipient is asked to acknowledge it.
    #[serde(default)]
    pub ack_required: bool,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// In a thread read, for a message that asks to be acknowledged: each recipient and when they
    /// acknowledged it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acks: Option<Vec<RecipientAck>>,
}

/// One recipient of a message that asks to be acknowledged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientAck {
    /// The recipient.
    pub member: Handle,
    /// When they acknowledged it; `None` while they have not.
    #[serde(with = "time::serde::rfc3339::option")]
    pub acked_at: Option<OffsetDateTime>,
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
    /// How much the first message asks for its recipients' attention.
    pub importance: Importance,
    /// Whether each recipient is asked to acknowledge the first message.
    pub ack_required: bool,
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
    importance: Importance,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    ack_required: bool,
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
            importance: start.importance,
            ack_required: start.ack_required,
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
    /// How much it asks for its recipients' attention.
    pub importance: Importance,
    /// Whether each recipient is asked to acknowledge it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ack_required: bool,
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
    /// The highest importance among its messages.
    #[serde(default)]
    pub importance: Importance,
    /// How many of its messages ask the caller for an acknowledgement they have not given.
    #[serde(default)]
    pub awaiting_ack: u64,
}

/// `GET /v1/threads`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Threads {
    /// Newest activity first.
    pub threads: Vec<ThreadSummary>,
}

/// What `GET /v1/inbox` is asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct InboxRequest {
    /// Only the threads with unread messages.
    pub unread: bool,
    /// The messages awaiting the caller's acknowledgement too.
    pub unacked: bool,
    /// The messages still to be delivered to the caller's agent too.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deliverable: bool,
}

/// A message still to be delivered to the caller's agent, with its thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deliverable {
    /// The thread.
    pub thread: Thread,
    /// The message.
    pub message: Message,
    /// Whether it awaits the caller's acknowledgement.
    #[serde(default)]
    pub awaiting_ack: bool,
    /// Whether acknowledging it grants something, as a wide claim's announcement grants edits.
    #[serde(default)]
    pub ack_grants: bool,
}

/// A delivery claimed for one of the caller's sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRecord {
    /// What: a message's id, or `offer:<slot>:<deadline>`.
    pub subject: String,
    /// The session that delivers it.
    pub session_id: SessionId,
    /// When it was claimed.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// The answer of `POST /v1/messages/{id}/deliver` and `POST /v1/slots/{name}/offer/deliver`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivered {
    /// The delivery.
    pub delivery: DeliveryRecord,
}

/// A message awaiting the caller's acknowledgement, with its thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unacked {
    /// The thread.
    pub thread: Thread,
    /// The message.
    pub message: Message,
}

/// `GET /v1/inbox`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbox {
    /// Newest activity first.
    pub threads: Vec<ThreadSummary>,
    /// The messages awaiting the caller's acknowledgement, newest first; empty unless asked for.
    #[serde(default)]
    pub unacked: Vec<Unacked>,
    /// The slots on offer to the caller.
    #[serde(default)]
    pub offers: Vec<PendingOffer>,
    /// The messages still to be delivered to the caller's agent, newest first; empty unless asked
    /// for.
    #[serde(default)]
    pub deliverable: Vec<Deliverable>,
}

/// An acknowledgement recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckRecord {
    /// The message.
    pub message: MessageId,
    /// Its thread.
    pub thread: ThreadId,
    /// The recipient who acknowledged it.
    pub member: Handle,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// `POST /v1/messages/{id}/ack`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acked {
    /// The acknowledgement that stands: the new one, or the caller's first.
    pub ack: AckRecord,
    /// `false` when the caller had acknowledged the message before.
    pub created: bool,
    /// The grants it made: for a wide claim's announcement, one per area it asked about that the
    /// caller still holds.
    #[serde(default)]
    pub granted: Vec<Grant>,
    /// Whether the message announced a wide claim that has ended, so the acknowledgement granted
    /// nothing.
    #[serde(default)]
    pub announcement_ended: bool,
}

/// One recipient of a message the caller sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientStatus {
    /// The recipient.
    pub member: Handle,
    /// When they read it.
    #[serde(with = "time::serde::rfc3339::option")]
    pub read_at: Option<OffsetDateTime>,
    /// When they acknowledged it.
    #[serde(with = "time::serde::rfc3339::option")]
    pub acked_at: Option<OffsetDateTime>,
}

/// `GET /v1/messages/{id}/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageStatus {
    /// The thread.
    pub thread: Thread,
    /// The message.
    pub message: Message,
    /// Its recipients, ordered by handle.
    pub recipients: Vec<RecipientStatus>,
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
    /// What in them works otherwise than the admin may expect, one sentence each.
    #[serde(default)]
    pub warnings: Vec<String>,
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
            importance: Importance::Normal,
            ack_required: false,
        };
        assert_eq!(
            serde_json::to_value(StartThreadBody::of(&start)).unwrap(),
            json!({"subject": "hello", "to": ["bob"], "label": "laptop/cli", "body": "hi",
                   "importance": "normal"})
        );
        let project = StartThread {
            to: Recipients::Project,
            item: Some(ItemId(3)),
            importance: Importance::Urgent,
            ack_required: true,
            ..start
        };
        assert_eq!(
            serde_json::to_value(StartThreadBody::of(&project)).unwrap(),
            json!({"subject": "hello", "to_project": true, "item": 3, "label": "laptop/cli",
                   "body": "hi", "importance": "urgent", "ack_required": true})
        );
    }
}
