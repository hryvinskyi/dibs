//! The ports the guard owns. The command line implements them over the client; the guard's tests
//! implement them over plain values. Nothing behind them is reached any other way: the guard reads
//! no clock, file or network itself.

use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;
use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::delivery::DeliverySubject;
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::Note;
use dibs_domain::ids::{ItemId, MessageId, SessionId, ThreadId};
use dibs_domain::item::{ExternalRef, Priority, Title};
use dibs_domain::message::{Importance, Subject};
use dibs_domain::paths::RelPath;
use dibs_domain::push::GitFacts;
use dibs_domain::radar::BranchName;
use dibs_domain::repo::RepoId;
use dibs_domain::report::{RefusalId, ReportDay, Tally};
use dibs_domain::review::PrRef;
use dibs_domain::session::HostName;
use dibs_domain::settings::ProjectSettings;
use dibs_domain::slot::{Liveness, Purpose, SlotName};
use dibs_domain::worktree::{AgentKey, WorktreeName};

use crate::adapter::{HostId, OwnWorktrees};
use crate::event::{SessionKey, WorktreeLabel};
use crate::upkeep::Due;

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

/// A release of dibs newer than the one running, which the project's server recommends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewerRelease {
    /// The version running.
    pub installed: String,
    /// The version recommended.
    pub latest: String,
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
    /// While it is in review, the pull requests it waits for; empty while it is worked on.
    pub prs: Vec<PrRef>,
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
    /// Since when the project's integrations have not called, once that counts as silent.
    pub integration_silent_since: Option<OffsetDateTime>,
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
    /// The messages the server still has to deliver to one of the member's sessions, newest
    /// first.
    pub deliverable: Vec<PendingMessage>,
    /// The deliveries this machine claimed, for any of its sessions, won or lost.
    pub claims: Vec<ClaimedDelivery>,
    /// The grants asked of the member that wait for their person's decision, newest first.
    pub grant_asks: Vec<GrantAskLine>,
}

/// A grant asked of the member that waits for their decision, as the brief shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantAskLine {
    /// The conversation that asks.
    pub thread: ThreadId,
    /// Who asks, and would be the grantee.
    pub asker: Handle,
    /// The asker's item, the grant's item.
    pub item: ItemId,
    /// Its reference in an outside tracker, when it has one.
    pub reference: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// The area.
    pub area: QualifiedArea,
    /// The item of the member's claim that holds the area.
    pub holder_item: ItemId,
    /// That item's reference in an outside tracker, when it has one.
    pub holder_reference: Option<ExternalRef>,
}

/// A message that should reach the member's agent while it works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMessage {
    /// The message.
    pub message: MessageId,
    /// Its thread.
    pub thread: ThreadId,
    /// The work item its thread is about, when it is about one.
    pub item: Option<ItemId>,
    /// Who wrote it: a member's handle, or `dibs`.
    pub sender: Handle,
    /// Whether dibs wrote it, in the project's name; it may quote a member's words.
    pub from_dibs: bool,
    /// The subject of its thread.
    pub subject: Subject,
    /// How much it asks for the member's attention.
    pub importance: Importance,
    /// Whether it awaits the member's acknowledgement.
    pub awaiting_ack: bool,
    /// Whether acknowledging it grants something, as the announcement of a wide claim grants
    /// edits.
    pub ack_grants: bool,
    /// Its text, whole.
    pub body: String,
}

/// What a delivery puts in front of the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// A message.
    Message(PendingMessage),
    /// A slot offered to the member.
    Offer(OfferLine),
}

impl Pending {
    /// What is delivered, as the server names it.
    pub fn subject(&self) -> DeliverySubject {
        match self {
            Self::Message(message) => DeliverySubject::Message(message.message),
            Self::Offer(offer) => DeliverySubject::Offer {
                slot: offer.slot.clone(),
                until: offer.until,
            },
        }
    }
}

/// A delivery this machine claimed from the server for one session of a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedDelivery {
    /// What it delivers.
    pub pending: Pending,
    /// The host of the session it was claimed for.
    pub host: String,
    /// That host's id of the session.
    pub session: SessionKey,
    /// Whether the session won it; otherwise another session of the member had it first.
    pub won: bool,
    /// When it was claimed.
    pub at: OffsetDateTime,
}

/// A slot's group as the server stands it now: its hold, its queue and its offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotState {
    /// The slot asked about.
    pub slot: SlotName,
    /// The hold of its group, when one stands: of this slot or of another of the group.
    pub hold: Option<SlotHoldState>,
    /// Its group's queue, first in line first.
    pub queue: Vec<SlotWaiterState>,
    /// Its group's offer, when one stands.
    pub offer: Option<SlotOfferState>,
}

