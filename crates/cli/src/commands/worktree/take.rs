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
//! A take under a deadline finishes within it: it waits for a free worktree only as long as leaves
//! a reset its time, and never prepares one itself, as a first preparation does not fit; when no
//! prepared worktree is free, it starts the preparation of one in the background and answers busy.
//! A reset the deadline cut short does not set its worktree aside: the worktree is not at fault.

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

use crate::commands::worktree::pool::Pool;
use crate::commands::worktree::project::{ProjectCommands, Site, Step, StepFailed};
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
    /// No worktree could be handed out in the time given.
    Busy {
        /// What stands in the way, as a sentence.
        sentence: String,
        /// The holds of the worktrees offered, as the server named them.
        held: Vec<OwnWorktreeHold>,
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
        let mut preparing: Option<WorktreeName> = None;
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                if failures.is_empty() {
                    return Ok(Taken::Busy {
                        sentence: "No worktree could be made ready in the time given.".to_owned(),
                        held: Vec::new(),
                        preparing,
                    });
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
                match (took.outcome, took.worktree) {
                    (WorktreeOutcome::Took, Some(name)) => {
                        match self.ready(ask, &name, took.again, deadline).await {
                            Ok(taken) => return Ok(Taken::Took(taken)),
                            Err(not_ready) => {
                                self.set_aside(ask, &name, &not_ready).await;
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
            if deadline.is_some() && preparing.is_none() {
                preparing = self.prepare_in_background(&facts);
            }
            let left = wait_until.saturating_duration_since(Instant::now());
            if !left.is_zero() {
                tokio::time::sleep(left.min(ask.poll_every)).await;
                continue;
            }
            return Ok(Taken::Busy {
                sentence: busy_sentence(&held, &facts, preparing.as_ref()),
                held,
                preparing,
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
        let taken_at = record
            .as_ref()
            .map_or_else(|| self.clock.now(), |record| record.taken_at);
        self.remember(name, ask, taken_at, false);
        if let Err(error) = files.mark_taken(name) {
            tracing::debug!(error = %error, "the free and stopped marks could not be cleared");
        }
        if deadline.is_some() && !prepared {
            self.prepare_in_background(&[self.pool.facts(
                name,
                ask.session,
                ask.agent.as_ref(),
                self.clock.now(),
            )]);
            return Err(NotReady {
                reason: "it is not prepared yet, and a preparation does not fit in the time left"
                    .to_owned(),
                broken: false,
            });
        }
        let patience = deadline.map_or(LOCK_PATIENCE, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        });
        let lock = self.pool.lock(name, patience).await.map_err(|error| {
            tracing::debug!(error = %error, "a worktree's lock could not be taken");
            NotReady {
                reason: "its lock could not be taken on this machine".to_owned(),
                broken: false,
            }
        })?;
        let Some(_lock) = lock else {
            return Err(NotReady {
                reason: "another dibs is still preparing, resetting or stopping it".to_owned(),
                broken: false,
            });
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
        self.commands
            .run(step, command, &site, limit)
            .map_err(|failed| NotReady {
                reason: failed.reason(),
                broken: !(limit < step.limit() && matches!(failed, StepFailed::TimedOut { .. })),
            })?;
        self.pool.write_config(name).map_err(|error| {
            tracing::debug!(error = %error, "a worktree's .dibs.toml could not be written");
            NotReady {
                reason: "its .dibs.toml could not be written".to_owned(),
                broken: true,
            }
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
        let broken = |reason: String| NotReady {
            reason,
            broken: true,
        };
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
        let branch = git
            .rescue(name, self.clock.now(), &author)
            .map_err(|error| broken(error.reason()))?;
        (self.notes)(&format!(
            "{name} held work nothing else kept; it is saved on the branch {branch} before the worktree is reused."
        ));
        Ok(Some(branch))
    }

    fn remember(&self, name: &WorktreeName, ask: &Ask, taken_at: OffsetDateTime, done: bool) {
        let record = WorktreeHoldRecord {
            session_id: ask.session,
            agent: ask.agent.clone(),
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

    /// Starts `dibs worktree prepare` in the background for the first worktree that needs it and
    /// that nobody prepares; answers it when one was started.
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
        .current_dir(checkout)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::guard::upkeep::detach(&mut command);
    command.spawn().map(|_| ())
}

/// What a busy take says: who holds each worktree offered, which are set aside, and which is being
/// prepared.
fn busy_sentence(
    held: &[OwnWorktreeHold],
    facts: &[Facts],
    preparing: Option<&WorktreeName>,
) -> String {
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
    if let Some(name) = preparing {
        parts.push(format!(
            "{name} is being prepared and can be taken in a few minutes."
        ));
    }
    parts.join(" ")
}
