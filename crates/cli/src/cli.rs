//! The command line as clap reads it.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

use dibs_client::ClientOptions;
use dibs_domain::escalation::EscalationReason;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{MessageId, ThreadId};
use dibs_domain::item::{ExternalRef, ItemKey, ItemStatus, Title};
use dibs_domain::member::{DisplayName, ExternalLogin, Role};
use dibs_domain::message::{Importance, Subject};
use dibs_domain::repo::RepoId;
use dibs_domain::report::RefusalId;
use dibs_domain::review::PrRef;
use dibs_domain::settings::parse_duration;
use dibs_domain::slot::{Purpose, SlotName};

/// Claim work, check edits and talk to the people and agents working on the same repositories.
#[derive(Debug, Parser)]
#[command(name = "dibs", version)]
pub struct Cli {
    /// Print every answer as JSON on standard output.
    #[arg(long, global = true)]
    pub json: bool,
    /// What to do.
    #[command(subcommand)]
    pub command: Invocation,
}

/// Every command: the ones that run on their own, and the ones that talk to the server.
#[derive(Debug, Subcommand)]
pub enum Invocation {
    /// Answer an agent host's hook: read its input on standard input, print its reply. Always
    /// succeeds; a problem is a line in the reply, never a refusal.
    Hook(HookArgs),
    /// Show the agent hosts this dibs supports, what each can carry, and what this machine has
    /// for them.
    Doctor,
    /// The commands that talk to the server.
    #[command(flatten)]
    Service(Command),
}

/// `dibs hook`.
#[derive(Debug, Args)]
pub struct HookArgs {
    /// The agent host (`neutral`, …).
    pub host: String,
    /// The host's event, as its hook configuration names it.
    pub event: String,
}

/// The commands that talk to the server.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in: read a token from standard input, check it, and store it for its project.
    Login(LoginArgs),
    /// Show who the stored token signs in as, and where.
    Whoami,
    /// Show the live and stale claims and the areas they hold.
    Status,
    /// List work items.
    Items(ItemsArgs),
    /// Show the work you can pick up now, soonest first, and why the other open items wait.
    Ready,
    /// Record or remove that a work item is blocked by another.
    #[command(subcommand)]
    Dep(DepCommand),
    /// Create a work item for a reference, or show the one it already names.
    New(NewArgs),
    /// Claim a work item, or renew your claim, with the areas you will change; `dibs claim review`
    /// records the pull request of a claim you hold, and `dibs claim narrow` gives areas of it up.
    Claim(ClaimArgs),
    /// Take over another member's stale claim, telling them why.
    Takeover(TakeoverArgs),
    /// Release your claim; the item is open again.
    Release(KeyArgs),
    /// Complete your claim; the item is done.
    Done(KeyArgs),
    /// Check whether you may edit paths, or warn about the changes of a range of commits.
    Check(CheckArgs),
    /// Let another member edit inside an area you hold, for their work item.
    Grant(GrantArgs),
    /// Start a conversation with members or with the whole project.
    Send(SendArgs),
    /// List the conversations waiting for you.
    Inbox(InboxArgs),
    /// Show a conversation and its messages.
    Thread(ThreadArgs),
    /// Mark a conversation read.
    Read(ThreadArgs),
    /// Acknowledge a message that asks for it, once you have read it.
    Ack(AckArgs),
    /// Ask the project's arbiters to settle what you and another member's agent do not agree on:
    /// who goes first on a work item, a slot, or a conversation left unanswered.
    Escalate(EscalateArgs),
    /// Tell the project's admins that a refusal of dibs was wrong, or that you resolved a rebase
    /// conflict by hand.
    #[command(subcommand)]
    Feedback(FeedbackCommand),
    /// Take, wait for and release slots: short holds on shared actions (a deploy, a release, a
    /// migration) that one member does at a time.
    #[command(subcommand)]
    Slot(SlotCommand),
    /// Send a heartbeat and refresh what this machine remembers about the project, as far as
    /// each is due.
    Sync(SyncArgs),
    /// Serve the same operations as tools over the Model Context Protocol on standard input and
    /// output, for an agent that can call tools.
    Mcp(McpArgs),
    /// Replace this dibs with a newer release, checked against the release key built into it.
    SelfUpdate(SelfUpdateArgs),
    /// Manage the project (admins only).
    #[command(subcommand)]
    Admin(AdminCommand),
}

