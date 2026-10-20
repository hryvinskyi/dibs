//! The ports the guard owns. The command line implements them over the client; the guard's tests
//! implement them over plain values. Nothing behind them is reached any other way: the guard reads
//! no clock, file or network itself.

use std::path::Path;

use thiserror::Error;
use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::Note;
use dibs_domain::ids::{ItemId, MessageId};
use dibs_domain::item::{ExternalRef, Priority, Title};
use dibs_domain::message::{Importance, Subject};
use dibs_domain::paths::RelPath;
use dibs_domain::repo::RepoId;
use dibs_domain::settings::ProjectSettings;
use dibs_domain::slot::{Purpose, SlotName};

use crate::adapter::HostId;
use crate::event::SessionKey;

/// Why a port could not do what it was asked.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PortError {
    /// The server could not be reached, was too slow, failed itself, or answered something this
    /// dibs does not understand. The sentence is for the person and names no internal detail.
    #[error("{0}")]
    Unavailable(String),
    /// Anything else: a missing token, a configuration that cannot be used, a refusal. The
    /// sentence is for the person and says what to do.
    #[error("{0}")]
    Refused(String),
}

/// A value and when it was fetched from the server or learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached<T> {
    /// The value.
    pub value: T,
    /// When.
    pub at: OffsetDateTime,
}

/// The project's settings and who the member signed in is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownSettings {
    /// The member the stored token signs in as.
    pub member: Handle,
    /// The project's settings.
    pub settings: ProjectSettings,
}

/// A claim as the brief shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimLine {
    /// The item.
    pub item: ItemId,
    /// Its reference in an outside tracker, when it has one.
    pub reference: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// The member holding it.
    pub holder: Handle,
    /// The areas it declared.
    pub areas: Vec<QualifiedArea>,
}

/// An area held by a member's live claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldLine {
    /// The area.
    pub area: QualifiedArea,
    /// The member.
    pub holder: Handle,
    /// The item of the claim.
    pub item: ItemId,
}

/// An item the member can pick up now, as the brief shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadyLine {
    /// The item.
    pub item: ItemId,
    /// Its reference in an outside tracker, when it has one.
    pub reference: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// How soon: 0 to 4, lower is sooner.
    pub priority: Priority,
    /// The areas the work will probably touch.
    pub areas_hint: Vec<QualifiedArea>,
    /// The member whose claim on it went stale, when one did.
    pub stale_holder: Option<Handle>,
}

/// A live wide claim as the brief shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideLine {
    /// The item.
    pub item: ItemId,
    /// Its reference in an outside tracker, when it has one.
    pub reference: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// The member whose wide claim it is.
    pub holder: Handle,
    /// Why the change is made.
    pub reason: String,
    /// Its areas, in area order.
    pub areas: Vec<WideAreaLine>,
}

/// One area of a wide claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideAreaLine {
    /// The area.
    pub area: QualifiedArea,
    /// The other member holding it, when one does.
    pub holder: Option<Handle>,
    /// Whether that member acknowledged the announcement or granted the edits.
    pub granted: bool,
}

/// A slot as the brief shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotLine {
    /// The slot.
    pub slot: SlotName,
    /// Its hold, when it is held through this slot.
    pub hold: Option<SlotHoldLine>,
    /// The members waiting for its group, first in line first, each with the slot they asked
    /// for.
    pub waiting: Vec<(Handle, SlotName)>,
}

/// A slot's hold as the brief shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotHoldLine {
    /// The member holding it.
    pub holder: Handle,
    /// Why.
    pub purpose: Purpose,
    /// When it ends unless renewed.
    pub expires_at: OffsetDateTime,
    /// When what keeps it was last seen.
    pub last_seen: OffsetDateTime,
    /// Whether its run stopped sending heartbeats.
    pub stalled: bool,
}