/// A slot's hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotHoldState {
    /// The slot held.
    pub slot: SlotName,
    /// The member holding it.
    pub holder: Handle,
    /// Why.
    pub purpose: Purpose,
    /// The session that took or last renewed it.
    pub session: SessionId,
    /// That session's machine.
    pub host: HostName,
    /// The work item, when one was named.
    pub item: Option<ItemId>,
    /// How it stays alive.
    pub liveness: Liveness,
    /// When it was taken.
    pub taken_at: OffsetDateTime,
    /// When it ends unless renewed.
    pub expires_at: OffsetDateTime,
    /// The last sign of life of what keeps it.
    pub last_seen: OffsetDateTime,
    /// Whether its run stopped sending heartbeats.
    pub stalled: bool,
}

/// A place in a slot group's queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotWaiterState {
    /// The member waiting.
    pub member: Handle,
    /// The slot they asked for.
    pub slot: SlotName,
    /// Why they want it.
    pub purpose: Purpose,
    /// When they joined the queue.
    pub since: OffsetDateTime,
    /// Their last sign of life.
    pub last_seen: OffsetDateTime,
}

/// A slot group's offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotOfferState {
    /// The member it is offered to.
    pub member: Handle,
    /// The slot they asked for.
    pub slot: SlotName,
    /// Why they want it.
    pub purpose: Purpose,
    /// When it lapses.
    pub until: OffsetDateTime,
}

/// What one session was shown between its tool calls, and pushed to it while it waited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionLedger {
    /// When something was last put in front of its agent.
    pub last_injection: Option<OffsetDateTime>,
    /// What was put there between tool calls, newest last, as the server names it.
    pub shown: Vec<String>,
    /// What was pushed while the session waited and is not yet known to have reached its agent.
    pub pushed: Vec<PushedDelivery>,
    /// The pushes that woke the session, newest last, within the window the push budget counts.
    pub wakes: Vec<Wake>,
}

/// A delivery pushed to a session while it waited, which the session's hooks show once if nothing
/// shows it was seen by `due`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedDelivery {
    /// What was pushed.
    pub pending: Pending,
    /// Whether the push was written to the host; one that was not is the hooks' to show at once,
    /// whoever is there.
    pub written: bool,
    /// When the hooks show it unless it was seen.
    pub due: OffsetDateTime,
}

/// A push that woke a session, by whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wake {
    /// Who wrote what was pushed: a member's handle, or `dibs`.
    pub sender: Handle,
    /// When.
    pub at: OffsetDateTime,
}

/// What shows that a pushed delivery reached the agent or its person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// The agent read or marked read this conversation.
    Thread(ThreadId),
    /// The agent acknowledged this message.
    Message(MessageId),
    /// The agent took, left or looked at this slot.
    Slot(SlotName),
    /// The person submitted a prompt: they are there.
    Person,
}

/// Whether a session's agent is at work or waits for its person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// It works: a prompt was submitted or a tool ran.
    Working,
    /// It finished its turn and waits for its person.
    Waiting,
}

/// When a session's agent was last at work and when it last began to wait, as its hooks stamped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionActivity {
    /// The last prompt or tool.
    pub working: Option<OffsetDateTime>,
    /// The last end of a turn.
    pub waiting: Option<OffsetDateTime>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeldWay {
    /// The holder's claim is in review: the area is free once its pull requests merge.
    InReview {
        /// The pull requests it waits for.
        prs: Vec<PrRef>,
    },
    /// Message the holder or ask for a grant.
    AskHolder,
    /// The member's wide claim names the area: its holder's acknowledgement of the announcement
    /// grants the edit.
    AwaitingAck,
    /// The member's feature claim waits for the holder's debt claim to hand the area over.
    AwaitingHandover {
        /// When the request goes to the arbiters if the area is not handed over by then, when the
        /// server says.
        escalates_at: Option<OffsetDateTime>,
    },
    /// Another member's claim already waits for the holder to hand the area over: message the
    /// holder.
    WaitedFor {
        /// The member whose claim waits.
        waiter: Handle,
        /// That claim's item.
        claim: ItemId,
        /// That item's reference, when it has one.
        claim_ref: Option<ExternalRef>,
    },
    /// The holder's claim is debt work: declaring the area on the member's feature claim asks the
    /// holder to hand it over.
    AskHandover {
        /// The item of the member's feature claim, as the command line takes it.
        claim: ItemId,
    },
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

    /// Git, for the worktrees a host would make and remove itself, in any repository, governed or
    /// not.
    fn git(&self) -> &dyn Git;
}

/// What became of a worktree the host would have removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// It was removed.
    Removed,
    /// Nothing is there to remove.
    Absent,
    /// It was left in place; the sentence says why (it holds changes, it is no worktree).
    Kept(String),
}