impl Command {
    /// Whether the command, once done, starts the background sync that sends what the hooks
    /// counted and no heartbeat carried yet: every command but the sync itself, the tool server,
    /// which runs for a whole session, `self-update`, which replaces the binary such a sync would
    /// run, and the pre-push check while the guard is switched off.
    pub fn reports_after(&self, switched_off: bool) -> bool {
        match self {
            Self::Sync(_) | Self::Mcp(_) | Self::SelfUpdate(_) => false,
            Self::Check(args) => !(args.push && switched_off),
            _ => true,
        }
    }

    /// The time limits of the command's requests. A person who runs a command and watches it
    /// waits longer, and a repeatable request of theirs is sent once more when its connection is
    /// lost; the background sync, the tool server an agent calls, and the checks git's hooks run
    /// (`--push`, `--git-range`) keep the short limits the caller waiting on them allows.
    pub fn client_options(&self) -> ClientOptions {
        match self {
            Self::Sync(_) | Self::Mcp(_) => ClientOptions::default(),
            Self::Check(args) if args.push || args.git_range.is_some() => ClientOptions::default(),
            _ => ClientOptions::for_person(),
        }
    }
}

/// `dibs mcp`.
#[derive(Debug, Args)]
pub struct McpArgs {
    /// The agent host that starts the server. A host that can wake a session waiting for its
    /// person gets what it needs for that when `DIBS_PUSH_TO_IDLE=1` is set for the session;
    /// otherwise the server is the same for every host.
    #[arg(long)]
    pub host: Option<String>,
}

/// `dibs sync`.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// The agent host whose session the heartbeat is for.
    #[arg(long, requires = "session")]
    pub host: Option<String>,
    /// The host's id of that session.
    #[arg(long, requires = "host")]
    pub session: Option<String>,
    /// Refresh everything, due or not.
    #[arg(long)]
    pub force: bool,
    /// Print nothing.
    #[arg(long)]
    pub quiet: bool,
    /// Also ask the server, at most once a day, which dibs it recommends, and install it when it
    /// is newer than this one.
    #[arg(long)]
    pub update: bool,
    /// Send a heartbeat, due or not, when this machine counted refusals or failures it has not
    /// reported yet.
    #[arg(long)]
    pub report: bool,
}

/// `dibs self-update`.
#[derive(Debug, Args)]
pub struct SelfUpdateArgs {
    /// The release to install (1.2.3 or v1.2.3); the newest when absent. Never an older one.
    #[arg(long)]
    pub version: Option<String>,
}

/// `dibs login`.
#[derive(Debug, Args)]
pub struct LoginArgs {
    /// The server, when no `.dibs.toml` names it.
    #[arg(long)]
    pub server: Option<String>,
    /// Keep the token in the operating system's credential store instead of the credentials file.
    #[arg(long)]
    pub keychain: bool,
}

/// A state of a work item.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum StatusArg {
    /// Nobody holds it.
    Open,
    /// A member holds it.
    Claimed,
    /// Completed.
    Done,
    /// Given up.
    Dropped,
}

impl From<StatusArg> for ItemStatus {
    fn from(value: StatusArg) -> Self {
        match value {
            StatusArg::Open => Self::Open,
            StatusArg::Claimed => Self::Claimed,
            StatusArg::Done => Self::Done,
            StatusArg::Dropped => Self::Dropped,
        }
    }
}

/// `dibs items`.
#[derive(Debug, Args)]
pub struct ItemsArgs {
    /// Only the items you hold.
    #[arg(long)]
    pub mine: bool,
    /// Only the items in this state.
    #[arg(long, value_enum)]
    pub status: Option<StatusArg>,
}

/// `dibs new`.
#[derive(Debug, Args)]
pub struct NewArgs {
    /// The reference in the outside tracker, `scheme:value` (`gh:acme/api#42`).
    #[arg(value_parser = ExternalRef::parse)]
    pub reference: ExternalRef,
    /// The title.
    #[arg(long, value_parser = Title::parse)]
    pub title: Title,
}

/// A work item named by its local id or its reference.
#[derive(Debug, Args)]
pub struct KeyArgs {
    /// The item: its local id (`12`) or its reference (`gh:acme/api#42`).
    #[arg(value_parser = ItemKey::parse)]
    pub key: ItemKey,
}

