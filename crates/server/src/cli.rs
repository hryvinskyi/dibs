//! The command line of `dibs-server`: serving, which is what it does with no command, and the
//! operator's commands on its database file.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use dibs_domain::handle::Handle;
use dibs_domain::member::{DisplayName, ExternalLogin};
use dibs_domain::project::ProjectSlug;

/// `dibs-server`.
#[derive(Debug, Parser)]
#[command(
    name = "dibs-server",
    version,
    about = "The dibs server, and the operator's commands on its database",
    long_about = "The dibs server, and the operator's commands on its database.\n\nEvery command \
                  but restore reads the database and the snapshot settings from DIBS_DATABASE, \
                  DIBS_SNAPSHOT_DIR, DIBS_SNAPSHOT_EVERY and DIBS_SNAPSHOT_KEEP, as the server \
                  does."
)]
pub struct Cli {
    /// What to do; serving when nothing is named.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serve the API and the pages until SIGTERM or Ctrl-C (the default).
    Serve,
    /// Create projects, add members, rotate tokens and take snapshots.
    #[command(subcommand)]
    Admin(AdminCommand),
    /// Restore a snapshot into a new database file. Run it with no server on that file.
    Restore(RestoreArgs),
    /// Bring the database's schema up to date. Run it with no server on the file.
    Migrate,
}

/// `dibs-server admin`.
#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Create a project with its first admin and print the admin's token, once.
    Init(InitArgs),
    /// Manage a project's members.
    #[command(subcommand)]
    Member(MemberCommand),
    /// Manage members' tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// Write one snapshot into DIBS_SNAPSHOT_DIR now and delete those past DIBS_SNAPSHOT_KEEP.
    Snapshot,
}

/// `dibs-server admin init`.
#[derive(Debug, Args)]
pub struct InitArgs {
    /// The project's slug.
    #[arg(long, value_parser = ProjectSlug::parse)]
    pub project: ProjectSlug,
    /// The first admin's handle.
    #[arg(long, value_parser = Handle::parse)]
    pub admin: Handle,
    /// The name shown for the admin.
    #[arg(long, value_parser = DisplayName::parse)]
    pub name: DisplayName,
    /// The admin's login in the outside tracker.
    #[arg(long, value_parser = ExternalLogin::parse)]
    pub login: Option<ExternalLogin>,
}

/// `dibs-server admin member`.
#[derive(Debug, Subcommand)]
pub enum MemberCommand {
    /// Add a member to a project and print their first token, once.
    Add(MemberAddArgs),
}

/// `dibs-server admin member add`.
#[derive(Debug, Args)]
pub struct MemberAddArgs {
    /// The project's slug.
    #[arg(long, value_parser = ProjectSlug::parse)]
    pub project: ProjectSlug,
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

/// `dibs-server admin token`.
#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Revoke every token of a member and print a new one, once.
    Rotate(RotateArgs),
}

/// `dibs-server admin token rotate`.
#[derive(Debug, Args)]
pub struct RotateArgs {
    /// The project's slug.
    #[arg(value_parser = ProjectSlug::parse)]
    pub project: ProjectSlug,
    /// The member's handle.
    #[arg(value_parser = Handle::parse)]
    pub handle: Handle,
}

/// `dibs-server restore`.
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// The snapshot file.
    pub snapshot: PathBuf,
    /// The database file to create; it must not exist.
    #[arg(long)]
    pub to: PathBuf,
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
    fn no_command_means_serving() {
        let cli = Cli::try_parse_from(["dibs-server"]).unwrap();
        assert!(cli.command.is_none());
        let cli = Cli::try_parse_from(["dibs-server", "serve"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Serve)));
    }

    #[test]
    fn the_admin_commands_take_their_values_in_their_types() {
        let cli = Cli::try_parse_from([
            "dibs-server",
            "admin",
            "init",
            "--project",
            "widgets",
            "--admin",
            "alice",
            "--name",
            "Alice",
        ])
        .unwrap();
        let Some(Command::Admin(AdminCommand::Init(init))) = cli.command else {
            panic!("not an init");
        };
        assert_eq!(init.project.as_str(), "widgets");
        assert_eq!(init.admin.as_str(), "alice");
        assert!(init.login.is_none());

        let cli =
            Cli::try_parse_from(["dibs-server", "admin", "token", "rotate", "widgets", "bob"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Admin(AdminCommand::Token(TokenCommand::Rotate(_))))
        ));
        assert!(
            Cli::try_parse_from([
                "dibs-server",
                "admin",
                "init",
                "--project",
                "Not A Slug",
                "--admin",
                "alice",
                "--name",
                "Alice",
            ])
            .is_err()
        );
    }

    #[test]
    fn a_restore_names_its_target() {
        assert!(Cli::try_parse_from(["dibs-server", "restore", "snap.db"]).is_err());
        let cli =
            Cli::try_parse_from(["dibs-server", "restore", "snap.db", "--to", "dibs.db"]).unwrap();
        let Some(Command::Restore(args)) = cli.command else {
            panic!("not a restore");
        };
        assert_eq!(args.to, PathBuf::from("dibs.db"));
    }
}
