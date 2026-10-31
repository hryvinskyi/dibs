//! Worktree pools: `worktree take`, `worktree release`, `worktree list` and `worktree prepare`.
//!
//! A checkout whose `.dibs.toml` has a `[worktrees]` table lends a fixed pool of long-lived
//! worktrees beside it to its member's sessions and agents, one holder each; the server keeps the
//! holds, this machine keeps the worktrees. Every command run inside one of the pool's worktrees
//! acts on the pool of the checkout that lends it.

pub mod pool;
pub mod project;
pub mod rescue;
pub mod select;
pub mod take;

use std::fmt::Write as _;
use std::path::PathBuf;

use rand::RngExt as _;
use serde::Serialize;

use dibs_agent_host::ports::Clock;
use dibs_client::api::{OwnWorktreeHold, ReleaseWorktreeRequest, WorktreeRelease};
use dibs_client::locations::Locations;
use dibs_client::state::{BrokenWorktree, WorktreeHoldRecord};
use dibs_domain::report::MachineId;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{AgentKey, AgentKeyError, WorktreeName};

use crate::cli::{WorktreeCommand, WorktreePrepareArgs, WorktreeReleaseArgs, WorktreeTakeArgs};
use crate::commands::worktree::pool::Pool;
use crate::commands::worktree::project::{ProjectCommands, ShellCommands, Site, Step};
use crate::commands::worktree::rescue::{Author, WorktreeGit};
use crate::commands::worktree::take::{Ask, TakeError, Taken, Taker};
use crate::context::Context;
use crate::failure::{Failure, Kind, sentence};
use crate::guard::SystemClock;
use crate::guard::machine::machine_id;

/// The purpose of a take that names none.
const DEFAULT_PURPOSE: &str = "work in a worktree";

/// Makes the key a take mints for a holder that names no agent of its own, so that two takes of
/// one session are two holders.
pub trait IdGen {
    /// A fresh agent key.
    ///
    /// # Errors
    /// [`AgentKeyError`] when the key made breaks the rules of one.
    fn agent_key(&self) -> Result<AgentKey, AgentKeyError>;
}

/// Agent keys from the machine's random number generator: `cli-` and 12 hexadecimal digits.
pub struct RandomIds;

impl IdGen for RandomIds {
    fn agent_key(&self) -> Result<AgentKey, AgentKeyError> {
        let bits = rand::rng().random::<u64>() & 0xffff_ffff_ffff;
        AgentKey::parse(&format!("cli-{bits:012x}"))
    }
}

/// Runs a worktree command.
///
/// # Errors
/// The command's [`Failure`].
pub async fn run(command: WorktreeCommand, ctx: &Context) -> Result<(), Failure> {
    match command {
        WorktreeCommand::Take(args) => take(args, ctx, &RandomIds).await,
        WorktreeCommand::Release(args) => release(args, ctx).await,
        WorktreeCommand::List => list(ctx).await,
        WorktreeCommand::Prepare(args) => prepare(args, ctx).await,
    }
}

/// The pool the current directory belongs to, the worktree it lies in when it is one, and the
/// client's directories.
fn locate(ctx: &Context) -> Result<(Pool, Option<WorktreeName>, Locations), Failure> {
    let here = ctx.checkout()?.ok_or_else(|| {
        Failure::config(
            "no_checkout",
            "No .dibs.toml was found here or above in this repository; run the command in a checkout whose .dibs.toml has a [worktrees] table, or in one of its worktrees.",
        )
    })?;
    let locations = ctx.locations()?;
    let (pool, inside) = Pool::locate(&here, locations.state_dir())?;
    Ok((pool, inside, locations))
}

/// This machine's id.
fn machine(locations: &Locations) -> Result<MachineId, Failure> {
    machine_id(locations.state_dir()).ok_or_else(|| {
        Failure::config(
            "no_machine_id",
            "This machine's id can be neither read nor kept in the state directory, so its worktree pool cannot be named.",
        )
    })
}