/// `dibs claim`.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct ClaimArgs {
    /// What to do with a claim you hold, in place of claiming.
    #[command(subcommand)]
    pub action: Option<ClaimAction>,
    /// The item: its local id or its reference.
    #[arg(value_parser = ItemKey::parse, required = true)]
    pub key: Option<ItemKey>,
    /// An area you will change: `repo:area`, or a name one repository declares. Repeat it for
    /// several.
    #[arg(long = "area")]
    pub areas: Vec<String>,
    /// Make the claim wide over these areas, comma-separated, for a change that cuts across them:
    /// it holds none of them, and each member holding one is asked to acknowledge, which lets you
    /// edit there. Needs `--reason`.
    #[arg(long = "wide", value_delimiter = ',', requires = "reason")]
    pub wide: Vec<String>,
    /// Why the wide change is made, as the members it asks read it.
    #[arg(long, requires = "wide")]
    pub reason: Option<String>,
    /// The work is debt only — refactoring, clean-up — not a feature: a feature claim that needs
    /// one of its areas takes it, after you hand it over. Applies to a new claim; a renewal keeps
    /// the kind the claim has.
    #[arg(long)]
    pub debt: bool,
}

/// What `dibs claim` does with a claim the member holds.
#[derive(Debug, Subcommand)]
pub enum ClaimAction {
    /// Record a pull request of your claim: the claim waits in review, keeps its areas so nobody
    /// edits the files under review, and needs no heartbeat while the pull request is open. Its
    /// merge completes the claim, unless `--more` says more pull requests of the item follow.
    Review(ReviewArgs),
    /// Give areas of your claim up. An area another member's feature claim waits for moves to it
    /// at once: commit and push what you have there first, and list the files you hand over in
    /// the conversation that asked for it.
    Narrow(NarrowArgs),
}

/// `dibs claim narrow`.
#[derive(Debug, Args)]
pub struct NarrowArgs {
    /// The item: its local id or its reference.
    #[arg(value_parser = ItemKey::parse)]
    pub key: ItemKey,
    /// An area to give up: `repo:area`, or a name one repository declares. Repeat it for several.
    #[arg(long = "area", required = true)]
    pub areas: Vec<String>,
}

/// `dibs claim review`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("which").required(true).args(["pr", "drop_pr"])))]
pub struct ReviewArgs {
    /// The item: its local id or its reference.
    #[arg(value_parser = ItemKey::parse)]
    pub key: ItemKey,
    /// The pull request to record, `<owner>/<repo>#<number>`.
    #[arg(long, value_parser = PrRef::parse)]
    pub pr: Option<PrRef>,
    /// More pull requests of the item follow this one: its merge returns the claim to work
    /// rather than completing it.
    #[arg(long, requires = "pr")]
    pub more: bool,
    /// Remove a pull request you recorded by mistake; with none left, the claim is worked on
    /// again.
    #[arg(long, value_parser = PrRef::parse, conflicts_with = "pr")]
    pub drop_pr: Option<PrRef>,
}

/// `dibs dep`.
#[derive(Debug, Subcommand)]
pub enum DepCommand {
    /// Record that the item is blocked by another; an import never removes this edge.
    Add(DepArgs),
    /// Remove that the item is blocked by another.
    Rm(DepArgs),
}

/// `dibs dep add` and `dibs dep rm`.
#[derive(Debug, Args)]
pub struct DepArgs {
    /// The item that waits: its local id or its reference.
    #[arg(value_parser = ItemKey::parse)]
    pub key: ItemKey,
    /// The item it waits for: its local id or its reference.
    #[arg(long = "blocked-by", value_parser = ItemKey::parse)]
    pub blocked_by: ItemKey,
}

/// `dibs takeover`.
#[derive(Debug, Args)]
pub struct TakeoverArgs {
    /// The item: its local id or its reference.
    #[arg(value_parser = ItemKey::parse)]
    pub key: ItemKey,
    /// Why, sent to the member who held it.
    #[arg(long)]
    pub reason: String,
    /// An area you will change. Repeat it for several.
    #[arg(long = "area")]
    pub areas: Vec<String>,
    /// Make the new claim debt work; without it, the claim keeps the kind the stale one had.
    #[arg(long)]
    pub debt: bool,
    /// Make the new claim a feature; without it, the claim keeps the kind the stale one had.
    #[arg(long, conflicts_with = "debt")]
    pub feature: bool,
}

