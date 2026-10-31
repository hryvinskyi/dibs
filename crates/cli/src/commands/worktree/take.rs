//! Taking a worktree of a pool for one holder — a session, or one agent of a session — and making
//! it ready to work in.
//!
//! The take offers the server the worktrees this machine can hand out, best first, and asks again
//! every few seconds while every one is held, as long as the caller waits. Once the server hands one
//! over, the holder is recorded on this machine before anything touches the tree, and under the
//! worktree's lock its work is rescued and it is reset — or prepared, when it was never prepared —
//! then its own `.dibs.toml` is written and the record says the reset finished.
//!
//! The same holder asking again (a retried hook, a worktree entered once more) gets its worktree
//! back untouched; only a reset that the record says never finished is resumed, rescue first. A
//! worktree that fails anywhere after the server handed it over is set aside as broken, its hold
//! released, and the next one is tried; the take fails only when none is left.
//!
//! A take under a deadline finishes within it, and never prepares a worktree itself, as a first
//! preparation does not fit. When no prepared worktree is free while the pool still has worktrees
//! nobody works in that are not prepared yet, it starts the preparation of one in the background —
//! unless a worktree of the pool is being prepared, reset or stopped already — and answers at once
//! that the pool is still being prepared. When it has nothing to offer the server at all, every
//! worktree being set aside as broken or in use on this machine, it answers at once that the pool
//! cannot serve, naming the broken ones and how to take them back. It answers busy only when the
//! server named every worktree it offered as held and it waited for one until the deadline less a
//! reset's time. A reset the deadline cut short does not set its worktree aside: the worktree is
//! not at fault. Under a deadline the project's commands write their output to the worktree's log
//! in this machine's state, never to the caller's pipes, which a process they leave running would
//! hold open past the deadline.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use time::OffsetDateTime;

use dibs_agent_host::ports::Clock;
use dibs_client::api::{
    OwnWorktreeHold, ReleaseWorktreeRequest, TakeWorktreeRequest, WorktreeOutcome, WorktreeRelease,
};
use dibs_client::state::{BrokenWorktree, WorktreeHoldRecord};
use dibs_client::{Client, ClientError};
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::report::MachineId;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{AgentKey, WorktreeName};

use crate::commands::worktree::BACKGROUND_PREPARE;
use crate::commands::worktree::beats;
use crate::commands::worktree::pool::Pool;
use crate::commands::worktree::project::{Output, ProjectCommands, Site, Step, StepFailed};
use crate::commands::worktree::rescue::{Author, WorktreeGit};
use crate::commands::worktree::select::{self, Facts};
use crate::failure::sentence;

/// What a take asks for.
#[derive(Debug, Clone)]
pub struct Ask {
    /// The holder's session.
    pub session: SessionId,
    /// The holder's agent; `None` for the session itself.
    pub agent: Option<AgentKey>,
    /// Why.
    pub purpose: Purpose,
    /// The work item, by local id.
    pub item: Option<ItemId>,
    /// How long to wait for a free worktree.
    pub wait: Duration,
    /// When the whole take must be over, counted from its start.
    pub deadline: Option<Duration>,
    /// How often a waiting take asks again.
    pub poll_every: Duration,
}

/// A worktree made ready for its holder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TakenWorktree {
    /// The worktree.
    pub worktree: WorktreeName,
    /// Its directory.
    pub path: PathBuf,
    /// The holder's session.
    pub session_id: SessionId,
    /// The holder's agent; `None` for the session itself.
    pub agent: Option<AgentKey>,
    /// Whether the holder held it already and got it back untouched.
    pub again: bool,
    /// Whether it was prepared for this take, rather than reset.
    pub prepared: bool,
    /// The branch its earlier work was rescued on, when it held any.
    pub rescued: Option<String>,
}

