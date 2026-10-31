//! The guard's part in a pool of worktrees: a worktree a host wants and gives back, the signs of
//! life its holders leave while they work, the holds an agent's or a session's end lets go of, and
//! the check that nobody works in a pool worktree their session does not hold.
//!
//! A worktree wanted is answered whatever else stands: switched off, outside every governed
//! checkout, in a checkout that lends no worktrees, with the server out of reach, in a pool still
//! being prepared. Every one of those makes the worktree the host would have made itself, and says
//! why; only a pool whose every worktree stayed held until the host's deadline refuses. A worktree given back is released when
//! it is a pool worktree its session holds, and never deleted; any other worktree given back is
//! removed as git removes one, never by force.

use std::path::{Path, PathBuf};

use dibs_domain::slot::split_commands;
use dibs_domain::worktree::WorktreeName;

use crate::adapter::{HostAdapter, WorktreeTerms};
use crate::event::{AgentKey, HostEvent, WorktreeLabel};
use crate::outcome::GuardOutcome;
use crate::ports::{
    Removal, Retaken, SessionRef, WorktreeHolder, WorktreePool, WorktreeRecord, WorktreeTake,
};
use crate::text::Text;
use crate::wording;

use super::{Guard, Problem, Run};

impl Guard<'_> {
    /// The answer to a worktree wanted or given back, which the guard gives switched off or not
    /// and whether or not a checkout governs the event's directory; `None` for every other event.
    pub(super) fn worktree_event(
        &self,
        adapter: &dyn HostAdapter,
        event: &HostEvent,
        switched_off: bool,
    ) -> Option<GuardOutcome> {
        let session = || SessionRef {
            host: adapter.id(),
            key: event.session_key().clone(),
        };
        match event {
            HostEvent::WorktreeWanted {
                cwd, label, agent, ..
            } => {
                let holder = WorktreeHolder {
                    session: session(),
                    agent: agent.clone(),
                };
                Some(self.wanted(adapter, &holder, cwd, label, switched_off))
            }
            HostEvent::WorktreeReturned { cwd, path, .. } => {
                Some(self.returned(adapter, &session(), cwd, path, switched_off))
            }
            HostEvent::SessionStarted { .. }
            | HostEvent::BeforeTool { .. }
            | HostEvent::AfterTool { .. }
            | HostEvent::PromptSubmitted { .. }
            | HostEvent::Stopped { .. }
            | HostEvent::AgentEnded { .. }
            | HostEvent::SessionEnded { .. } => None,
        }
    }

    /// A worktree from the pool for `holder`; the host's own worktree when the pool cannot serve
    /// one; a refusal when every worktree of the pool stayed held. Nothing for a host that does not
    /// serve worktrees.
    fn wanted(
        &self,
        adapter: &dyn HostAdapter,
        holder: &WorktreeHolder,
        cwd: &Path,
        label: &WorktreeLabel,
        switched_off: bool,
    ) -> GuardOutcome {
        let Some(terms) = served(adapter) else {
            return GuardOutcome::silent();
        };
        let unserved = if switched_off {
            Unserved::because(wording::WORKTREE_SWITCHED_OFF)
        } else {
            match self.pool_answer(cwd, holder, &terms) {
                Ok(outcome) => return outcome,
                Err(unserved) => unserved,
            }
        };
        match self.workspaces.git().make_worktree(cwd, &terms.own, label) {
            Ok(path) => GuardOutcome::Worktree {
                path,
                notice: Some(Text::new(unserved.notice)),
            },
            Err(failure) => GuardOutcome::NoWorktree {
                reason: Text::new(wording::no_own_worktree(&unserved.reason, &failure)),
            },
        }
    }

    /// The pool's answer for `holder`, or why the pool cannot serve one.
    fn pool_answer(
        &self,
        cwd: &Path,
        holder: &WorktreeHolder,
        terms: &WorktreeTerms,
    ) -> Result<GuardOutcome, Unserved> {
        let workspace = match self.workspaces.open(cwd) {
            Ok(Some(workspace)) => workspace,
            Ok(None) => return Err(Unserved::because(wording::WORKTREE_UNGOVERNED)),
            Err(error) => return Err(Unserved::because(&error.to_string())),
        };
        let Some(pool) = workspace.worktree_pool() else {
            return Err(Unserved::because(wording::WORKTREE_NO_POOL));
        };
        match pool.take(holder, terms.deadline()) {
            WorktreeTake::Took { path, notes, .. } => Ok(GuardOutcome::Worktree {
                path,
                notice: wording::took_worktree(&notes).map(Text::new),
            }),
            WorktreeTake::Busy(busy) => Ok(GuardOutcome::NoWorktree {
                reason: Text::new(wording::worktrees_busy(&busy)),
            }),
            WorktreeTake::Preparing(preparing) => Err(Unserved {
                reason: wording::WORKTREE_PREPARING.to_owned(),
                notice: wording::own_worktree_while_preparing(&preparing),
            }),
            WorktreeTake::CannotServe(reason) => Err(Unserved::because(&reason)),
        }
    }

    /// A worktree the host is done with: a pool worktree is released when `session` holds it and
    /// kept on disk whatever happens; any other worktree is removed as git removes one. Nothing for
    /// a host that does not serve worktrees, and nothing that needs the server while switched off.
    fn returned(
        &self,
        adapter: &dyn HostAdapter,
        session: &SessionRef,
        cwd: &Path,
        path: &Path,
        switched_off: bool,
    ) -> GuardOutcome {
        if served(adapter).is_none() {
            return GuardOutcome::silent();
        }
        let path = cwd.join(path);
        for place in [path.as_path(), cwd] {
            let Ok(Some(workspace)) = self.workspaces.open(place) else {
                continue;
            };
            let Some(pool) = workspace.worktree_pool() else {
                continue;
            };
            let Some(name) = pool.worktree_at(cwd, &path) else {
                continue;
            };
            if switched_off {
                return GuardOutcome::silent();
            }
            return release_returned(pool, session, &name);
        }
        match self.workspaces.git().remove_worktree(&path) {
            Removal::Removed | Removal::Absent => GuardOutcome::silent(),
            Removal::Kept(why) => GuardOutcome::notice(wording::worktree_kept(&path, &why)),
        }
    }
}