async fn take(args: WorktreeTakeArgs, ctx: &Context, ids: &dyn IdGen) -> Result<(), Failure> {
    let (pool, _, locations) = locate(ctx)?;
    let connection = ctx.connect().await?;
    let machine = machine(&locations)?;
    let session = connection.session().await?;
    let (agent, minted) = match args.agent {
        Some(agent) => (agent, false),
        None => (
            ids.agent_key().map_err(|_| {
                Failure::config("no_agent_key", "No agent key could be made for this take.")
            })?,
            true,
        ),
    };
    let item = match &args.item {
        Some(key) => Some(connection.resolve(key).await?),
        None => None,
    };
    let purpose = match args.purpose {
        Some(purpose) => purpose,
        None => Purpose::parse(DEFAULT_PURPOSE)
            .map_err(|_| Failure::config("invalid_purpose", "Give the take a --purpose."))?,
    };
    let member = &connection.me.member;
    let printer = ctx.printer;
    let notes = |text: &str| printer.note(text);
    let taker = Taker {
        client: &connection.client,
        machine,
        pool: &pool,
        commands: &ShellCommands,
        clock: &SystemClock,
        author: Author {
            name: member.display_name.to_string(),
            email: format!("{}@users.dibs.invalid", member.handle),
        },
        notes: &notes,
    };
    let ask = Ask {
        session,
        agent: Some(agent.clone()),
        purpose,
        item,
        wait: args.wait.unwrap_or_default(),
        deadline: args.deadline,
        poll_every: args.poll_every,
    };
    match taker.take(&ask).await {
        Ok(Taken::Took(taken)) => {
            if taken.again {
                printer.note(&format!(
                    "{} is held by this holder already; it was left as it is.",
                    taken.worktree
                ));
            }
            if minted {
                printer.note(&format!(
                    "{} is held for the agent {agent}; release it with dibs worktree release --agent {agent}, or dibs worktree release {}.",
                    taken.worktree, taken.worktree
                ));
            }
            printer.show(&taken, || taken.path.display().to_string());
            Ok(())
        }
        Ok(Taken::Busy { sentence, .. }) => Err(Failure::refused("worktree_busy", sentence)),
        Err(TakeError::Server(error)) => Err(connection.failed(error)),
        Err(TakeError::Unusable(sentence)) => Err(Failure::refused("worktree_unusable", sentence)),
    }
}

async fn release(args: WorktreeReleaseArgs, ctx: &Context) -> Result<(), Failure> {
    let (pool, inside, locations) = locate(ctx)?;
    let named = match (&args.worktree, &args.agent) {
        (Some(name), _) => Some(pool.known(name)?),
        (None, Some(_)) => None,
        (None, None) => Some(inside.ok_or_else(|| {
            Failure::config(
                "no_worktree_named",
                "Name the worktree to release, or its agent with --agent; outside a worktree of the pool dibs cannot tell which.",
            )
        })?),
    };
    let connection = ctx.connect().await?;
    let machine = machine(&locations)?;
    let session = connection.session().await?;
    let release = match (&named, args.agent) {
        (Some(name), _) => WorktreeRelease::Worktree {
            worktree: name.clone(),
        },
        (None, agent) => WorktreeRelease::Holder {
            holder_session: session,
            agent,
        },
    };
    let request = ReleaseWorktreeRequest {
        session_id: session,
        machine,
        pool: pool.id().clone(),
        release,
    };
    let files = pool.files();
    match connection.client.release_worktree(&request).await {
        Ok(released) => {
            let name = &released.released.worktree;
            if let Err(error) = files.forget_hold(name) {
                tracing::debug!(error = %error, "a worktree's holder could not be forgotten");
            }
            if let Err(error) = files.mark_free(name, SystemClock.now()) {
                tracing::debug!(error = %error, "a worktree's release could not be recorded");
            }
            ctx.printer.show(&released, || format!("Released {name}."));
            Ok(())
        }
        Err(error) => {
            if error.code() == "worktree_not_held"
                && let Some(name) = &named
                && let Err(io) = files.forget_hold(name)
            {
                tracing::debug!(error = %io, "a worktree's holder could not be forgotten");
            }
            Err(connection.failed(error))
        }
    }
}