/// What a take came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Taken {
    /// The holder holds this worktree, ready to work in.
    Took(TakenWorktree),
    /// Every worktree that could be handed out stayed held for the time given.
    Busy {
        /// Who holds them, as a sentence.
        sentence: String,
        /// The holds of the worktrees offered, as the server named them.
        held: Vec<OwnWorktreeHold>,
    },
    /// A take under a deadline found no prepared worktree free while the pool still has worktrees
    /// nobody works in that are not prepared yet: the pool cannot serve one in time, though not
    /// every worktree is held.
    Preparing {
        /// Which worktrees are being prepared, or wait to be, as a sentence.
        sentence: String,
        /// The worktree whose preparation the take started in the background.
        preparing: Option<WorktreeName>,
    },
}

/// Why a take ended without an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeError {
    /// The server refused or gave no decision.
    Server(ClientError),
    /// Every worktree the server handed over failed to become ready; the sentence says why.
    Unusable(String),
}

/// Why one worktree handed over did not become ready.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NotReady {
    reason: String,
    /// Whether it is set aside until it is prepared again.
    broken: bool,
    /// Whether it only waits to be prepared, which the take starts in the background once its
    /// hold is let go of, so that the preparation never finds it held.
    prepare: bool,
}

impl NotReady {
    /// The worktree failed for `reason`; `broken` sets it aside.
    fn failed(reason: String, broken: bool) -> Self {
        Self {
            reason,
            broken,
            prepare: false,
        }
    }
}

/// What a take works with.
pub struct Taker<'a, P: ProjectCommands> {
    /// The server.
    pub client: &'a Client,
    /// This machine.
    pub machine: MachineId,
    /// The pool.
    pub pool: &'a Pool,
    /// The project's commands.
    pub commands: &'a P,
    /// The time.
    pub clock: &'a dyn Clock,
    /// Who a rescue commit is made as.
    pub author: Author,
    /// Where the take says what it does along the way.
    pub notes: &'a dyn Fn(&str),
}

/// The longest a take without a deadline waits for a worktree's lock: a preparation's time and a
/// little more.
const LOCK_PATIENCE: Duration = Duration::from_secs(25 * 60);