/// Why the pool did not serve a wanted worktree, and the notice of the host's own worktree made
/// in its place.
struct Unserved {
    /// The reason, to sit inside another sentence.
    reason: String,
    /// The notice of the host's own worktree.
    notice: String,
}

impl Unserved {
    /// The pool did not serve for `reason`, and the notice says so.
    fn because(reason: &str) -> Self {
        Self {
            reason: reason.to_owned(),
            notice: wording::own_worktree(reason),
        }
    }
}

/// Releases the hold `session` or one of its agents has on the pool worktree `name` given back;
/// leaves another holder's hold alone.
fn release_returned(
    pool: &dyn WorktreePool,
    session: &SessionRef,
    name: &WorktreeName,
) -> GuardOutcome {
    let held = pool
        .records(session)
        .iter()
        .any(|record| record.this_session && record.worktree == *name);
    if !held {
        return GuardOutcome::silent();
    }
    match pool.release(name) {
        Ok(()) => GuardOutcome::silent(),
        Err(error) => {
            GuardOutcome::notice(wording::worktree_not_released(name, &error.to_string()))
        }
    }
}

/// The terms of a host that serves worktrees; `None` for any other.
fn served(adapter: &dyn HostAdapter) -> Option<WorktreeTerms> {
    adapter
        .worktree_terms()
        .filter(|_| adapter.capabilities().serves_worktrees)
}