/// `dibs worktree list`, as `--json` prints it.
#[derive(Debug, Serialize)]
struct Listing {
    /// Where the pool's worktrees live.
    root: PathBuf,
    /// Whether the server's holds are in it; when not, only this machine's records are.
    server: bool,
    /// Each worktree of the pool, in order.
    worktrees: Vec<Listed>,
}

/// One worktree of `dibs worktree list`.
#[derive(Debug, Serialize)]
struct Listed {
    worktree: WorktreeName,
    path: PathBuf,
    prepared: bool,
    stopped: bool,
    broken: Option<BrokenWorktree>,
    held: Option<OwnWorktreeHold>,
    record: Option<WorktreeHoldRecord>,
    rescue_branches: Vec<String>,
}

async fn list(ctx: &Context) -> Result<(), Failure> {
    let (pool, _, locations) = locate(ctx)?;
    let mut holds: Vec<OwnWorktreeHold> = Vec::new();
    let mut server = false;
    match ctx.connect().await {
        Ok(connection) => {
            let machine = machine(&locations)?;
            match connection.client.worktrees(Some(&machine)).await {
                Ok(answer) => {
                    server = true;
                    holds = answer
                        .pools
                        .into_iter()
                        .filter(|own| own.pool == *pool.id())
                        .flat_map(|own| own.holds)
                        .collect();
                }
                Err(error) if error.is_unavailable() => {
                    ctx.printer.warn(&format!(
                        "{} Only what this machine remembers is shown.",
                        sentence(&error.to_string())
                    ));
                }
                Err(error) => return Err(connection.failed(error)),
            }
        }
        Err(failure) if failure.kind == Kind::Unreachable => {
            ctx.printer.warn(&format!(
                "{} Only what this machine remembers is shown.",
                failure.message
            ));
        }
        Err(failure) => return Err(failure),
    }
    let checkout_git = WorktreeGit::new(pool.checkout().root());
    let files = pool.files();
    let worktrees = pool
        .names()
        .into_iter()
        .map(|name| Listed {
            path: pool.path(&name),
            prepared: pool.is_prepared(&name),
            stopped: files.stopped_at(&name).is_some(),
            broken: files.broken(&name),
            held: holds.iter().find(|hold| hold.worktree == name).cloned(),
            record: files.hold(&name),
            rescue_branches: checkout_git.rescue_branches(&name),
            worktree: name,
        })
        .collect();
    let listing = Listing {
        root: pool.settings().root.clone(),
        server,
        worktrees,
    };
    ctx.printer.show(&listing, || listing_text(&listing));
    Ok(())
}

fn listing_text(listing: &Listing) -> String {
    let mut text = format!("Worktrees in {}:\n", listing.root.display());
    for listed in &listing.worktrees {
        let state = match (listed.prepared, listed.stopped) {
            (false, _) => "not prepared",
            (true, false) => "prepared",
            (true, true) => "prepared, environment stopped",
        };
        let _ = write!(text, "  {}  {state}", listed.worktree);
        match (&listed.held, &listed.record) {
            (Some(hold), _) => {
                let who = hold.agent.as_ref().map_or_else(
                    || format!("the session {}", hold.session),
                    |agent| format!("the agent {agent} of {}", hold.session),
                );
                let _ = write!(
                    text,
                    "; held by {who} for {} since {}",
                    hold.purpose,
                    clock(hold.taken_at)
                );
            }
            (None, Some(record)) if !listing.server => {
                let who = record.agent.as_ref().map_or_else(
                    || "a session".to_owned(),
                    |agent| format!("the agent {agent}"),
                );
                let _ = write!(
                    text,
                    "; taken on this machine for {who} since {}",
                    clock(record.taken_at)
                );
            }
            (None, _) => text.push_str("; free"),
        }
        if let Some(broken) = &listed.broken {
            let _ = write!(text, "; set aside as broken: {}", broken.reason);
        }
        if !listed.rescue_branches.is_empty() {
            let _ = write!(text, "; rescued on {}", listed.rescue_branches.join(", "));
        }
        text.push('\n');
    }
    text
}