/// A slot offered to the member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferLine {
    /// The slot.
    pub slot: SlotName,
    /// When the offer lapses.
    pub until: OffsetDateTime,
}

/// The project's claims and holds, and the member's ready list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusView {
    /// Live claims.
    pub live: Vec<ClaimLine>,
    /// Stale claims.
    pub stale: Vec<ClaimLine>,
    /// Areas held by live claims. A wide claim holds none of the areas it names.
    pub holds: Vec<HoldLine>,
    /// The live wide claims.
    pub wide: Vec<WideLine>,
    /// The items the member can pick up now, soonest first.
    pub ready: Vec<ReadyLine>,
    /// How many open items nobody holds wait for items that are neither done nor closed.
    pub blocked: u64,
    /// How many open items nobody holds touch an area another member holds.
    pub area_held: u64,
    /// The declared slots.
    pub slots: Vec<SlotLine>,
}

/// A message that asks the member for an acknowledgement they have not given, as the brief shows
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnackedLine {
    /// The message.
    pub message: MessageId,
    /// Who wrote it.
    pub sender: Handle,
    /// The subject of its conversation.
    pub subject: Subject,
    /// How much it asks for the member's attention.
    pub importance: Importance,
}

/// The conversations waiting for the member.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxView {
    /// How many conversations have messages the member has not read.
    pub unread: u64,
    /// The subjects of those conversations, newest activity first.
    pub subjects: Vec<Subject>,
    /// The messages that ask the member for an acknowledgement they have not given, newest first.
    pub unacked: Vec<UnackedLine>,
    /// The slots on offer to the member.
    pub offers: Vec<OfferLine>,
}

/// Another member who edited a shared file under a live claim, and the item they hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedBy {
    /// The member.
    pub holder: Handle,
    /// Their item.
    pub item: ItemId,
    /// That item's reference, when it has one.
    pub item_ref: Option<ExternalRef>,
}

/// The server's answer for one path the member is about to change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathAnswer {
    /// The member may edit.
    Allowed,
    /// The member may edit; a hot-file rule has something to say.
    Noted {
        /// The rule's kind.
        rule: String,
        /// The rule's note.
        note: Option<Note>,
        /// Other members who edited the same file under a live claim, with their items.
        touched_by: Vec<TouchedBy>,
    },
    /// The path needs a live claim first.
    NeedsClaim,
    /// Another member holds the path's area.
    HeldBy {
        /// The holder, when the server names them.
        holder: Option<Handle>,
        /// The holder's item.
        item: ItemId,
        /// That item's reference, when it has one.
        item_ref: Option<ExternalRef>,
        /// The area.
        area: QualifiedArea,
        /// The way on.
        way: HeldWay,
    },
    /// The holder granted the member edits in the area.
    Granted {
        /// The holder, when the server names them.
        by: Option<Handle>,
        /// The area.
        area: QualifiedArea,
    },
}

/// What a member can do about a path another member holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldWay {
    /// Message the holder or ask for a grant.
    AskHolder,
    /// The member's wide claim names the area: its holder's acknowledgement of the announcement
    /// grants the edit.
    AwaitingAck,
}

/// One session of one host, as the guard names it to its ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    /// The host.
    pub host: HostId,
    /// The host's id of the session.
    pub key: SessionKey,
}

/// The time.
pub trait Clock {
    /// Now.
    fn now(&self) -> OffsetDateTime;
}

/// Where an event's working directory leads.
pub trait Workspaces {
    /// The governed checkout `cwd` lies in, or `None` when no checkout governs it.
    ///
    /// # Errors
    /// [`PortError::Refused`] when the checkout's configuration cannot be used.
    fn open(&self, cwd: &Path) -> Result<Option<Box<dyn Workspace + '_>>, PortError>;
}

/// One governed checkout and the project it belongs to.
pub trait Workspace {
    /// `path` (absolute, or relative to `cwd`) relative to the checkout's root; `None` when it lies
    /// outside the checkout or is no file path.
    fn relative(&self, cwd: &Path, path: &Path) -> Option<RelPath>;