/// Where a pool worktree's hold stands for an event of a session.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Standing<'a> {
    /// No hold of it is recorded on this machine.
    Free,
    /// The session, or one of its agents, holds it: the session's agents work in its tree, and the
    /// session may look into its agents' trees.
    Held,
    /// Another session, or an agent of one, holds it.
    Other(&'a WorktreeRecord),
}

/// Where the hold of a worktree whose record is `record` stands for the session the record was
/// read for.
pub(super) fn standing(record: Option<&WorktreeRecord>) -> Standing<'_> {
    match record {
        None => Standing::Free,
        Some(record) if record.this_session => Standing::Held,
        Some(record) => Standing::Other(record),
    }
}

/// The directories a command line works in by name: those it changes to (`cd`, `pushd`) and those
/// it runs git in (`git -C <dir>`, `git --work-tree <dir>`), in the order written.
pub(super) fn dirs_named(command: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    for simple in split_commands(command) {
        let mut words = simple.words.iter().map(String::as_str);
        match words.next().map(program_name) {
            Some("cd" | "pushd") => {
                if let Some(dir) = words.find(|word| !word.starts_with('-') || *word == "-")
                    && dir != "-"
                {
                    dirs.push(dir.to_owned());
                }
            }
            Some("git") => git_dirs(words, &mut dirs),
            _ => {}
        }
    }
    dirs
}

/// The directories named by git's options before its subcommand.
fn git_dirs<'w>(mut words: impl Iterator<Item = &'w str>, dirs: &mut Vec<String>) {
    while let Some(word) = words.next() {
        match word {
            "-C" | "--work-tree" => {
                if let Some(dir) = words.next() {
                    dirs.push(dir.to_owned());
                }
            }
            "-c" | "--git-dir" | "--namespace" | "--config-env" => {
                words.next();
            }
            option if option.starts_with("--work-tree=") => {
                dirs.push(option["--work-tree=".len()..].to_owned());
            }
            option if option.starts_with('-') => {}
            _ => return,
        }
    }
}