impl<P: ProjectCommands> Taker<'_, P> {
    /// Takes a worktree for the holder `ask` names and makes it ready.
    ///
    /// # Errors
    /// [`TakeError::Server`] for the server's refusal or silence; [`TakeError::Unusable`] when
    /// every worktree handed over failed to become ready.
    pub async fn take(&self, ask: &Ask) -> Result<Taken, TakeError> {
        let started = Instant::now();
        let deadline = ask.deadline.map(|deadline| started + deadline);
        let mut wait_until = started + ask.wait;
        if let Some(deadline) = deadline {
            let room = deadline
                .checked_sub(Step::Reset.limit())
                .unwrap_or(started)
                .max(started);
            wait_until = wait_until.min(room);
        }
        let mut tried: BTreeSet<WorktreeName> = BTreeSet::new();
        let mut failures: Vec<String> = Vec::new();
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                if failures.is_empty() {
                    return Err(TakeError::Unusable(
                        "No worktree could be made ready in the time given.".to_owned(),
                    ));
                }
                return Err(TakeError::Unusable(format!(
                    "No worktree of this pool could be made ready in the time given: {}.",
                    failures.join("; ")
                )));
            }
            let facts = self.facts(ask, &tried);
            let names = select::candidates(&facts, deadline.is_none());
            let offered = !names.is_empty();
            let mut held = Vec::new();
            if offered {
                let took = self
                    .client
                    .take_worktree(&self.request(ask, names))
                    .await
                    .map_err(TakeError::Server)?;
                beats::served(self.pool.state(), self.clock.now());
                match (took.outcome, took.worktree) {
                    (WorktreeOutcome::Took, Some(name)) => {
                        match self.ready(ask, &name, took.again, deadline).await {
                            Ok(taken) => return Ok(Taken::Took(taken)),
                            Err(not_ready) => {
                                self.set_aside(ask, &name, &not_ready).await;
                                if not_ready.prepare {
                                    continue;
                                }
                                failures.push(format!("{name}: {}", not_ready.reason));
                                tried.insert(name);
                                continue;
                            }
                        }
                    }
                    _ => held = took.held,
                }
            }
            if !offered && !failures.is_empty() {
                return Err(TakeError::Unusable(format!(
                    "No worktree of this pool could be made ready: {}.",
                    failures.join("; ")
                )));
            }
            if deadline.is_some() {
                let unprepared = select::unprepared(&facts);
                if !unprepared.is_empty() {
                    let preparing = self.prepare_in_background(&facts);
                    return Ok(Taken::Preparing {
                        sentence: preparing_sentence(&unprepared, preparing.as_ref()),
                        preparing,
                    });
                }
                if !offered {
                    return Err(TakeError::Unusable(self.unservable_sentence(&facts)));
                }
            }
            let left = wait_until.saturating_duration_since(Instant::now());
            if !left.is_zero() {
                tokio::time::sleep(left.min(ask.poll_every)).await;
                continue;
            }
            return Ok(Taken::Busy {
                sentence: busy_sentence(&held, &facts),
                held,
            });
        }
    }

    /// What this machine knows of each worktree, those already tried in this take left out.
    fn facts(&self, ask: &Ask, tried: &BTreeSet<WorktreeName>) -> Vec<Facts> {
        let now = self.clock.now();
        self.pool
            .names()
            .iter()
            .filter(|name| !tried.contains(*name))
            .map(|name| self.pool.facts(name, ask.session, ask.agent.as_ref(), now))
            .collect()
    }

    fn request(&self, ask: &Ask, names: Vec<WorktreeName>) -> TakeWorktreeRequest {
        TakeWorktreeRequest {
            session_id: ask.session,
            machine: self.machine.clone(),
            pool: self.pool.id().clone(),
            names,
            agent: ask.agent.clone(),
            purpose: ask.purpose.clone(),
            item: ask.item,
        }
    }

    /// Makes the worktree the server handed over ready for the holder.
    async fn ready(
        &self,
        ask: &Ask,
        name: &WorktreeName,
        again: bool,
        deadline: Option<Instant>,
    ) -> Result<TakenWorktree, NotReady> {
        let files = self.pool.files();
        let path = self.pool.path(name);
        let record = files
            .hold(name)
            .filter(|record| record.is_of(ask.session, ask.agent.as_ref()));
        let prepared = self.pool.is_prepared(name);
        let taken = |prepared_now: bool, rescued: Option<String>| TakenWorktree {
            worktree: name.clone(),
            path: path.clone(),
            session_id: ask.session,
            agent: ask.agent.clone(),
            again,
            prepared: prepared_now,
            rescued,
        };
        if again && prepared && record.as_ref().is_none_or(|record| record.reset_done) {
            if record.is_none() {
                self.remember(name, ask, self.clock.now(), true);
            }
            return Ok(taken(false, None));
        }
        if deadline.is_some() && !prepared {
            return Err(NotReady {
                reason: "it is not prepared yet, and a preparation does not fit in the time left"
                    .to_owned(),
                broken: false,
                prepare: true,
            });
        }
        let taken_at = record
            .as_ref()
            .map_or_else(|| self.clock.now(), |record| record.taken_at);
        self.remember(name, ask, taken_at, false);
        if let Err(error) = files.mark_taken(name) {
            tracing::debug!(error = %error, "the free and stopped marks could not be cleared");
        }
        let patience = deadline.map_or(LOCK_PATIENCE, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        });
        let lock = self.pool.lock(name, patience).await.map_err(|error| {
            tracing::debug!(error = %error, "a worktree's lock could not be taken");
            NotReady::failed(
                "its lock could not be taken on this machine".to_owned(),
                false,
            )
        })?;
        let Some(_lock) = lock else {
            return Err(NotReady::failed(
                "another dibs is still preparing, resetting or stopping it".to_owned(),
                false,
            ));
        };
        let rescued = self.rescue(name)?;
        let step = if prepared { Step::Reset } else { Step::Prepare };
        let settings = self.pool.settings();
        let command = if prepared {
            &settings.reset
        } else {
            &settings.prepare
        };
        let limit = deadline.map_or(step.limit(), |deadline| {
            step.limit()
                .min(deadline.saturating_duration_since(Instant::now()))
        });
        let site = Site {
            name,
            path: &path,
            base: &settings.base,
            checkout: self.pool.checkout().root(),
        };
        let log = self.pool.log(name);
        let output = if deadline.is_some() {
            Output::Log(&log)
        } else {
            Output::Stderr
        };
        self.commands
            .run_to(step, command, &site, limit, output)
            .map_err(|failed| {
                let reason = match output {
                    Output::Log(log) => {
                        format!("{}; its output is in {}", failed.reason(), log.display())
                    }
                    Output::Stderr => failed.reason(),
                };
                let cut_short =
                    limit < step.limit() && matches!(failed, StepFailed::TimedOut { .. });
                NotReady::failed(reason, !cut_short)
            })?;
        self.pool.write_config(name).map_err(|error| {
            tracing::debug!(error = %error, "a worktree's .dibs.toml could not be written");
            NotReady::failed(error.reason(), true)
        })?;
        self.remember(name, ask, taken_at, true);
        Ok(taken(!prepared, rescued))
    }

    /// Saves the worktree's work on a rescue branch when it holds work nothing else keeps; leaves
    /// alone a worktree that is not a git worktree yet.
    fn rescue(&self, name: &WorktreeName) -> Result<Option<String>, NotReady> {
        if !self.pool.exists(name) {
            return Ok(None);
        }
        let git = WorktreeGit::new(&self.pool.path(name));
        let broken = |reason: String| NotReady::failed(reason, true);
        if let Some(operation) = git.in_progress().map_err(|error| broken(error.reason()))? {
            return Err(broken(format!(
                "{operation} is in progress in it; finish or abort it, then run dibs worktree prepare {name}"
            )));
        }
        if !git.needs_rescue().map_err(|error| broken(error.reason()))? {
            return Ok(None);
        }
        let author = Author {
            name: self.author.name.clone(),
            email: git.email().unwrap_or_else(|| self.author.email.clone()),
        };
        let rescue = git
            .rescue(name, self.clock.now(), &author)
            .map_err(|error| broken(error.reason()))?;
        let mut notice = format!(
            "{name} held work nothing else kept; it is saved on the branch {} before the worktree is reused.",
            rescue.branch
        );
        if let Some(untracked) = rescue.untracked_sentence() {
            notice.push(' ');
            notice.push_str(&untracked);
        }
        (self.notes)(&notice);
        Ok(Some(rescue.branch))
    }

    fn remember(&self, name: &WorktreeName, ask: &Ask, taken_at: OffsetDateTime, done: bool) {
        let record = WorktreeHoldRecord {
            session_id: ask.session,
            agent: ask.agent.clone(),
            purpose: Some(ask.purpose.clone()),
            item: ask.item,
            taken_at,
            reset_done: done,
        };
        if let Err(error) = self.pool.files().remember_hold(name, &record) {
            tracing::debug!(error = %error, "a worktree's holder could not be recorded");
        }
    }

    /// Releases the hold of a worktree that did not become ready, sets it aside when it is broken,
    /// and says so.
    async fn set_aside(&self, ask: &Ask, name: &WorktreeName, not_ready: &NotReady) {
        let files = self.pool.files();
        if not_ready.broken {
            let broken = BrokenWorktree {
                at: self.clock.now(),
                reason: sentence(&not_ready.reason),
            };
            if let Err(error) = files.mark_broken(name, &broken) {
                tracing::debug!(error = %error, "a worktree could not be set aside");
            }
        }
        let release = ReleaseWorktreeRequest {
            session_id: ask.session,
            machine: self.machine.clone(),
            pool: self.pool.id().clone(),
            release: WorktreeRelease::Holder {
                holder_session: ask.session,
                agent: ask.agent.clone(),
            },
        };
        if let Err(error) = self.client.release_worktree(&release).await {
            tracing::debug!(
                code = error.code(),
                "a worktree that did not become ready kept its hold"
            );
        }
        if let Err(error) = files.forget_hold(name) {
            tracing::debug!(error = %error, "a worktree's holder could not be forgotten");
        }
        let now = self.clock.now();
        if let Err(error) = files.mark_free(name, now) {
            tracing::debug!(error = %error, "a worktree's release could not be recorded");
        }
        let aside = if not_ready.broken {
            format!(" It is set aside until dibs worktree prepare {name} succeeds.")
        } else {
            String::new()
        };
        (self.notes)(&format!(
            "{name} could not be used: {}.{aside}",
            not_ready.reason.trim_end_matches('.')
        ));
    }

    /// What a take under a deadline says when it had nothing to offer the server: which worktrees
    /// are set aside as broken, why, and how each is taken back, and which are in use here.
    fn unservable_sentence(&self, facts: &[Facts]) -> String {
        let files = self.pool.files();
        let broken: Vec<(&WorktreeName, Option<String>)> = facts
            .iter()
            .filter(|facts| facts.broken)
            .map(|facts| {
                let reason = files.broken(&facts.name).map(|broken| broken.reason);
                (&facts.name, reason)
            })
            .collect();
        let in_use: Vec<&WorktreeName> = facts
            .iter()
            .filter(|facts| facts.in_use && !facts.broken)
            .map(|facts| &facts.name)
            .collect();
        unservable(&broken, &in_use)
    }

    /// Starts `dibs worktree prepare` in the background for the first worktree that needs it,
    /// unless a worktree of the pool is locked; answers it when one was started.
    fn prepare_in_background(&self, facts: &[Facts]) -> Option<WorktreeName> {
        let name = select::to_prepare(facts)?;
        match spawn_prepare(self.pool.checkout().root(), &name) {
            Ok(()) => {
                (self.notes)(&format!(
                    "{name} is not prepared yet; dibs started preparing it in the background."
                ));
                Some(name)
            }
            Err(error) => {
                tracing::debug!(error = %error, "a background preparation could not start");
                None
            }
        }
    }
}