/// Git, as the guard uses it to stand in for a host's own worktrees.
pub trait Git {
    /// Makes the worktree the host would make for `label` in the repository `cwd` lies in, as
    /// `own` lays it out, on a new branch from the commit checked out there; answers its
    /// directory. A worktree already made there for the label is answered as it is.
    ///
    /// # Errors
    /// The sentence saying why no worktree could be made.
    fn make_worktree(
        &self,
        cwd: &Path,
        own: &OwnWorktrees,
        label: &WorktreeLabel,
    ) -> Result<PathBuf, String>;

    /// Removes the worktree at `path` as git removes one: never by force, so a worktree that holds
    /// changes is kept.
    fn remove_worktree(&self, path: &Path) -> Removal;
}

/// The holder of a pool worktree: a session of a host, or one agent of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeHolder {
    /// The session.
    pub session: SessionRef,
    /// The agent; `None` for the session's main agent.
    pub agent: Option<AgentKey>,
}

/// What a take of a pool worktree came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeTake {
    /// The holder holds this worktree, ready to work in.
    Took {
        /// The worktree.
        worktree: WorktreeName,
        /// Its directory, absolute.
        path: PathBuf,
        /// What the take did on the way that the person should know: work rescued onto a branch, a
        /// worktree set aside. Sentences.
        notes: Vec<String>,
    },
    /// Every worktree of the pool stayed held until the deadline. The sentence names the holders.
    Busy(String),
    /// No prepared worktree was free while the pool still has worktrees nobody holds that are not
    /// prepared yet, and a preparation does not fit in the deadline. The sentence says which are
    /// being prepared.
    Preparing(String),
    /// The pool could not serve a worktree at all: the server is unreachable, too old or refused,
    /// or no worktree of the pool could be made ready. The sentence says why.
    CannotServe(String),
}

/// A hold of a pool worktree this machine keeps a record of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    /// The worktree.
    pub worktree: WorktreeName,
    /// Whether the holder is the session asked about, or one of its agents.
    pub this_session: bool,
    /// The holder's agent; `None` for a session itself.
    pub agent: Option<AgentKey>,
    /// Why it was taken, when the record says.
    pub purpose: Option<Purpose>,
    /// When it was taken.
    pub since: OffsetDateTime,
}

/// What taking a worktree again by its name came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retaken {
    /// The holder holds it now.
    Took,
    /// Another holder has it.
    Held {
        /// That holder's agent; `None` for a session.
        agent: Option<AgentKey>,
        /// Why they took it.
        purpose: Option<Purpose>,
    },
}

/// The pool of worktrees the checkout of a workspace lends, or belongs to.
pub trait WorktreePool {
    /// Takes a worktree of the pool for `holder` and makes it ready, finishing within `deadline`:
    /// the holder's own worktree again when it holds one already, untouched.
    fn take(&self, holder: &WorktreeHolder, deadline: Duration) -> WorktreeTake;

    /// Takes the worktree `name` again for `holder`, as it is: nothing is rescued or reset, as the
    /// holder is already at work in it.
    ///
    /// # Errors
    /// [`PortError`] when the server gave no decision.
    fn retake(&self, name: &WorktreeName, holder: &WorktreeHolder) -> Result<Retaken, PortError>;

    /// Releases the hold of `name` this machine's record names, for that holder only, and forgets
    /// the record. Nothing is released when no record is kept.
    ///
    /// # Errors
    /// [`PortError`] when the server gave no decision.
    fn release(&self, name: &WorktreeName) -> Result<(), PortError>;

    /// Ends `session` on the server, which frees every hold of it and of its agents, and forgets
    /// this machine's records of them.
    ///
    /// # Errors
    /// [`PortError`] when the server gave no decision.
    fn end_session(&self, session: &SessionRef) -> Result<(), PortError>;

    /// Every hold of the pool this machine keeps a record of, each saying whether `session` or one
    /// of its agents holds it. Read from this machine alone.
    fn records(&self, session: &SessionRef) -> Vec<WorktreeRecord>;

    /// The newest branch the work found in `name` was rescued onto, when there is one.
    fn rescued(&self, name: &WorktreeName) -> Option<String>;

    /// Leaves a sign of life of the holder working in `name` at `at`, on this machine only. A sign
    /// that cannot be left is lost.
    fn touch(&self, name: &WorktreeName, at: OffsetDateTime);

    /// The worktree of the pool `path` (absolute, or relative to `cwd`) lies in, when it lies in
    /// one.
    fn worktree_at(&self, cwd: &Path, path: &Path) -> Option<WorktreeName>;
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

    /// The checkout's git settings, as a push run from `cwd` reads them: its current branch and
    /// every setting in force. `None` when git cannot tell. Asked only about a line that pushes,
    /// in a project that protects a branch.
    fn git_facts(&self, cwd: &Path) -> Option<GitFacts>;