    /// The repository of `settings` this checkout is, its remote confirmed.
    ///
    /// # Errors
    /// [`PortError::Refused`] when the checkout governs no repository of the settings.
    fn repo(&self, settings: &ProjectSettings) -> Result<RepoId, PortError>;

    /// What this machine remembers about the project.
    fn cache(&self) -> &dyn LocalCache;

    /// The project's server.
    fn gateway(&self) -> &dyn CoordinationGateway;

    /// The background upkeep of the caches.
    fn upkeep(&self) -> &dyn Upkeep;
}

/// What this machine remembers about the project between hook runs. Reading never touches the
/// network; a failed write is not the caller's concern, so writes answer nothing.
pub trait LocalCache {
    /// The settings and the member, as last fetched.
    ///
    /// # Errors
    /// [`PortError::Refused`] when the cached settings use something this dibs does not know.
    fn settings(&self) -> Result<Option<Cached<KnownSettings>>, PortError>;

    /// The status as last fetched.
    fn status(&self) -> Option<Cached<StatusView>>;

    /// The inbox as last fetched.
    fn inbox(&self) -> Option<Cached<InboxView>>;

    /// The areas the member was found to hold through edit checks, each with when.
    fn noted_holds(&self) -> Vec<Cached<QualifiedArea>>;

    /// Remembers that the member holds these areas as of `at`.
    fn note_holds(&self, areas: &[QualifiedArea], at: OffsetDateTime);

    /// When the member's last heartbeat from this machine was sent.
    fn last_heartbeat(&self) -> Option<OffsetDateTime>;

    /// When a call last found the server unreachable, unless one reached it since.
    fn unreachable_since(&self) -> Option<OffsetDateTime>;

    /// Remembers that the server was unreachable at `at`.
    fn mark_unreachable(&self, at: OffsetDateTime);

    /// Forgets that the server was unreachable.
    fn clear_unreachable(&self);

    /// When the person was last warned that the server could not be reached.
    fn last_warning(&self) -> Option<OffsetDateTime>;

    /// Remembers that the person was warned at `at`.
    fn mark_warning(&self, at: OffsetDateTime);

    /// When the person was last told that this checkout governs no repository of the project.
    fn last_repo_warning(&self) -> Option<OffsetDateTime>;

    /// Remembers that the person was told so at `at`.
    fn mark_repo_warning(&self, at: OffsetDateTime);
}

/// The project's server. Every answer it fetches is also remembered, so the next read of
/// [`LocalCache`] sees it.
pub trait CoordinationGateway {
    /// The settings and the member the token signs in as.
    ///
    /// # Errors
    /// [`PortError`].
    fn settings(&self) -> Result<KnownSettings, PortError>;

    /// The project's claims and holds.
    ///
    /// # Errors
    /// [`PortError`].
    fn status(&self) -> Result<StatusView, PortError>;

    /// The conversations with messages the member has not read.
    ///
    /// # Errors
    /// [`PortError`].
    fn inbox(&self) -> Result<InboxView, PortError>;

    /// The answers for `paths` of `repo`, one per path in the order asked, recording the edits as
    /// being made by `session`.
    ///
    /// # Errors
    /// [`PortError`].
    fn edit_check(
        &self,
        session: &SessionRef,
        repo: &RepoId,
        paths: &[RelPath],
    ) -> Result<Vec<PathAnswer>, PortError>;
}

/// The upkeep the guard never waits for: heartbeats and refreshed caches.
pub trait Upkeep {
    /// Starts the upkeep in the background for `session`, unless it is already running or was
    /// started a moment ago; answers whether it was started now.
    ///
    /// # Errors
    /// [`PortError::Refused`] when it could not be started.
    fn start(&self, session: &SessionRef, now: OffsetDateTime) -> Result<bool, PortError>;
}