/// Starts `dibs worktree prepare <name>` in the checkout, in a process of its own that outlives
/// this one, with nothing of this one's open.
fn spawn_prepare(checkout: &std::path::Path, name: &WorktreeName) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .args(["worktree", "prepare", name.as_str()])
        .env(BACKGROUND_PREPARE, "1")
        .current_dir(checkout)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::guard::upkeep::detach(&mut command);
    command.spawn().map(|_| ())
}

/// What a take that had nothing to offer says: the `broken` worktrees, with why each was set aside
/// when that is known, and the ones `in_use` on this machine.
fn unservable(broken: &[(&WorktreeName, Option<String>)], in_use: &[&WorktreeName]) -> String {
    let mut parts = Vec::new();
    for (name, reason) in broken {
        let why = reason
            .as_deref()
            .map(|reason| format!(" ({})", clause(reason)))
            .unwrap_or_default();
        parts.push(format!(
            "{name} is set aside as broken{why}, and dibs worktree prepare {name} takes it back"
        ));
    }
    if !in_use.is_empty() {
        let names: Vec<&str> = in_use.iter().map(|name| name.as_str()).collect();
        let verb = if names.len() == 1 { "is" } else { "are" };
        parts.push(format!(
            "{} {verb} still worked in on this machine",
            names.join(", ")
        ));
    }
    if parts.is_empty() {
        return "No worktree of this pool can be handed out now.".to_owned();
    }
    format!(
        "No worktree of this pool can be handed out now: {}.",
        parts.join("; ")
    )
}