/// `dibs check`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("what").required(true).args(["paths", "git_range", "push"])))]
pub struct CheckArgs {
    /// The paths you are about to change, relative to the current directory or absolute.
    pub paths: Vec<PathBuf>,
    /// Warn about every path the commits of this range change (`origin/main..HEAD`), merge
    /// commits left out. Warnings only: the command always succeeds.
    #[arg(long, value_name = "REV-RANGE", conflicts_with = "paths")]
    pub git_range: Option<String>,
    /// With `--git-range`, only the commits of this author; `me` is the author git is set up as.
    #[arg(long, requires = "git_range", conflicts_with = "paths")]
    pub author: Option<String>,
    /// Check the push git describes on standard input, as its pre-push hook receives it
    /// (`<local ref> <local sha> <remote ref> <remote sha>` per line), with the hook's arguments
    /// as `--remote "$1" --url "$2"`: refused for a protected branch of the repository pushed to
    /// that you are not among the pushers of — judged from what this machine remembers, even when
    /// the server cannot be reached — and while a slot another member holds freezes a ref it
    /// pushes to; silent otherwise. Anything else that keeps it from deciding lets the push go
    /// with one warning.
    #[arg(long, conflicts_with_all = ["paths", "git_range"])]
    pub push: bool,
    /// With `--push`, the remote pushed to: its name, or the URL when the push named none (the
    /// pre-push hook's first argument). Without it, the push is judged as one to the repository
    /// the checkout governs.
    #[arg(long, requires = "push")]
    pub remote: Option<String>,
    /// With `--push`, the URL pushed to (the pre-push hook's second argument).
    #[arg(long, requires = "push")]
    pub url: Option<String>,
}

/// `dibs grant`.
#[derive(Debug, Args)]
pub struct GrantArgs {
    /// The grantee's work item: its local id or its reference.
    #[arg(value_parser = ItemKey::parse)]
    pub key: ItemKey,
    /// The area you hold.
    #[arg(long)]
    pub area: String,
    /// The member who may edit in it.
    #[arg(long, value_parser = Handle::parse)]
    pub to: Handle,
}

/// `dibs send`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("recipients").required(true).args(["to", "project"])))]
pub struct SendArgs {
    /// A member to send it to. Repeat it for several.
    #[arg(long, value_parser = Handle::parse)]
    pub to: Vec<Handle>,
    /// Send it to every person in the project.
    #[arg(long)]
    pub project: bool,
    /// The subject.
    #[arg(long, value_parser = Subject::parse)]
    pub subject: Subject,
    /// The work item it is about: its local id or its reference.
    #[arg(long, value_parser = ItemKey::parse)]
    pub item: Option<ItemKey>,
    /// How much it asks for its recipients' attention: normal, high or urgent.
    #[arg(long, value_parser = Importance::parse, default_value = "normal")]
    pub importance: Importance,
    /// Ask each recipient to acknowledge it.
    #[arg(long)]
    pub ack: bool,
    /// The message; `-` reads it from standard input.
    pub body: String,
}

/// `dibs inbox`.
#[derive(Debug, Args)]
pub struct InboxArgs {
    /// Only the conversations with messages you have not read.
    #[arg(long)]
    pub unread: bool,
    /// Also list the messages that ask you for an acknowledgement you have not given.
    #[arg(long)]
    pub unacked: bool,
}

/// `dibs ack`.
#[derive(Debug, Args)]
pub struct AckArgs {
    /// The message's id, as `dibs thread` and `dibs inbox --unacked` show it.
    #[arg(value_parser = parse_message)]
    pub id: MessageId,
}

fn parse_message(text: &str) -> Result<MessageId, String> {
    text.parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .map(MessageId)
        .ok_or_else(|| format!("'{text}' is not a message id; ids are whole numbers"))
}

/// `dibs feedback`.
#[derive(Debug, Subcommand)]
pub enum FeedbackCommand {
    /// A refusal that was wrong, by the id its reason ends with (`r-7f3a`).
    FalseRefusal(FalseRefusalArgs),
    /// A file whose conflict you resolved by hand while rebasing; only the area it lies in is
    /// kept.
    Conflict(ConflictArgs),
}

/// `dibs feedback false-refusal`.
#[derive(Debug, Args)]
pub struct FalseRefusalArgs {
    /// The refusal's id, as its reason shows it.
    #[arg(value_parser = RefusalId::parse)]
    pub id: RefusalId,
    /// Why it was wrong, one line of at most 280 characters (`-` reads it from standard input).
    /// It is kept as written for 90 days and read by you and the project's admins: name no path,
    /// command line, credential or customer's data.
    #[arg(long)]
    pub reason: String,
}

