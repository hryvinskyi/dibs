//! What each command of the command line does, and the status it exits with: 0 when it did what
//! it was asked, 1 when it refused or failed, 2 when the configuration is refused.
//!
//! The server logs JSON lines to standard output. Every other command writes its answer to
//! standard output and its log to standard error; a command that issues a token writes the token
//! alone on standard output and the sentence about it on standard error, so a script can capture
//! the token and nothing else.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use tracing::level_filters::LevelFilter;

use dibs_app::token::SecretToken;
use dibs_app::use_cases::{CreateMemberInput, InitProjectInput, NewMember, UseCases};
use dibs_app::{AppError, Operator};
use dibs_domain::ids::ThreadId;
use dibs_domain::member::Role;
use dibs_domain::project::ProjectSlug;
use dibs_store_sqlite::SqliteStore;

use crate::adapters::{BroadcastNotifier, SystemClock};
use crate::cli::{AdminCommand, Cli, Command, MemberCommand, ThreadCommand, TokenCommand};
use crate::config::Config;
use crate::hold::{DatabaseHold, HoldError};
use crate::restore::restore;
use crate::serve::{serve, shutdown_signal, use_cases};
use crate::snapshot::Snapshots;

/// A command's outcome when it did not do what it was asked.
enum Failed {
    /// The configuration is refused (status 2).
    Config(String),
    /// The command refused or failed (status 1).
    Refused(String),
}

/// Runs the command `cli` names, reading the configuration through `lookup`.
pub async fn run(cli: Cli, lookup: impl Fn(&str) -> Option<String>) -> ExitCode {
    let configured =
        || Config::from_lookup(&lookup).map_err(|error| Failed::Config(error.to_string()));
    let outcome = match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => match configured() {
            Ok(config) => serve_until_stopped(config).await,
            Err(failed) => Err(failed),
        },
        Command::Migrate => match configured() {
            Ok(config) => {
                log_to_stderr(config.log);
                migrate(&config.database).await
            }
            Err(failed) => Err(failed),
        },
        Command::Admin(command) => match configured() {
            Ok(config) => {
                log_to_stderr(config.log);
                admin(command, &config).await
            }
            Err(failed) => Err(failed),
        },
        Command::Restore(args) => {
            log_to_stderr(LevelFilter::INFO);
            restore_offline(&args.snapshot, &args.to).await
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failed::Config(sentence)) => {
            eprintln!("dibs-server: {sentence}.");
            ExitCode::from(2)
        }
        Err(Failed::Refused(sentence)) => {
            eprintln!("dibs-server: {sentence}.");
            ExitCode::FAILURE
        }
    }
}

async fn serve_until_stopped(config: Config) -> Result<(), Failed> {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(config.log)
        .with_current_span(false)
        .with_span_list(false)
        .init();
    serve(config, shutdown_signal()).await.map_err(|error| {
        tracing::error!(error = ?error, "the server stopped");
        Failed::Refused(error.to_string())
    })
}

async fn restore_offline(snapshot: &Path, target: &Path) -> Result<(), Failed> {
    let restored = restore(snapshot, target).await.map_err(|error| {
        tracing::warn!(cause = ?error, "the restore refused or failed");
        Failed::Refused(error.to_string())
    })?;
    println!(
        "Restored {} into {} ({} bytes). Start the server on it with DIBS_DATABASE={}.",
        snapshot.display(),
        restored.database.display(),
        restored.bytes,
        restored.database.display()
    );
    Ok(())
}

async fn migrate(database: &Path) -> Result<(), Failed> {
    let _hold = DatabaseHold::take(database).map_err(|error| match error {
        HoldError::Held { .. } => Failed::Refused(format!(
            "a running server, a restore or another migration holds {}; stop it first",
            database.display()
        )),
        HoldError::Io { .. } => Failed::Refused(error.to_string()),
    })?;
    let store = open_existing(database).await?;
    store.close().await;
    println!("The schema of {} is up to date.", database.display());
    Ok(())
}

async fn admin(command: AdminCommand, config: &Config) -> Result<(), Failed> {
    let store = match &command {
        AdminCommand::Init(_) => open(&config.database).await?,
        _ => open_existing(&config.database).await?,
    };
    let done = admin_on(command, config, &store).await;
    store.close().await;
    done
}