/// A sentence as a clause inside another: its first letter in lower case, its full stop dropped.
fn clause(sentence: &str) -> String {
    let trimmed = sentence.trim().trim_end_matches('.');
    let mut chars = trimmed.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// What a busy take says: who holds each worktree offered, and which are set aside.
fn busy_sentence(held: &[OwnWorktreeHold], facts: &[Facts]) -> String {
    let mut parts = Vec::new();
    if held.is_empty() {
        parts.push("No worktree of this pool is free to hand out.".to_owned());
    } else {
        let holds: Vec<String> = held
            .iter()
            .map(|hold| {
                let who = hold.agent.as_ref().map_or_else(
                    || format!("the session {}", hold.session),
                    |agent| format!("the agent {agent} of {}", hold.session),
                );
                format!("{} by {who} ({})", hold.worktree, hold.purpose)
            })
            .collect();
        parts.push(format!(
            "Every worktree this machine could hand out is held: {}.",
            holds.join("; ")
        ));
    }
    let broken: Vec<&str> = facts
        .iter()
        .filter(|facts| facts.broken)
        .map(|facts| facts.name.as_str())
        .collect();
    if !broken.is_empty() {
        parts.push(format!(
            "Set aside as broken: {}; dibs worktree prepare <name> takes one back.",
            broken.join(", ")
        ));
    }
    parts.join(" ")
}

/// What a take says of a pool still being prepared: which of its `unprepared` worktrees are being
/// prepared — by another run, or by the one this take started, `preparing` — or, when none is,
/// which wait for a preparation that could not be started.
fn preparing_sentence(unprepared: &[&Facts], preparing: Option<&WorktreeName>) -> String {
    let (being, waiting): (Vec<&Facts>, Vec<&Facts>) = unprepared
        .iter()
        .partition(|facts| facts.locked || Some(&facts.name) == preparing);
    let listed = |list: &[&Facts]| -> (String, &'static str) {
        let names: Vec<&str> = list.iter().map(|facts| facts.name.as_str()).collect();
        let verb = if names.len() == 1 { "is" } else { "are" };
        (names.join(", "), verb)
    };
    let said = if being.is_empty() {
        let (names, verb) = listed(&waiting);
        format!("{names} {verb} not prepared yet; dibs worktree prepare <name> prepares one.")
    } else {
        let (names, verb) = listed(&being);
        format!("{names} {verb} being prepared and can be taken once ready.")
    };
    format!("No prepared worktree of this pool is free yet. {said}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unprepared(name: &str, locked: bool) -> Facts {
        Facts {
            name: WorktreeName::parse(name).unwrap(),
            prepared: false,
            stopped: false,
            broken: false,
            in_use: false,
            locked,
        }
    }

    #[test]
    fn a_pool_with_nothing_to_offer_names_the_broken_worktrees_and_their_repair() {
        let first = WorktreeName::parse("worktree1").unwrap();
        let second = WorktreeName::parse("worktree2").unwrap();
        assert_eq!(
            unservable(
                &[(
                    &first,
                    Some("Its prepare command failed with status 5.".to_owned())
                )],
                &[]
            ),
            "No worktree of this pool can be handed out now: worktree1 is set aside as broken (its prepare command failed with status 5), and dibs worktree prepare worktree1 takes it back."
        );
        assert_eq!(
            unservable(&[(&second, None)], &[&first]),
            "No worktree of this pool can be handed out now: worktree2 is set aside as broken, and dibs worktree prepare worktree2 takes it back; worktree1 is still worked in on this machine."
        );
    }

    #[test]
    fn a_pool_being_prepared_names_what_is_prepared_and_what_waits() {
        let started = unprepared("worktree1", false);
        let other = unprepared("worktree2", true);
        let idle = unprepared("worktree3", false);
        assert_eq!(
            preparing_sentence(&[&started], Some(&started.name)),
            "No prepared worktree of this pool is free yet. worktree1 is being prepared and can be taken once ready."
        );
        assert_eq!(
            preparing_sentence(&[&started, &other, &idle], Some(&started.name)),
            "No prepared worktree of this pool is free yet. worktree1, worktree2 are being prepared and can be taken once ready.",
            "a worktree a later take prepares on demand is not named"
        );
        assert_eq!(
            preparing_sentence(&[&started, &idle], None),
            "No prepared worktree of this pool is free yet. worktree1, worktree3 are not prepared yet; dibs worktree prepare <name> prepares one.",
            "a preparation that could not start is not said to run"
        );
    }
}