/// `dibs feedback conflict`.
#[derive(Debug, Args)]
pub struct ConflictArgs {
    /// The file, relative to the current directory or absolute.
    pub path: PathBuf,
    /// What conflicted, one line of at most 280 characters (`-` reads it from standard input). It
    /// is kept as written for 90 days and read by you and the project's admins: name no path,
    /// command line, credential or customer's data.
    #[arg(long)]
    pub reason: String,
}

/// `dibs escalate`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("subject").required(true).args(["item", "slot", "thread"])))]
pub struct EscalateArgs {
    /// The work item: its local id or its reference.
    #[arg(value_parser = ItemKey::parse)]
    pub item: Option<ItemKey>,
    /// The slot instead (`deploy:prod`).
    #[arg(long, value_parser = SlotName::parse)]
    pub slot: Option<SlotName>,
    /// The conversation instead, by its id.
    #[arg(long, value_parser = parse_thread)]
    pub thread: Option<ThreadId>,
    /// Why, as the arbiters read it: what each side wants, and what has been tried.
    #[arg(long, value_parser = EscalationReason::parse)]
    pub reason: EscalationReason,
}

/// A conversation named by its id.
#[derive(Debug, Args)]
pub struct ThreadArgs {
    /// The conversation's id.
    #[arg(value_parser = parse_thread)]
    pub id: ThreadId,
}

fn parse_thread(text: &str) -> Result<ThreadId, String> {
    text.parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .map(ThreadId)
        .ok_or_else(|| format!("'{text}' is not a conversation id; ids are whole numbers"))
}

/// `dibs admin`.
#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Manage members.
    #[command(subcommand)]
    Member(MemberCommand),
    /// Manage tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// Manage the project's settings.
    #[command(subcommand)]
    Settings(SettingsCommand),
    /// End any member's claim on an item.
    ForceRelease(KeyArgs),
    /// Manage slots.
    #[command(subcommand)]
    Slot(AdminSlotCommand),
    /// Manage conversations.
    #[command(subcommand)]
    Thread(AdminThreadCommand),
}

/// `dibs admin thread`.
#[derive(Debug, Subcommand)]
pub enum AdminThreadCommand {
    /// Delete a conversation and every message in it at once; the log keeps only that it was
    /// deleted.
    Delete(ThreadArgs),
}

/// `dibs admin slot`.
#[derive(Debug, Subcommand)]
pub enum AdminSlotCommand {
    /// End any member's hold on a slot, telling them why; the first waiter is offered it.
    ForceRelease(SlotReasonArgs),
}

/// A slot and the reason given for acting on it.
#[derive(Debug, Args)]
pub struct SlotReasonArgs {
    /// The slot (`deploy:prod`).
    #[arg(value_parser = SlotName::parse)]
    pub slot: SlotName,
    /// Why, sent to the member who holds it.
    #[arg(long)]
    pub reason: String,
}

/// The longest `--wait`: under the time an agent's shell tool lets a command run.
pub const MAX_WAIT: Duration = Duration::from_secs(9 * 60);

/// `dibs slot`.
#[derive(Debug, Subcommand)]
pub enum SlotCommand {
    /// Take a slot, renew your hold, or join its queue; with --wait, wait for it to be offered
    /// to you. Exits 0 when you hold it, 1 when you are still queued.
    Take(SlotTakeArgs),
    /// Take the slot (or wait for it with --wait), run the command while holding it with a
    /// heartbeat every 30 seconds, and release it when the command ends, with its exit status.
    Run(SlotRunArgs),
    /// Release a slot you hold; the first member waiting is offered it.
    Release(SlotArgs),
    /// Leave a slot's queue, or give up the offer to you.
    Leave(SlotArgs),
    /// Show every slot: who holds it and why, until when, and who waits.
    List,
    /// Show what a command line needs according to a settings file, without asking a server.
    Test(SlotTestArgs),
    /// Take over another member's stalled hold, telling them what you checked.
    Takeover(SlotTakeoverArgs),
}

/// A slot named on the command line.
#[derive(Debug, Args)]
pub struct SlotArgs {
    /// The slot (`deploy:prod`).
    #[arg(value_parser = SlotName::parse)]
    pub slot: SlotName,
}

