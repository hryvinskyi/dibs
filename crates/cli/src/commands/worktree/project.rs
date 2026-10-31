//! The project's own commands for its worktrees: `prepare` makes a missing one ready to work in,
//! `reset` brings a reused one back to the base. dibs knows git and its server; whatever else a
//! worktree needs is the project's.
//!
//! A command is run by the system's shell (`sh -c` on Unix, `cmd /C` on Windows) with the checkout
//! as its working directory, standard input closed, and its output on dibs's standard error, out of
//! the way of the path dibs prints. It learns which worktree from the environment:
//! `DIBS_WORKTREE_NAME` (`worktree2`), `DIBS_WORKTREE_PATH` (its absolute directory, which may not
//! exist yet for `prepare`), `DIBS_WORKTREE_BASE` (the ref it is reset to) and `DIBS_CHECKOUT`
//! (the checkout's absolute directory), beside everything dibs's own environment holds. Exit `0` is
//! success; any other status, or no end within the step's time, is a failure, and a command still
//! running then is killed with whatever it started in its process group.

use std::io;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use dibs_domain::worktree::WorktreeName;

/// A step of a worktree's life that runs a command of the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Make a missing worktree ready to work in.
    Prepare,
    /// Bring a reused worktree back to the base.
    Reset,
}

impl Step {
    /// The longest one run of the step may take.
    pub const fn limit(self) -> Duration {
        match self {
            Self::Prepare => Duration::from_secs(20 * 60),
            Self::Reset => Duration::from_secs(10 * 60),
        }
    }

    /// The step's name, as the settings and the messages name it.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Reset => "reset",
        }
    }
}

/// The worktree a command is run for.
#[derive(Debug, Clone, Copy)]
pub struct Site<'a> {
    /// Its name.
    pub name: &'a WorktreeName,
    /// Its directory.
    pub path: &'a Path,
    /// The ref it is reset to.
    pub base: &'a str,
    /// The checkout that lends it.
    pub checkout: &'a Path,
}

/// Why a command of the project did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepFailed {
    /// The shell could not be started.
    NotStarted {
        /// The step.
        step: &'static str,
    },
    /// The command ended with a status other than success.
    Failed {
        /// The step.
        step: &'static str,
        /// Its exit status, when it exited rather than being ended by a signal.
        status: Option<i32>,
    },
    /// The command did not end within its time and was killed.
    TimedOut {
        /// The step.
        step: &'static str,
        /// The time it had.
        seconds: u64,
    },
}

impl StepFailed {
    /// What happened, as the end of a sentence about the worktree: `its reset command failed with
    /// status 7`.
    pub fn reason(&self) -> String {
        match self {
            Self::NotStarted { step } => format!("its {step} command could not be started"),
            Self::Failed { step, status } => match status {
                Some(code) => format!("its {step} command failed with status {code}"),
                None => format!("its {step} command failed"),
            },
            Self::TimedOut { step, seconds } => format!(
                "its {step} command did not finish within {seconds} seconds and was stopped"
            ),
        }
    }
}

/// Runs the project's commands.
pub trait ProjectCommands {
    /// Runs `command` for the worktree at `site` as `step`, within `limit`.
    ///
    /// # Errors
    /// [`StepFailed`] when it cannot start, fails, or outlasts `limit`.
    fn run(
        &self,
        step: Step,
        command: &str,
        site: &Site<'_>,
        limit: Duration,
    ) -> Result<(), StepFailed>;
}

/// The project's commands, run by the system's shell.
pub struct ShellCommands;

/// How often a running command is looked at.
const POLL: Duration = Duration::from_millis(50);

impl ProjectCommands for ShellCommands {
    fn run(
        &self,
        step: Step,
        command: &str,
        site: &Site<'_>,
        limit: Duration,
    ) -> Result<(), StepFailed> {
        let word = step.word();
        let mut shell = shell(command);
        shell
            .current_dir(site.checkout)
            .env("DIBS_WORKTREE_NAME", site.name.as_str())
            .env("DIBS_WORKTREE_PATH", site.path)
            .env("DIBS_WORKTREE_BASE", site.base)
            .env("DIBS_CHECKOUT", site.checkout)
            .stdin(Stdio::null())
            .stdout(Stdio::from(io::stderr()))
            .stderr(Stdio::inherit());
        own_group(&mut shell);
        let mut child = shell.spawn().map_err(|error| {
            tracing::debug!(error = %error, step = word, "a project command could not be started");
            StepFailed::NotStarted { step: word }
        })?;
        let status = wait(&mut child, limit).map_err(|error| {
            tracing::debug!(error = %error, step = word, "a project command could not be waited for");
            StepFailed::Failed {
                step: word,
                status: None,
            }
        })?;
        match status {
            Some(status) if status.success() => Ok(()),
            Some(status) => Err(StepFailed::Failed {
                step: word,
                status: status.code(),
            }),
            None => {
                stop(&mut child);
                Err(StepFailed::TimedOut {
                    step: word,
                    seconds: limit.as_secs(),
                })
            }
        }
    }
}

/// The child's status once it ends within `limit`; `None` when it is still running then.
fn wait(child: &mut Child, limit: Duration) -> io::Result<Option<ExitStatus>> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let Some(left) = limit.checked_sub(started.elapsed()) else {
            return Ok(None);
        };
        std::thread::sleep(left.min(POLL));
    }
}

#[cfg(unix)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

#[cfg(not(any(unix, windows)))]
fn shell(command: &str) -> Command {
    Command::new(command)
}

/// Puts the command in a process group of its own, so that a command that outlasts its time is
/// stopped with everything it started.
#[cfg(unix)]
fn own_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_group(_command: &mut Command) {}

/// Kills the command and its process group, and reaps it.
#[cfg(unix)]
fn stop(child: &mut Child) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    let group = i32::try_from(child.id()).map(Pid::from_raw);
    match group {
        Ok(group) => {
            if let Err(error) = killpg(group, Signal::SIGKILL) {
                tracing::debug!(error = %error, "a project command's group could not be killed");
                let _ = child.kill();
            }
        }
        Err(_) => {
            let _ = child.kill();
        }
    }
    let _ = child.wait();
}

/// Kills the command, and reaps it.
#[cfg(not(unix))]
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn site<'a>(name: &'a WorktreeName, dir: &'a Path) -> Site<'a> {
        Site {
            name,
            path: dir,
            base: "main",
            checkout: dir,
        }
    }

    #[test]
    fn a_command_succeeds_fails_or_is_stopped_at_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let name = WorktreeName::parse("worktree1").unwrap();
        let site = site(&name, dir.path());
        let long = Duration::from_secs(10);
        assert_eq!(
            ShellCommands.run(
                Step::Reset,
                "test \"$DIBS_WORKTREE_NAME\" = worktree1",
                &site,
                long
            ),
            Ok(())
        );
        assert_eq!(
            ShellCommands.run(Step::Reset, "exit 7", &site, long),
            Err(StepFailed::Failed {
                step: "reset",
                status: Some(7)
            })
        );
        let started = Instant::now();
        let stopped = ShellCommands.run(Step::Reset, "sleep 30", &site, Duration::from_millis(300));
        assert!(matches!(
            stopped,
            Err(StepFailed::TimedOut { step: "reset", .. })
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