async fn admin_on(
    command: AdminCommand,
    config: &Config,
    store: &Arc<SqliteStore>,
) -> Result<(), Failed> {
    let app = use_cases(store.clone(), Arc::new(BroadcastNotifier::new(1)));
    match command {
        AdminCommand::Init(args) => {
            let created = app
                .init_project
                .execute(InitProjectInput {
                    slug: args.project,
                    admin: NewMember {
                        handle: args.admin,
                        display_name: args.name,
                        external_login: args.login,
                    },
                })
                .await
                .map_err(refused)?;
            show_token(
                &format!(
                    "Created the project '{}' with its admin '{}'.",
                    created.project.slug, created.admin.handle
                ),
                &created.token,
            );
        }
        AdminCommand::Member(MemberCommand::Add(args)) => {
            let operator = operator(&app, &args.project).await?;
            let created = app
                .create_member
                .execute(
                    &operator,
                    CreateMemberInput {
                        member: NewMember {
                            handle: args.handle,
                            display_name: args.name,
                            external_login: args.login,
                        },
                        role: Role::from(args.role),
                    },
                )
                .await
                .map_err(refused)?;
            show_token(
                &format!(
                    "Added '{}' to the project '{}'.",
                    created.member.handle, args.project
                ),
                &created.token,
            );
        }
        AdminCommand::Member(MemberCommand::Remove(args)) => {
            let operator = operator(&app, &args.project).await?;
            let removed = app
                .remove_member
                .execute(&operator, &args.handle)
                .await
                .map_err(refused)?;
            println!(
                "Removed '{}' from the project '{}': {} token(s) revoked, {} browser session(s) ended, {} claim(s) released, {} slot hold(s) ended, {} queue place(s) and {} offer(s) given up. Their messages stay.",
                removed.member.handle,
                args.project,
                removed.tokens_revoked,
                removed.ui_sessions_ended,
                removed.released.len(),
                removed.slots.held.len(),
                removed.slots.waited.len(),
                removed.slots.offered.len()
            );
        }
        AdminCommand::Thread(ThreadCommand::Delete(args)) => {
            let operator = operator(&app, &args.project).await?;
            let deleted = app
                .delete_thread
                .execute(&operator, ThreadId(args.thread))
                .await
                .map_err(refused)?;
            println!(
                "Deleted conversation {} of the project '{}' with its {} message(s).",
                deleted.thread, args.project, deleted.messages
            );
        }
        AdminCommand::Token(TokenCommand::Rotate(args)) => {
            let operator = operator(&app, &args.project).await?;
            let rotated = app
                .rotate_token
                .execute(&operator, &args.handle)
                .await
                .map_err(refused)?;
            show_token(
                &format!(
                    "Revoked every token of '{}' in the project '{}'.",
                    rotated.member.handle, args.project
                ),
                &rotated.token,
            );
        }
        AdminCommand::Snapshot => {
            let Some(settings) = &config.snapshots else {
                return Err(Failed::Config(
                    "DIBS_SNAPSHOT_DIR is not set; set it to the directory snapshots are written into"
                        .to_owned(),
                ));
            };
            let snapshots = Snapshots::new(store.clone(), Arc::new(SystemClock), settings);
            let taken = snapshots.take().await.map_err(|error| {
                tracing::error!(error = %error, cause = ?error, "the snapshot failed");
                Failed::Refused(error.to_string())
            })?;
            println!(
                "Wrote {} ({} bytes in {} ms); deleted {} older snapshots.",
                taken.path.display(),
                taken.bytes,
                taken.duration.as_millis(),
                taken.removed
            );
        }
    }
    Ok(())
}

async fn operator(app: &UseCases, project: &ProjectSlug) -> Result<Operator, Failed> {
    app.operate_project.execute(project).await.map_err(refused)
}

/// Opens the database, creating it when it does not exist.
async fn open(database: &Path) -> Result<Arc<SqliteStore>, Failed> {
    SqliteStore::open(database)
        .await
        .map(Arc::new)
        .map_err(|error| {
            tracing::error!(cause = ?error, "the database could not be opened");
            Failed::Refused(format!(
                "the database {} could not be opened",
                database.display()
            ))
        })
}

/// Opens the database, refusing when there is none: only creating a project creates the file.
async fn open_existing(database: &Path) -> Result<Arc<SqliteStore>, Failed> {
    if !database.is_file() {
        return Err(Failed::Refused(format!(
            "there is no database at {}; create it with admin init, or check DIBS_DATABASE",
            database.display()
        )));
    }
    open(database).await
}

fn refused(error: AppError) -> Failed {
    if matches!(error, AppError::Store(_)) {
        tracing::error!(cause = ?error, "the store failed");
    }
    Failed::Refused(error.to_string())
}

fn show_token(sentence: &str, token: &SecretToken) {
    eprintln!(
        "{sentence} The token follows on standard output, shown this once; hand it over through a password manager."
    );
    println!("{}", token.expose());
}

fn log_to_stderr(level: LevelFilter) {
    tracing_subscriber::fmt()
        .json()
        .with_max_level(level)
        .with_current_span(false)
        .with_span_list(false)
        .with_writer(std::io::stderr)
        .init();
}