/// What a take asks for.
#[derive(Debug, Args)]
pub struct SlotAsk {
    /// The slot (`deploy:prod`).
    #[arg(value_parser = SlotName::parse)]
    pub slot: SlotName,
    /// Why you take it, as every member reads it; never a credential, a host or a customer's
    /// data.
    #[arg(long, value_parser = Purpose::parse)]
    pub purpose: Purpose,
    /// The work item it is for: its local id or its reference.
    #[arg(long, value_parser = ItemKey::parse)]
    pub item: Option<ItemKey>,
    /// When the slot is held, wait this long (at most 9m) for it to be offered to you.
    #[arg(long, value_parser = wait_duration)]
    pub wait: Option<Duration>,
    /// How often a waiting take asks again.
    #[arg(long, hide = true, value_parser = interval, default_value = "10s")]
    pub poll_every: Duration,
}

/// `dibs slot take`.
#[derive(Debug, Args)]
pub struct SlotTakeArgs {
    /// The slot and why.
    #[command(flatten)]
    pub ask: SlotAsk,
    /// Hold it until this forge workflow's run for this commit finishes (`deploy.yml@<sha>`),
    /// whatever this machine does meanwhile.
    #[arg(long, value_name = "WORKFLOW@SHA")]
    pub until_run: Option<String>,
}

/// `dibs slot run`.
#[derive(Debug, Args)]
pub struct SlotRunArgs {
    /// The slot and why.
    #[command(flatten)]
    pub ask: SlotAsk,
    /// How often the run sends its heartbeat.
    #[arg(long, hide = true, value_parser = interval, default_value = "30s")]
    pub heartbeat_every: Duration,
    /// The command and its arguments, after `--`.
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<OsString>,
}

/// `dibs slot takeover`.
#[derive(Debug, Args)]
pub struct SlotTakeoverArgs {
    /// The slot (`deploy:prod`).
    #[arg(value_parser = SlotName::parse)]
    pub slot: SlotName,
    /// What you checked before taking it over, sent to the member whose hold stalled.
    #[arg(long)]
    pub reason: String,
    /// Why you hold it now.
    #[arg(long, value_parser = Purpose::parse)]
    pub purpose: Purpose,
}

/// `dibs slot test`.
#[derive(Debug, Args)]
pub struct SlotTestArgs {
    /// The settings file (TOML), as `dibs admin settings apply` takes it.
    pub settings: PathBuf,
    /// The repository the command runs in, for a project that lists several.
    #[arg(long, value_parser = RepoId::parse)]
    pub repo: Option<RepoId>,
    /// The command line, after `--`; several words are joined by spaces.
    #[arg(last = true, required = true, num_args = 1..)]
    pub line: Vec<String>,
}

fn wait_duration(text: &str) -> Result<Duration, String> {
    let wait = interval(text)?;
    if wait > MAX_WAIT {
        return Err("wait at most 9m, under the time a shell tool lets a command run".to_owned());
    }
    Ok(wait)
}

fn interval(text: &str) -> Result<Duration, String> {
    let parsed = parse_duration(text).map_err(|error| error.to_string())?;
    let seconds = u64::try_from(parsed.whole_seconds()).map_err(|_| "a duration is positive")?;
    if seconds == 0 {
        return Err("a duration is longer than 0s".to_owned());
    }
    Ok(Duration::from_secs(seconds))
}

/// `dibs admin member`.
#[derive(Debug, Subcommand)]
pub enum MemberCommand {
    /// Add a member and print their first token, once.
    Add(MemberAddArgs),
    /// Remove a member: revoke their tokens, end their browser sessions, release their claims and
    /// slots and take them out of slot queues. Their messages stay.
    Remove(HandleArgs),
}

/// `dibs admin member add`.
#[derive(Debug, Args)]
pub struct MemberAddArgs {
    /// Their handle.
    #[arg(value_parser = Handle::parse)]
    pub handle: Handle,
    /// The name shown for them.
    #[arg(long, value_parser = DisplayName::parse)]
    pub name: DisplayName,
    /// Their login in the outside tracker, such as their GitHub login.
    #[arg(long, visible_alias = "github", value_parser = ExternalLogin::parse)]
    pub login: Option<ExternalLogin>,
    /// Their role.
    #[arg(long, value_enum, default_value = "member")]
    pub role: RoleArg,
}

/// The role a new member gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RoleArg {
    /// A person who also manages the project.
    Admin,
    /// A person working in the project.
    Member,
    /// An integration that feeds the project's items from an outside tracker; it holds no claim
    /// and reads no conversation.
    Importer,
}

impl From<RoleArg> for Role {
    fn from(value: RoleArg) -> Self {
        match value {
            RoleArg::Admin => Self::Admin,
            RoleArg::Member => Self::Member,
            RoleArg::Importer => Self::Importer,
        }
    }
}

