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
//! One at a time per project: the sync takes the project's sync lock for the whole of its run. A
//! hook starts one that does everything due only while it can take that lock, and not again
//! within a few seconds of the last such start, so a burst of hooks starts one at most and the
//! work done once for the whole machine (the caches, the report the heartbeat carries, the signs
//! of life in pool worktrees, the check for a newer client) is not repeated by every session.
//!
//! A session's heartbeat is due by the session's own clock, and each session's heartbeat keeps its
//! own session seen and reports its own checkout, which no other session's can do for it, so
//! another session's start does not hold it back:
//! when the start that does everything is refused, a hook whose session's heartbeat is due starts a
//! sync of that heartbeat alone (`--heartbeat-only`), unless one was started for the same session
//! within the same few seconds. A sync of the heartbeat alone waits its turn while another sync
//! holds the lock, so the hook never waits and the heartbeat is not lost; one that waits in vain
//! leaves the heartbeat due, and the session's next hook starts another.

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

/// How soon after one start no other sync that does everything is started, and how soon after a
/// start for one session no heartbeat-only sync is started for that session. A sync that does
/// everything is spaced by the machine's last start alone.
pub const START_SPACING: Duration = Duration::seconds(15);

/// How long any sync a hook starts, of one session's heartbeat alone or of everything due, waits
/// for another sync to let go of the lock: shorter than [`START_SPACING`], and a start for a
/// session is spaced from the last one for it, so a session never has two syncs waiting.
pub const TURN_PATIENCE: StdDuration = StdDuration::from_secs(10);

/// What a hook starts in the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    /// Everything due, the work done once for the whole machine included.
    Everything,
    /// The session's heartbeat alone.
    Heartbeat,
}

impl Start {
    /// What a hook of a session starts at `now`, if anything: everything due while the project's
    /// sync lock is `free` and no such start was made within [`START_SPACING`] (`last_start`);
    /// otherwise the session's heartbeat alone, when it is due and nothing was started for the
    /// session within [`START_SPACING`] (`session_start`). Each moment is read only when it
    /// decides.
    fn for_hook(
        now: OffsetDateTime,
        due: Due,
        free: bool,
        last_start: impl FnOnce() -> Option<OffsetDateTime>,
        session_start: impl FnOnce() -> Option<OffsetDateTime>,
    ) -> Option<Self> {
        let spaced =
            |at: Option<OffsetDateTime>| at.is_some_and(|at| is_fresh(at, now, START_SPACING));
        if free && !spaced(last_start()) {
            Some(Self::Everything)
        } else if due.heartbeat && !spaced(session_start()) {
            Some(Self::Heartbeat)
        } else {
            None
        }
    }

    /// Whether this start is the one the hooks space theirs from.
    const fn does_everything(self) -> bool {
        matches!(self, Self::Everything)
    }

    /// The arguments of `dibs sync` for this start of `session`.
    fn args(self, session: &SessionRef, due: Due) -> Vec<&str> {
        let mut args = vec![
            "--host",
            session.host.as_str(),
            "--session",
            session.key.as_str(),
        ];
        match self {
            Self::Everything if due.update => args.push("--update"),
            Self::Everything => {}
            Self::Heartbeat => args.push("--heartbeat-only"),
        }
        args
    }
}

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
    /// The project's sync lock, opened, and whether it was free and is now taken for as long as
    /// the file lives.
    fn try_lock(&self) -> Result<(File, bool), PortError> {
        let lock = open_lock(&self.state).map_err(refused("Opening the sync lock"))?;
        match lock.try_lock() {
            Ok(()) => Ok((lock, true)),
            Err(TryLockError::WouldBlock) => Ok((lock, false)),
            Err(TryLockError::Error(error)) => Err(refused("Taking the sync lock")(error)),
        }
    }

    /// Starts `dibs sync --quiet` with `args` in the background.
    fn spawn(&self, args: &[&str]) -> Result<(), PortError> {
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
        Ok(())
    }

    /// Starts the sync a command of the command line leaves behind when the hooks counted
    /// something not reported yet: a heartbeat of the command line's session that carries it and
    /// names no checkout, as that session works in none by running it.
    /// A command runs far less often than a hook, so only a sync still running holds it back, and
    /// it does not count as the last start the hooks space theirs from: a session's first hook
    /// still asks for the daily check for a newer client.
    ///
    /// # Errors
    /// [`PortError::Refused`] when it could not be started.
    pub fn start_reporting(&self) -> Result<bool, PortError> {
        let (_lock, free) = self.try_lock()?;
        if free {
            self.spawn(&["--report"])?;
        }
        Ok(free)
    }
}

