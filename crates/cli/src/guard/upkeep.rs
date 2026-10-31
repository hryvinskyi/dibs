//! The background sync a hook starts and never waits for: `dibs sync`, in a process of its own
//! that outlives the hook, with nothing of the hook's open (its standard streams go nowhere, so a
//! host waiting for the hook's output to end is not kept waiting).
//!
//! A start of a session that finds the daily check for a newer client due asks the sync for it
//! (`--update`); no other event does.
//!
//! A command of the command line starts one too (`--report`) when the hooks counted refusals or
//! failures that no heartbeat carried yet.
//!
//! One at a time per project: the hook starts one only while it can take the project's sync lock,
//! and not again within a few seconds of the last start; the sync itself takes the same lock for
//! the whole of its run. A second hook that cannot take the lock starts nothing.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration as StdDuration;

use time::{Duration, OffsetDateTime};

use dibs_agent_host::ports::{PortError, SessionRef, Upkeep};
use dibs_agent_host::upkeep::{Due, is_fresh};
use dibs_client::state::StateDir;

/// The name of the lock the sync holds while it runs.
pub const SYNC_LOCK: &str = "sync";

/// How soon after one start no other sync is started.
pub const START_SPACING: Duration = Duration::seconds(15);

/// Starts `dibs sync` in the background.
pub struct DetachedSync {
    state: StateDir,
    root: PathBuf,
}

impl DetachedSync {
    /// Syncs for the checkout at `root`, with the project's state in `state`.
    pub fn new(state: StateDir, root: &Path) -> Self {
        Self {
            state,
            root: root.to_path_buf(),
        }
    }
}

fn refused(what: &str) -> impl Fn(std::io::Error) -> PortError + '_ {
    move |error| {
        tracing::debug!(error = %error, what, "the background sync could not start");
        PortError::Refused(format!("{what} failed on this machine."))
    }
}

impl DetachedSync {
    /// Starts `dibs sync --quiet` with `args` in the background, unless one runs or, when
    /// `spaced`, was started within [`START_SPACING`]; answers whether it was started now. Only a
    /// spaced start is remembered as the last start.
    fn spawn(&self, now: OffsetDateTime, spaced: bool, args: &[&str]) -> Result<bool, PortError> {
        let lock = open_lock(&self.state).map_err(refused("Opening the sync lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(error)) => return Err(refused("Taking the sync lock")(error)),
        }
        if spaced {
            if self
                .state
                .sync_started_at()
                .is_some_and(|at| is_fresh(at, now, START_SPACING))
            {
                return Ok(false);
            }
            self.state
                .remember_sync_started(now)
                .map_err(refused("Recording the sync's start"))?;
        }
        let exe = std::env::current_exe().map_err(refused("Finding the dibs binary"))?;
        let mut command = Command::new(exe);
        command
            .args(["sync", "--quiet"])
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut command);
        command.spawn().map_err(refused("Starting dibs sync"))?;
        Ok(true)
    }

    /// Starts the sync a command of the command line leaves behind when the hooks counted
    /// something not reported yet: a heartbeat of the command line's session that carries it.
    /// A command runs far less often than a hook, so only a sync still running holds it back, and
    /// it does not count as the last start the hooks space theirs from: a session's first hook
    /// still asks for the daily check for a newer client.
    ///
    /// # Errors
    /// [`PortError::Refused`] when it could not be started.
    pub fn start_reporting(&self, now: OffsetDateTime) -> Result<bool, PortError> {
        self.spawn(now, false, &["--report"])
    }
}

impl Upkeep for DetachedSync {
    fn start(
        &self,
        session: &SessionRef,
        now: OffsetDateTime,
        due: Due,
    ) -> Result<bool, PortError> {
        let mut args = vec![
            "--host",
            session.host.as_str(),
            "--session",
            session.key.as_str(),
        ];
        if due.update {
            args.push("--update");
        }
        self.spawn(now, true, &args)
    }
}

/// The project's sync lock file, opened (and made when missing).
///
/// # Errors
/// The file system's failure.
pub fn open_lock(state: &StateDir) -> std::io::Result<File> {
    let path = state.lock_file(SYNC_LOCK)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
}

/// The project's sync lock, taken for as long as the answer lives; `None` when another process
/// still holds it after `patience`.
///
/// # Errors
/// The file system's failure.
pub fn take_lock(state: &StateDir, patience: StdDuration) -> std::io::Result<Option<File>> {
    let lock = open_lock(state)?;
    let step = StdDuration::from_millis(20);
    let mut waited = StdDuration::ZERO;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(Some(lock)),
            Err(TryLockError::WouldBlock) if waited < patience => {
                thread::sleep(step);
                waited += step;
            }
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

/// Puts the child in a process group of its own, so the signals that end the hook's group (a
/// host stopping its hook, a terminal closing) do not reach it.
#[cfg(unix)]
pub fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Starts the child detached from the hook's console, in a process group of its own, so it
/// neither opens a window nor ends with the hook's console.
#[cfg(windows)]
pub fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(not(any(unix, windows)))]
pub fn detach(_command: &mut Command) {}