/// `dibs admin token`.
#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Revoke every token of a member and print a new one, once.
    Rotate(HandleArgs),
}

/// A member named by their handle.
#[derive(Debug, Args)]
pub struct HandleArgs {
    /// Their handle.
    #[arg(value_parser = Handle::parse)]
    pub handle: Handle,
}

/// `dibs admin settings`.
#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Make a settings file the project's settings.
    Apply(FileArgs),
}

/// A file to read.
#[derive(Debug, Args)]
pub struct FileArgs {
    /// The settings file (TOML).
    pub file: PathBuf,
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_command_but_the_sync_the_tool_server_and_self_update_reports_after_it_ran() {
        let reports = |line: &[&str], off: bool| {
            let Invocation::Service(command) = Cli::try_parse_from(line).unwrap().command else {
                panic!("{line:?} is no command of the server");
            };
            command.reports_after(off)
        };
        for line in [
            &["dibs", "status"][..],
            &["dibs", "whoami"],
            &["dibs", "ready"],
            &["dibs", "claim", "12"],
            &["dibs", "release", "12"],
            &["dibs", "inbox"],
            &["dibs", "slot", "list"],
            &["dibs", "check", "src/a.rs"],
            &[
                "dibs",
                "feedback",
                "false-refusal",
                "r-7f3a51",
                "--reason",
                "free",
            ],
            &["dibs", "admin", "force-release", "12"],
        ] {
            assert!(reports(line, false), "{line:?}");
            assert!(
                reports(line, true),
                "the switch silences hooks only: {line:?}"
            );
        }
        for line in [
            &["dibs", "sync"][..],
            &["dibs", "sync", "--report"],
            &["dibs", "mcp"],
            &["dibs", "mcp", "--host", "neutral"],
            &["dibs", "self-update"],
        ] {
            assert!(!reports(line, false), "{line:?}");
        }
        assert!(reports(&["dibs", "check", "--push"], false));
        assert!(!reports(&["dibs", "check", "--push"], true));
    }

    #[test]
    fn a_persons_commands_wait_longer_than_the_sync_the_tool_server_and_gits_hooks() {
        let options = |line: &[&str]| {
            let Invocation::Service(command) = Cli::try_parse_from(line).unwrap().command else {
                panic!("{line:?} is no command of the server");
            };
            command.client_options()
        };
        for line in [
            &["dibs", "login"][..],
            &["dibs", "whoami"],
            &["dibs", "status"],
            &["dibs", "claim", "12"],
            &["dibs", "check", "src/a.rs"],
            &["dibs", "admin", "token", "rotate", "bob"],
            &["dibs", "admin", "member", "add", "carol", "--name", "Carol"],
        ] {
            assert_eq!(options(line), ClientOptions::for_person(), "{line:?}");
        }
        for line in [
            &["dibs", "sync"][..],
            &["dibs", "sync", "--report"],
            &["dibs", "mcp"],
            &["dibs", "check", "--push"],
            &["dibs", "check", "--push", "--remote", "origin"],
            &["dibs", "check", "--git-range", "origin/main..HEAD"],
        ] {
            assert_eq!(options(line), ClientOptions::default(), "{line:?}");
        }
    }

    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_check_takes_paths_or_a_range_but_not_both() {
        assert!(Cli::try_parse_from(["dibs", "check", "a.rs", "b.rs"]).is_ok());
        assert!(Cli::try_parse_from(["dibs", "check", "--git-range", "a..b"]).is_ok());
        assert!(
            Cli::try_parse_from(["dibs", "check", "--git-range", "a..b", "--author", "me"]).is_ok()
        );
        assert!(Cli::try_parse_from(["dibs", "check"]).is_err());
        assert!(Cli::try_parse_from(["dibs", "check", "a.rs", "--git-range", "a..b"]).is_err());
        assert!(Cli::try_parse_from(["dibs", "check", "a.rs", "--author", "me"]).is_err());
    }

    #[test]
    fn a_message_goes_to_members_or_the_project() {
        let parse = |args: &[&str]| Cli::try_parse_from(args.iter().copied());
        assert!(parse(&["dibs", "send", "--to", "bob", "--subject", "s", "hi"]).is_ok());
        assert!(parse(&["dibs", "send", "--project", "--subject", "s", "-"]).is_ok());
        assert!(parse(&["dibs", "send", "--subject", "s", "hi"]).is_err());
        assert!(parse(&["dibs", "send", "--to", "Bob", "--subject", "s", "hi"]).is_err());
    }