fn program_name(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

impl Run<'_> {
    /// The pool of the checkout this run is in, and the worktree of it `path` lies in.
    fn pool_worktree(&self, cwd: &Path, path: &Path) -> Option<(&dyn WorktreePool, WorktreeName)> {
        let pool = self.workspace.worktree_pool()?;
        let name = pool.worktree_at(cwd, path)?;
        Some((pool, name))
    }

    /// Leaves a sign of life in the pool worktree `path` lies in, when it lies in one.
    pub(super) fn leave_beat(&self, cwd: &Path, path: &Path) {
        if let Some((pool, name)) = self.pool_worktree(cwd, path) {
            pool.touch(&name, self.now);
        }
    }

    /// Leaves a sign of life in each pool worktree a tool about to run works in by name: the files
    /// an edit writes, the directories a command line changes to or runs git in.
    pub(super) fn leave_beats(&self, cwd: &Path, call: &crate::event::ToolCall) {
        let paths: Vec<PathBuf> = match call {
            crate::event::ToolCall::Edit { paths } => paths.clone(),
            crate::event::ToolCall::Shell { command } => {
                dirs_named(command).into_iter().map(PathBuf::from).collect()
            }
            crate::event::ToolCall::Other => Vec::new(),
        };
        let mut touched: Vec<WorktreeName> = Vec::new();
        for path in paths {
            if let Some((pool, name)) = self.pool_worktree(cwd, &path)
                && !touched.contains(&name)
            {
                pool.touch(&name, self.now);
                touched.push(name);
            }
        }
    }

    /// The refusal for a session or `agent` of it working in a pool worktree, `cwd`, that their
    /// session holds no hold on and another holder has. A worktree nobody holds is taken again for
    /// them, as it is: they were at work in it already. `None` when they may go on, also when the
    /// server could not say.
    pub(super) fn not_the_holder(&self, cwd: &Path, agent: Option<&AgentKey>) -> Option<String> {
        let (pool, name) = self.pool_worktree(cwd, cwd)?;
        let records = pool.records(&self.session);
        let record = records.iter().find(|record| record.worktree == name);
        match standing(record) {
            Standing::Held => None,
            Standing::Other(record) => {
                let holder = wording::WorktreeHolderShown {
                    agent: record.agent.as_ref(),
                    purpose: record.purpose.as_ref(),
                    since: Some(record.since),
                };
                Some(wording::not_the_holder(
                    &name,
                    &holder,
                    pool.rescued(&name).as_deref(),
                ))
            }
            Standing::Free => {
                let holder = WorktreeHolder {
                    session: self.session.clone(),
                    agent: agent.cloned(),
                };
                match pool.retake(&name, &holder) {
                    Ok(Retaken::Took) | Err(_) => None,
                    Ok(Retaken::Held { agent, purpose }) => {
                        let holder = wording::WorktreeHolderShown {
                            agent: agent.as_ref(),
                            purpose: purpose.as_ref(),
                            since: None,
                        };
                        Some(wording::not_the_holder(
                            &name,
                            &holder,
                            pool.rescued(&name).as_deref(),
                        ))
                    }
                }
            }
        }
    }

    /// Releases the holds of the agent that ended, when this machine records one: an agent that
    /// held nothing costs no call.
    pub(super) fn agent_ended(&self, agent: &AgentKey) -> GuardOutcome {
        let Some(pool) = self.workspace.worktree_pool() else {
            return GuardOutcome::silent();
        };
        let held: Vec<WorktreeName> = pool
            .records(&self.session)
            .into_iter()
            .filter(|record| record.this_session && record.agent.as_ref() == Some(agent))
            .map(|record| record.worktree)
            .collect();
        for name in held {
            if let Err(error) = pool.release(&name) {
                return self.no_decision(Problem::from(error));
            }
        }
        GuardOutcome::silent()
    }

    /// Ends the session on the server, which frees its holds and its agents', when this machine
    /// records one of them: a session that held nothing costs no call.
    pub(super) fn session_ended(&self) -> GuardOutcome {
        let Some(pool) = self.workspace.worktree_pool() else {
            return GuardOutcome::silent();
        };
        let held = pool
            .records(&self.session)
            .iter()
            .any(|record| record.this_session);
        if !held {
            return GuardOutcome::silent();
        }
        match pool.end_session(&self.session) {
            Ok(()) => GuardOutcome::silent(),
            Err(error) => self.no_decision(Problem::from(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use super::*;

    fn record(this_session: bool) -> WorktreeRecord {
        WorktreeRecord {
            worktree: WorktreeName::parse("worktree2").unwrap(),
            this_session,
            agent: None,
            purpose: None,
            since: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_worktree_is_free_without_a_record_held_by_its_session_and_else_anothers() {
        assert_eq!(standing(None), Standing::Free);
        let ours = record(true);
        assert_eq!(standing(Some(&ours)), Standing::Held);
        let theirs = record(false);
        assert_eq!(standing(Some(&theirs)), Standing::Other(&theirs));
    }

    #[test]
    fn a_command_line_names_the_directories_it_changes_to_and_runs_git_in() {
        assert_eq!(
            dirs_named("cd ../w2 && git -C /pool/w3 -c a=b status; git --work-tree=/pool/w4 log"),
            ["../w2", "/pool/w3", "/pool/w4"]
        );
        assert_eq!(
            dirs_named("git -c core.x=y --work-tree /pool/w5 commit -C HEAD"),
            ["/pool/w5"],
            "an option of the subcommand is not git's"
        );
        assert_eq!(dirs_named("cd -P /pool/w6"), ["/pool/w6"]);
        assert!(dirs_named("cd - && ls /pool/w7 && /usr/bin/git status").is_empty());
        assert_eq!(dirs_named("/usr/bin/git -C w8 diff"), ["w8"]);
    }
}