    /// Whether `dir`, a `git -C` directory relative to `cwd`, lies in this checkout.
    fn contains_dir(&self, cwd: &Path, dir: &str) -> bool;

    /// The branch the checkout is on, read without starting git; `None` on a detached head or when
    /// it cannot be told. Asked only before an edit check goes to the server.
    fn branch(&self) -> Option<BranchName>;

    /// What this machine remembers about the project.
    fn cache(&self) -> &dyn LocalCache;

    /// The project's server.
    fn gateway(&self) -> &dyn CoordinationGateway;

    /// The background upkeep of the caches.
    fn upkeep(&self) -> &dyn Upkeep;

    /// What each session was shown.
    fn ledger(&self) -> &dyn DeliveryLedger;

    /// What this machine counts for the project's reports.
    fn tallies(&self) -> &dyn Tallies;

    /// The pool of worktrees the checkout lends, or, when it is one of a pool's worktrees, the
    /// pool it belongs to; `None` when it has none.
    fn worktree_pool(&self) -> Option<&dyn WorktreePool>;
}

/// What this machine counts for the project's reports: kinds and counts only, never a path, a
/// command line or a work item. A count that cannot be written is lost, which is not the caller's
/// concern.
pub trait Tallies {
    /// Counts one refusal of `kinds` (each kind once) on `day`, and answers the id it is shown
    /// with; `None` when it could not be counted, or no id was free for it.
    fn refused(&self, day: ReportDay, kinds: &[Tally]) -> Option<RefusalId>;

    /// Counts one answer given without the server on `day`.
    fn unreachable(&self, day: ReportDay);

    /// Counts `session` as one that ran with the guard switched off, on `day`, unless it was
    /// counted before: a session is counted once, on the day it was first seen switched off.
    fn switched_off(&self, day: ReportDay, session: &SessionRef);
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

    /// The state of every declared slot, as the status last fetched holds it.
    fn slot_states(&self) -> Option<Cached<Vec<SlotState>>>;

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

    /// When the server was last asked which client it recommends.
    fn versions_checked_at(&self) -> Option<OffsetDateTime>;

    /// The release the server last recommended, when it is newer than the one running.
    fn newer_release(&self) -> Option<NewerRelease>;

    /// When the newest sign of life was left in a pool worktree of the project on this machine.
    fn newest_worktree_beat(&self) -> Option<OffsetDateTime>;

    /// When the signs of life left in pool worktrees were last sent to the server.
    fn worktree_beats_sent_at(&self) -> Option<OffsetDateTime>;
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

    /// Where each of these slots stands now, in the order asked, read fresh from the server: a
    /// state from a cache could let two members act at once.
    ///
    /// # Errors
    /// [`PortError`].
    fn slots(&self, names: &[SlotName]) -> Result<Vec<SlotState>, PortError>;

    /// The answers for `paths` of `repo`, one per path in the order asked, recording the edits as
    /// being made by `session` in a checkout on `branch`, when it is on one.
    ///
    /// # Errors
    /// [`PortError`].
    fn edit_check(
        &self,
        session: &SessionRef,
        repo: &RepoId,
        branch: Option<&BranchName>,
        paths: &[RelPath],
    ) -> Result<Vec<PathAnswer>, PortError>;
}

/// What each session of this machine was shown. Reading and writing one session's ledger happens
/// under that session's lock, so two hooks of one session running at once never both show the
/// same thing.
pub trait DeliveryLedger {
    /// The session's ledger, its lock taken until the answer is dropped; `None` when another
    /// process holds the lock or it cannot be taken, in which case nothing is shown.
    fn open(&self, session: &SessionRef) -> Option<Box<dyn LedgerEntry + '_>>;

    /// Stamps that the session is at work or waits, at `at`, without waiting for any lock or for
    /// the disk; a stamp that cannot be written is left out.
    fn stamp(&self, session: &SessionRef, activity: Activity, at: OffsetDateTime);

    /// The session's last stamps; none that cannot be read.
    fn activity(&self, session: &SessionRef) -> SessionActivity;
}

/// One session's ledger, locked.
pub trait LedgerEntry {
    /// What was shown so far; empty when nothing was or the file cannot be read.
    fn read(&self) -> SessionLedger;

    /// Replaces it in one step; answers whether it was written.
    fn write(&self, ledger: &SessionLedger) -> bool;
}

/// The upkeep the guard never waits for: heartbeats, refreshed caches and, at most once a day, a
/// newer client.
pub trait Upkeep {
    /// Starts the upkeep of what is `due` in the background for `session`, unless it is already
    /// running or was started a moment ago; answers whether it was started now.
    ///
    /// # Errors
    /// [`PortError::Refused`] when it could not be started.
    fn start(&self, session: &SessionRef, now: OffsetDateTime, due: Due)
    -> Result<bool, PortError>;
}
