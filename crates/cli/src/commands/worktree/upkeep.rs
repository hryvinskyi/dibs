//! `dibs worktree upkeep`, run by a schedule every few minutes: keeps a checkout's pool of
//! worktrees alive and its idle environments stopped. In a checkout without a pool it does nothing
//! and succeeds.
//!
//! It sends the signs of life left in the pool's worktrees, then asks the server which worktrees
//! of the pool it holds: the server ends holds on its own (a holder silent too long, a session that
//! ended) without telling this machine. A record of a holder the server no longer holds, written
//! before the server was asked, is forgotten. A worktree neither the server nor a record of this
//! machine holds is free, and the time it was first seen free is written down — unless its last
//! sign of life is younger than an agent's silence: an agent the host may still continue left it,
//! and writing it free would hand that agent's tree to the next take. Without the server, only a
//! worktree released on this machine counts as free.
//!
//! A free, prepared worktree free for `idle_after` or longer, with no sign of life since it was
//! freed and not stopped since, has its environment stopped by the project's `idle` command, under
//! the worktree's lock: a worktree whose lock another dibs holds (a take resetting it, a
//! preparation) is left alone until the next run. Its stop is written down, so it runs once until
//! the worktree is taken again. Directories in the pool's directory beyond its count are named,
//! never stopped or removed.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::Duration as StdDuration;

use serde::Serialize;
use time::{Duration, OffsetDateTime};

use dibs_agent_host::ports::Clock;
use dibs_client::ClientError;
use dibs_client::locations::Locations;
use dibs_client::state::WorktreeHoldRecord;
use dibs_domain::worktree::WorktreeName;

use crate::commands::worktree::beats::{Beats, Sent};
use crate::commands::worktree::pool::Pool;
use crate::commands::worktree::project::{ProjectCommands, ShellCommands, Site, Step};
use crate::commands::worktree::{clock, locate, machine};
use crate::context::Context;
use crate::failure::{Failure, Kind};
use crate::guard::SystemClock;

/// What one run did, as `--json` prints it.
#[derive(Debug, Default, Serialize)]
pub struct Upkept {
    /// Whether the server's holds were read; when not, only this machine's records counted.
    pub server: bool,
    /// What the sending of the signs of life did.
    pub beats: Sent,
    /// The worktrees whose holder the server let go of, their record forgotten here.
    pub let_go: Vec<WorktreeName>,
    /// The worktrees first seen free in this run.
    pub freed: Vec<WorktreeName>,
    /// The worktrees whose environment was stopped.
    pub stopped: Vec<WorktreeName>,
    /// The worktrees due to be stopped that another dibs was preparing, resetting or stopping.
    pub busy: Vec<WorktreeName>,
    /// The worktrees whose stop failed, each with why.
    pub failed: Vec<StopFailed>,
    /// The directories in the pool's directory beyond its count.
    pub beyond: Vec<String>,
}

/// A stop that failed.
#[derive(Debug, Serialize)]
pub struct StopFailed {
    /// The worktree.
    pub worktree: WorktreeName,
    /// Why, as the end of a sentence about it.
    pub reason: String,
}

/// What the server holds of the pool, and when it was asked.
struct ServerHolds {
    held: BTreeSet<WorktreeName>,
    asked: OffsetDateTime,
}

/// Runs `dibs worktree upkeep`.
///
/// # Errors
/// A configuration [`Failure`] outside a checkout or without a token; a refused one when the
/// environment of a worktree could not be stopped.
pub async fn upkeep(ctx: &Context) -> Result<(), Failure> {
    let (pool, _, locations) = match locate(ctx) {
        Ok(found) => found,
        Err(failure) if failure.code == "no_worktree_pool" => {
            ctx.printer.show(&Upkept::default(), || {
                format!("{} Nothing to keep.", failure.message)
            });
            return Ok(());
        }
        Err(failure) => return Err(failure),
    };
    let clock = SystemClock;
    let mut upkept = Upkept::default();
    let holds = server_holds(ctx, &pool, &locations, &clock, &mut upkept).await?;
    upkept.server = holds.is_some();
    let keeper = Keeper {
        pool: &pool,
        commands: &ShellCommands,
        clock: &clock,
    };
    keeper.keep(holds.as_ref(), &mut upkept).await;
    upkept.beyond = pool.beyond_count();
    ctx.printer.show(&upkept, || upkept_text(&pool, &upkept));
    if upkept.failed.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = upkept
        .failed
        .iter()
        .map(|failed| failed.worktree.as_str())
        .collect();
    Err(Failure::refused(
        "worktree_idle_failed",
        format!(
            "The environment of {} could not be stopped; the output above says why.",
            names.join(", ")
        ),
    )
    .after_answer())
}