    #[test]
    fn a_hook_names_its_host_and_event() {
        let Invocation::Hook(hook) = Cli::try_parse_from(["dibs", "hook", "neutral", "after-tool"])
            .unwrap()
            .command
        else {
            panic!("not a hook");
        };
        assert_eq!(
            (hook.host.as_str(), hook.event.as_str()),
            ("neutral", "after-tool")
        );
        assert!(Cli::try_parse_from(["dibs", "sync", "--session", "s"]).is_err());
        assert!(
            Cli::try_parse_from(["dibs", "sync", "--host", "neutral", "--session", "s"]).is_ok()
        );
        assert!(Cli::try_parse_from(["dibs", "sync", "--update", "--quiet"]).is_ok());
        assert!(Cli::try_parse_from(["dibs", "self-update", "--version", "v1.2.3"]).is_ok());
    }

    #[test]
    fn a_dependency_names_both_items() {
        let Invocation::Service(Command::Dep(DepCommand::Add(dep))) =
            Cli::try_parse_from(["dibs", "dep", "add", "gh:Acme/Web#1", "--blocked-by", "12"])
                .unwrap()
                .command
        else {
            panic!("not a dependency");
        };
        assert_eq!(dep.key.to_string(), "gh:acme/web#1");
        assert_eq!(dep.blocked_by.to_string(), "12");
        assert!(Cli::try_parse_from(["dibs", "dep", "rm", "1"]).is_err());
        assert!(Cli::try_parse_from(["dibs", "ready"]).is_ok());
    }

    #[test]
    fn keys_are_ids_or_references() {
        let Invocation::Service(Command::Claim(claim)) =
            Cli::try_parse_from(["dibs", "claim", "gh:acme/api#4", "--area", "billing"])
                .unwrap()
                .command
        else {
            panic!("not a claim");
        };
        assert_eq!(
            claim.key.as_ref().map(ToString::to_string).as_deref(),
            Some("gh:acme/api#4")
        );
        assert_eq!(claim.areas, ["billing"]);
        assert!(claim.action.is_none());
        let Invocation::Service(Command::Claim(review)) = Cli::try_parse_from([
            "dibs",
            "claim",
            "review",
            "42",
            "--pr",
            "acme/api#7",
            "--more",
        ])
        .unwrap()
        .command
        else {
            panic!("not a claim");
        };
        let Some(ClaimAction::Review(review)) = review.action else {
            panic!("not a review");
        };
        assert_eq!(review.key.to_string(), "42");
        assert_eq!(
            review.pr.map(|pr| pr.to_string()).as_deref(),
            Some("acme/api#7")
        );
        assert!(review.more);
        assert!(
            Cli::try_parse_from(["dibs", "claim", "review", "42", "--drop-pr", "acme/api#7"])
                .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "dibs",
                "claim",
                "review",
                "42",
                "--pr",
                "acme/api#7",
                "--drop-pr",
                "acme/api#8"
            ])
            .is_err()
        );
        let Invocation::Service(Command::Claim(debt)) =
            Cli::try_parse_from(["dibs", "claim", "42", "--area", "billing", "--debt"])
                .unwrap()
                .command
        else {
            panic!("not a claim");
        };
        assert!(debt.debt);
        let Invocation::Service(Command::Claim(narrow)) = Cli::try_parse_from([
            "dibs",
            "claim",
            "narrow",
            "42",
            "--area",
            "billing",
            "--area",
            "api:search",
        ])
        .unwrap()
        .command
        else {
            panic!("not a claim");
        };
        let Some(ClaimAction::Narrow(narrow)) = narrow.action else {
            panic!("not a narrowing");
        };
        assert_eq!(narrow.key.to_string(), "42");
        assert_eq!(narrow.areas, ["billing", "api:search"]);
        assert!(Cli::try_parse_from(["dibs", "claim", "narrow", "42"]).is_err());
        assert!(Cli::try_parse_from(["dibs", "claim"]).is_err());
        assert!(Cli::try_parse_from(["dibs", "claim", "review", "42"]).is_err());
        assert!(
            Cli::try_parse_from(["dibs", "claim", "review", "42", "--pr", "acme/api"]).is_err()
        );
        assert!(Cli::try_parse_from(["dibs", "release", "not a key"]).is_err());
        assert!(Cli::try_parse_from(["dibs", "thread", "0"]).is_err());
    }
}
