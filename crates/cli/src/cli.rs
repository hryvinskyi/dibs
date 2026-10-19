//! The command line as clap reads it.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

use dibs_domain::handle::Handle;
use dibs_domain::ids::ThreadId;
use dibs_domain::item::{ExternalRef, ItemKey, ItemStatus, Title};
use dibs_domain::member::{DisplayName, ExternalLogin};
use dibs_domain::message::Subject;

/// Claim work, check edits and talk to the people and agents working on the same repositories.
#[derive(Debug, Parser)]
#[command(name = "dibs", version)]
pub struct Cli {
    /// Print every answer as JSON on standard output.
    #[arg(long, global = true)]
    pub json: bool,
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The commands.
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
    /// Create a work item for a reference, or show the one it already names.
    New(NewArgs),
    /// Claim a work item, or renew your claim, with the areas you will change.
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
    /// Manage the project (admins only).
    #[command(subcommand)]
    Admin(AdminCommand),
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
pub struct ClaimArgs {
    /// The item: its local id or its reference.
    #[arg(value_parser = ItemKey::parse)]
    pub key: ItemKey,
    /// An area you will change: `repo:area`, or a name one repository declares. Repeat it for
    /// several.
    #[arg(long = "area")]
    pub areas: Vec<String>,
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
}

/// `dibs check`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("what").required(true).args(["paths", "git_range"])))]
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
    /// The message; `-` reads it from standard input.
    pub body: String,
}

/// `dibs inbox`.
#[derive(Debug, Args)]
pub struct InboxArgs {
    /// Only the conversations with messages you have not read.
    #[arg(long)]
    pub unread: bool,
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
}

/// `dibs admin member`.
#[derive(Debug, Subcommand)]
pub enum MemberCommand {
    /// Add a member and print their first token, once.
    Add(MemberAddArgs),
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
    /// Their login in the outside tracker.
    #[arg(long, value_parser = ExternalLogin::parse)]
    pub login: Option<ExternalLogin>,
    /// Make them an admin.
    #[arg(long)]
    pub admin: bool,
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
    fn keys_are_ids_or_references() {
        let Command::Claim(claim) =
            Cli::try_parse_from(["dibs", "claim", "gh:acme/api#4", "--area", "billing"])
                .unwrap()
                .command
        else {
            panic!("not a claim");
        };
        assert_eq!(claim.key.to_string(), "gh:acme/api#4");
        assert_eq!(claim.areas, ["billing"]);
        assert!(Cli::try_parse_from(["dibs", "release", "not a key"]).is_err());
        assert!(Cli::try_parse_from(["dibs", "thread", "0"]).is_err());
    }
}