/// Sends the signs of life and reads which worktrees of the pool the server holds; `None` when
/// the server cannot be reached or serves no worktree pools, which the person is told.
async fn server_holds(
    ctx: &Context,
    pool: &Pool,
    locations: &Locations,
    clock: &dyn Clock,
    upkept: &mut Upkept,
) -> Result<Option<ServerHolds>, Failure> {
    let local = "Only the worktrees released on this machine count as free.";
    let connection = match ctx.connect().await {
        Ok(connection) => connection,
        Err(failure) if failure.kind == Kind::Unreachable => {
            ctx.printer.warn(&format!("{} {local}", failure.message));
            return Ok(None);
        }
        Err(failure) => return Err(failure),
    };
    let machine = machine(locations)?;
    let session = match connection.session().await {
        Ok(session) => session,
        Err(failure) if failure.kind == Kind::Unreachable => {
            ctx.printer.warn(&format!("{} {local}", failure.message));
            return Ok(None);
        }
        Err(failure) => return Err(failure),
    };
    let beats = Beats {
        client: &connection.client,
        state: pool.state(),
        sender: session,
        machine: &machine,
    };
    match beats.send(clock.now()).await {
        Ok(sent) => upkept.beats = sent,
        Err(error) if error.is_unavailable() => {
            let failure = connection.failed(error);
            ctx.printer.warn(&format!("{} {local}", failure.message));
            return Ok(None);
        }
        Err(error) => return Err(connection.failed(error)),
    }
    let asked = clock.now();
    match connection.client.worktrees(Some(&machine)).await {
        Ok(answer) => Ok(Some(ServerHolds {
            held: answer
                .pools
                .into_iter()
                .filter(|own| own.pool == *pool.id())
                .flat_map(|own| own.holds)
                .map(|hold| hold.worktree)
                .collect(),
            asked,
        })),
        Err(ClientError::ServerTooOld { .. }) => {
            ctx.printer
                .note(&format!("The server keeps no worktree holds yet. {local}"));
            Ok(None)
        }
        Err(error) if error.is_unavailable() => {
            let failure = connection.failed(error);
            ctx.printer.warn(&format!("{} {local}", failure.message));
            Ok(None)
        }
        Err(error) => Err(connection.failed(error)),
    }
}

/// What the local half of a run works with.
struct Keeper<'a, P: ProjectCommands> {
    pool: &'a Pool,
    commands: &'a P,
    clock: &'a dyn Clock,
}

impl<P: ProjectCommands> Keeper<'_, P> {
    /// Writes down which worktrees are free and stops the environments due to be stopped.
    async fn keep(&self, holds: Option<&ServerHolds>, upkept: &mut Upkept) {
        let files = self.pool.files();
        for name in self.pool.names() {
            let mut record = files.hold(&name);
            if let (Some(held), Some(holds)) = (&record, holds)
                && let_go(held, holds.held.contains(&name), holds.asked)
            {
                if let Err(error) = files.forget_hold(&name) {
                    tracing::debug!(error = %error, "a worktree's holder could not be forgotten");
                }
                upkept.let_go.push(name.clone());
                record = None;
            }
            let free = record.is_none() && holds.is_none_or(|holds| !holds.held.contains(&name));
            if !free {
                continue;
            }
            let free_since = match (files.free_since(&name), holds) {
                (Some(since), _) => since,
                (None, Some(_)) if self.pool.in_use(&name, self.clock.now()) => continue,
                (None, Some(_)) => {
                    let now = self.clock.now();
                    if let Err(error) = files.mark_free(&name, now) {
                        tracing::debug!(error = %error, "a free worktree could not be written down");
                    }
                    upkept.freed.push(name.clone());
                    now
                }
                (None, None) => continue,
            };
            let now = self.clock.now();
            let settings = self.pool.settings();
            let Some(idle) = &settings.idle else {
                continue;
            };
            if !self.pool.is_prepared(&name)
                || self.pool.in_use(&name, now)
                || !due_to_stop(
                    free_since,
                    files.stopped_at(&name),
                    settings.idle_after,
                    now,
                )
            {
                continue;
            }
            self.stop(&name, idle, free_since, upkept).await;
        }
    }

    /// Stops the environment of the free worktree `name` under its lock, unless another dibs holds
    /// the lock or the worktree was taken meanwhile.
    async fn stop(
        &self,
        name: &WorktreeName,
        idle: &str,
        free_since: OffsetDateTime,
        upkept: &mut Upkept,
    ) {
        let files = self.pool.files();
        let lock = match self.pool.lock(name, StdDuration::ZERO).await {
            Ok(lock) => lock,
            Err(error) => {
                tracing::debug!(error = %error, "a worktree's lock could not be taken");
                upkept.failed.push(StopFailed {
                    worktree: name.clone(),
                    reason: "its lock could not be taken on this machine".to_owned(),
                });
                return;
            }
        };
        let Some(_lock) = lock else {
            upkept.busy.push(name.clone());
            return;
        };
        if files.hold(name).is_some() || files.free_since(name) != Some(free_since) {
            return;
        }
        let path = self.pool.path(name);
        let settings = self.pool.settings();
        let site = Site {
            name,
            path: &path,
            base: &settings.base,
            checkout: self.pool.checkout().root(),
        };
        match self
            .commands
            .run(Step::Idle, idle, &site, Step::Idle.limit())
        {
            Ok(()) => {
                if let Err(error) = files.mark_stopped(name, self.clock.now()) {
                    tracing::debug!(error = %error, "a worktree's stop could not be written down");
                }
                upkept.stopped.push(name.clone());
            }
            Err(failed) => upkept.failed.push(StopFailed {
                worktree: name.clone(),
                reason: failed.reason(),
            }),
        }
    }
}