/// A time as the list shows it: `2026-03-02 09:05 UTC`.
fn clock(at: time::OffsetDateTime) -> String {
    let at = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute()
    )
}

/// The longest `dibs worktree prepare` waits for another run to let go of a worktree.
const PREPARE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(25 * 60);

async fn prepare(args: WorktreePrepareArgs, ctx: &Context) -> Result<(), Failure> {
    let (pool, _, _) = locate(ctx)?;
    let names = match &args.worktree {
        Some(name) => vec![pool.known(name)?],
        None => pool.names(),
    };
    let mut failed = Vec::new();
    for name in &names {
        match prepare_one(&pool, name, &ShellCommands, &SystemClock, ctx).await {
            Ok(()) => ctx.printer.note(&format!("{name} is prepared.")),
            Err(reason) => {
                ctx.printer
                    .warn(&format!("{name} could not be prepared: {reason}."));
                failed.push(name.to_string());
            }
        }
    }
    if failed.is_empty() {
        return Ok(());
    }
    Err(Failure::refused(
        "worktree_unusable",
        format!(
            "{} could not be prepared and stays set aside; the output above says why.",
            failed.join(", ")
        ),
    )
    .after_answer())
}

/// Prepares one worktree under its lock: runs the project's `prepare`, writes the worktree's
/// `.dibs.toml` and takes it back into the pool; sets it aside when the preparation fails.
async fn prepare_one(
    pool: &Pool,
    name: &WorktreeName,
    commands: &dyn ProjectCommands,
    clock: &dyn Clock,
    ctx: &Context,
) -> Result<(), String> {
    if pool.is_locked(name) {
        ctx.printer.note(&format!(
            "Another dibs is preparing, resetting or stopping {name}; waiting for it."
        ));
    }
    let lock = pool
        .lock(name, PREPARE_PATIENCE)
        .await
        .map_err(|_| "its lock could not be taken on this machine".to_owned())?;
    let Some(_lock) = lock else {
        return Err("another dibs is still preparing, resetting or stopping it".to_owned());
    };
    let path = pool.path(name);
    let settings = pool.settings();
    let site = Site {
        name,
        path: &path,
        base: &settings.base,
        checkout: pool.checkout().root(),
    };
    let files = pool.files();
    let outcome = commands
        .run(
            Step::Prepare,
            &settings.prepare,
            &site,
            Step::Prepare.limit(),
        )
        .map_err(|failed| failed.reason())
        .and_then(|()| {
            pool.write_config(name)
                .map_err(|_| "its .dibs.toml could not be written".to_owned())
        });
    match &outcome {
        Ok(()) => {
            if let Err(error) = files.clear_broken(name) {
                tracing::debug!(error = %error, "a worktree could not be taken back");
            }
        }
        Err(reason) => {
            let broken = BrokenWorktree {
                at: clock.now(),
                reason: sentence(reason),
            };
            if let Err(error) = files.mark_broken(name, &broken) {
                tracing::debug!(error = %error, "a worktree could not be set aside");
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_agent_key_is_valid_and_fresh_each_time() {
        let first = RandomIds.agent_key().unwrap();
        let second = RandomIds.agent_key().unwrap();
        assert!(first.as_str().starts_with("cli-"), "{first}");
        assert_ne!(first, second);
    }
}