impl Upkeep for DetachedSync {
    /// The lock stays taken until the sync is started, so two hooks never both start one that
    /// does everything; a start for a session is remembered before it is made, so a burst of that
    /// session's hooks starts one.
    fn start(
        &self,
        session: &SessionRef,
        now: OffsetDateTime,
        due: Due,
    ) -> Result<bool, PortError> {
        let (_lock, free) = self.try_lock()?;
        let (host, key) = (session.host.as_str(), session.key.as_str());
        let Some(start) = Start::for_hook(
            now,
            due,
            free,
            || self.state.sync_started_at(),
            || self.state.session_sync_started_at(host, key),
        ) else {
            return Ok(false);
        };
        if start.does_everything() {
            self.state
                .remember_sync_started(now)
                .map_err(refused("Recording the sync's start"))?;
        }
        self.state
            .remember_session_sync_started(host, key, now)
            .map_err(refused("Recording the sync's start"))?;
        self.spawn(&start.args(session, due))?;
        Ok(true)
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

#[cfg(test)]
mod tests {
    use dibs_agent_host::{HostId, SessionKey};

    use super::*;

    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn heartbeat_due() -> Due {
        Due {
            heartbeat: true,
            status: true,
            ..Due::default()
        }
    }

    fn ago(seconds: i64) -> Option<OffsetDateTime> {
        Some(NOW - Duration::seconds(seconds))
    }

    #[test]
    fn a_due_heartbeat_is_not_held_back_by_another_sessions_start_a_moment_ago() {
        let start = Start::for_hook(NOW, heartbeat_due(), true, || ago(5), || None);
        assert_eq!(start, Some(Start::Heartbeat));
        let earlier_own = Start::for_hook(NOW, heartbeat_due(), true, || ago(5), || ago(15));
        assert_eq!(
            earlier_own,
            Some(Start::Heartbeat),
            "the session's own start is as old as the spacing"
        );
    }

    #[test]
    fn a_second_start_for_the_same_session_within_the_spacing_is_refused() {
        for free in [true, false] {
            let start = Start::for_hook(NOW, heartbeat_due(), free, || ago(5), || ago(5));
            assert_eq!(start, None, "lock free: {free}");
        }
    }

    #[test]
    fn a_due_heartbeat_waits_its_turn_while_another_sync_holds_the_lock() {
        let start = Start::for_hook(
            NOW,
            heartbeat_due(),
            false,
            || panic!("no start that does everything is made while the lock is held"),
            || None,
        );
        assert_eq!(start, Some(Start::Heartbeat));
        let nothing_of_its_own = Due {
            status: true,
            ..Due::default()
        };
        assert_eq!(
            Start::for_hook(NOW, nothing_of_its_own, false, || None, || None),
            None,
            "the running sync does the machine's work"
        );
    }

    #[test]
    fn the_machines_work_is_started_at_most_once_within_the_spacing() {
        let start = Start::for_hook(
            NOW,
            heartbeat_due(),
            true,
            || ago(15),
            || panic!("a start that does everything reads no session's start"),
        );
        assert_eq!(start, Some(Start::Everything));
        assert_eq!(
            Start::for_hook(NOW, heartbeat_due(), true, || None, || ago(1)),
            Some(Start::Everything)
        );
        let caches_only = Due {
            status: true,
            inbox: true,
            ..Due::default()
        };
        assert_eq!(
            Start::for_hook(NOW, caches_only, true, || ago(14), || None),
            None,
            "a start a moment ago does the caches"
        );
        assert_eq!(
            Start::for_hook(NOW, heartbeat_due(), true, || ago(-60), || None),
            Some(Start::Everything),
            "a start from the future holds nothing back"
        );
    }

    #[test]
    fn a_start_of_the_heartbeat_alone_never_asks_for_a_newer_client() {
        let session = SessionRef {
            host: HostId::new("neutral"),
            key: SessionKey::parse("s-1").unwrap(),
        };
        let due = Due {
            update: true,
            ..heartbeat_due()
        };
        assert_eq!(
            Start::Everything.args(&session, due),
            ["--host", "neutral", "--session", "s-1", "--update"]
        );
        assert_eq!(
            Start::Everything.args(&session, heartbeat_due()),
            ["--host", "neutral", "--session", "s-1"]
        );
        assert_eq!(
            Start::Heartbeat.args(&session, due),
            ["--host", "neutral", "--session", "s-1", "--heartbeat-only"]
        );
        assert!(Start::Everything.does_everything());
        assert!(!Start::Heartbeat.does_everything());
    }
}