/// Whether this machine's record of a holder is of a hold the server let go of: the server, asked
/// at `asked`, holds nothing on the worktree, and the record was written before it was asked (a
/// record written later is of a take the answer could not know of yet).
pub fn let_go(record: &WorktreeHoldRecord, held_by_server: bool, asked: OffsetDateTime) -> bool {
    !held_by_server && record.taken_at < asked
}

/// Whether the environment of a worktree free since `free_since` is due to be stopped at `now`:
/// free for `idle_after` or longer, and not stopped since it was freed.
pub fn due_to_stop(
    free_since: OffsetDateTime,
    stopped_at: Option<OffsetDateTime>,
    idle_after: Duration,
    now: OffsetDateTime,
) -> bool {
    now - free_since >= idle_after && stopped_at.is_none_or(|stopped| stopped <= free_since)
}

fn upkept_text(pool: &Pool, upkept: &Upkept) -> String {
    let mut text = String::new();
    let mut line = |sentence: String| {
        let _ = writeln!(text, "{sentence}");
    };
    if upkept.beats.beats > 0 {
        line(format!(
            "Sent {} signs of life from the pool's worktrees.",
            upkept.beats.beats
        ));
    }
    for name in &upkept.beats.retaken {
        line(format!(
            "{name} was let go by the server while its holder worked in it; it is held for that holder again."
        ));
    }
    for name in &upkept.beats.forgotten {
        line(format!(
            "{name} is held by another holder now; this machine forgot its earlier holder."
        ));
    }
    for name in &upkept.let_go {
        line(format!(
            "{name} was let go by the server after its holder fell silent."
        ));
    }
    let files = pool.files();
    for name in &upkept.stopped {
        let since = files
            .free_since(name)
            .map_or_else(String::new, |at| format!(", free since {}", clock(at)));
        line(format!("Stopped the environment of {name}{since}."));
    }
    for name in &upkept.busy {
        line(format!(
            "{name} is being prepared, reset or stopped by another dibs; its environment is left for the next run."
        ));
    }
    for failed in &upkept.failed {
        line(format!(
            "The environment of {} could not be stopped: {}.",
            failed.worktree, failed.reason
        ));
    }
    for directory in &upkept.beyond {
        line(format!(
            "{directory} lies in the pool's directory beyond its {} worktrees; dibs leaves it as it is.",
            pool.settings().count
        ));
    }
    if text.is_empty() {
        text.push_str("Nothing was due.\n");
    }
    text.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use dibs_domain::ids::SessionId;

    use super::*;

    fn at(minutes: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000) + Duration::minutes(minutes)
    }

    fn record(taken_at: OffsetDateTime) -> WorktreeHoldRecord {
        WorktreeHoldRecord {
            session_id: SessionId(4),
            agent: None,
            purpose: None,
            item: None,
            taken_at,
            reset_done: true,
        }
    }

    #[test]
    fn a_record_is_let_go_only_when_the_server_holds_nothing_and_it_came_before_the_question() {
        assert!(let_go(&record(at(0)), false, at(1)));
        assert!(!let_go(&record(at(0)), true, at(1)), "the server holds it");
        assert!(
            !let_go(&record(at(2)), false, at(1)),
            "a take the answer could not know of yet"
        );
    }

    #[test]
    fn a_free_worktree_is_stopped_once_after_idle_after_until_it_is_freed_again() {
        let idle_after = Duration::minutes(30);
        assert!(!due_to_stop(at(0), None, idle_after, at(29)));
        assert!(due_to_stop(at(0), None, idle_after, at(30)));
        assert!(
            !due_to_stop(at(0), Some(at(30)), idle_after, at(90)),
            "stopped since it was freed"
        );
        assert!(
            due_to_stop(at(100), Some(at(30)), idle_after, at(130)),
            "freed again after the last stop"
        );
    }
}
