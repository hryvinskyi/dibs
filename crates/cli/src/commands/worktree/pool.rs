//! A checkout's pool of worktrees as a command finds it: from the checkout that lends it, or from
//! inside one of its worktrees, whose own `.dibs.toml` names that checkout — a worktree is never a
//! pool of its own. The pool's worktrees, where each lives, what this machine knows of each, the
//! lock whatever resets, prepares or stops one holds, and the `.dibs.toml` dibs writes into a
//! worktree so that it is a governed checkout of the same project.
//!
//! A pool's tables are read only from a `.dibs.toml` that git neither tracks nor leaves unignored,
//! as git answers in the directory of the file. The `[worktrees]` table names commands dibs runs as
//! the person reading it; from a committed file — or one a commit could pick up — those would be
//! whatever a branch says, run on every member's machine that checks it out. So a checkout whose
//! `[worktrees]` table sits in such a file lends no pool and runs nothing, and a worktree whose
//! `[pool_worktree]` table does is no pool worktree. The worktree's own file is written only where
//! git would neither track nor see it, and never through a link: the base it is reset to decides
//! what else lies in the worktree.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use time::OffsetDateTime;

use dibs_client::checkout::{Checkout, FILE_NAME, WorktreeSettings, pool_id};
use dibs_client::credentials::CredentialKey;
use dibs_client::state::{StateDir, WorktreeState};
use dibs_domain::ids::SessionId;
use dibs_domain::worktree::{AgentKey, PoolId, Silence, WorktreeName};

use crate::commands::worktree::rescue::{GitFailed, WorktreeGit};
use crate::commands::worktree::select::Facts;
use crate::failure::{Failure, sentence};

/// Where git stands on a checkout's own `.dibs.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Git tracks it: it is committed, or staged to be.
    Tracked,
    /// Git neither tracks nor ignores it: the next `git add -A` stages it.
    Exposed,
    /// Git ignores it and does not track it: it stays on this machine.
    Private,
}

/// Tells where git stands on the `.dibs.toml` of a directory.
pub trait ConfigStandings: fmt::Debug + Sync {
    /// Where git stands on `<dir>/.dibs.toml`, which need not exist.
    ///
    /// # Errors
    /// The end of a sentence saying why git could not tell.
    fn standing(&self, dir: &Path) -> Result<Standing, String>;
}

/// Asks the git of the directory.
#[derive(Debug)]
pub struct GitStandings;

impl ConfigStandings for GitStandings {
    fn standing(&self, dir: &Path) -> Result<Standing, String> {
        let git = WorktreeGit::new(dir);
        let reason = |failed: GitFailed| failed.reason();
        if git.tracks(FILE_NAME).map_err(reason)? {
            return Ok(Standing::Tracked);
        }
        if git.ignores(FILE_NAME).map_err(reason)? {
            return Ok(Standing::Private);
        }
        Ok(Standing::Exposed)
    }
}

/// Why the `.dibs.toml` of a worktree was not written.
#[derive(Debug)]
pub enum ConfigNotWritten {
    /// Git would track or see the file there, or could not tell.
    Refused(String),
    /// The file system failed.
    Io(io::Error),
}

impl ConfigNotWritten {
    /// What happened, as the end of a sentence about the worktree.
    pub fn reason(&self) -> String {
        match self {
            Self::Refused(reason) => reason.clone(),
            Self::Io(_) => "its .dibs.toml could not be written".to_owned(),
        }
    }
}

impl fmt::Display for ConfigNotWritten {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(reason) => f.write_str(reason),
            Self::Io(error) => write!(f, "the file system failed: {error}"),
        }
    }
}

impl From<io::Error> for ConfigNotWritten {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// One checkout's pool of worktrees.
#[derive(Debug, Clone)]
pub struct Pool {
    checkout: Checkout,
    settings: WorktreeSettings,
    id: PoolId,
    state: StateDir,
    files: WorktreeState,
    standings: &'static dyn ConfigStandings,
}

/// The `.dibs.toml` of a pool worktree.
#[derive(Serialize)]
struct WorktreeFile<'a> {
    server: &'a str,
    project: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<&'a str>,
    pool_worktree: PoolWorktreeFile,
}

#[derive(Serialize)]
struct PoolWorktreeFile {
    checkout: String,
    name: String,
}

/// How often a lock held by another run is tried again.
const LOCK_POLL: Duration = Duration::from_millis(100);

impl Pool {
    /// The pool of the checkout `here` governs — its own, or, inside a pool worktree, the pool of
    /// the checkout that lends it — with this machine's state under `state_base`, and the worktree
    /// `here` is, when it is one. Git is asked where it stands on the files that hold the pool's
    /// tables.
    ///
    /// # Errors
    /// A configuration [`Failure`] saying which file declares no pool, why it cannot be read, or
    /// that git tracks it or does not ignore it.
    pub fn locate(
        here: &Checkout,
        state_base: &Path,
    ) -> Result<(Self, Option<WorktreeName>), Failure> {
        static GIT: GitStandings = GitStandings;
        Self::locate_with(here, state_base, &GIT)
    }

    /// [`Pool::locate`], with `standings` telling where git stands on each file.
    ///
    /// # Errors
    /// As [`Pool::locate`].
    pub fn locate_with(
        here: &Checkout,
        state_base: &Path,
        standings: &'static dyn ConfigStandings,
    ) -> Result<(Self, Option<WorktreeName>), Failure> {
        let (lender, inside) = match &here.config().pool_worktree {
            Some(mark) => {
                kept_local(here, "[pool_worktree]", standings.standing(here.root()))?;
                let lender = Checkout::discover(&mark.checkout)
                    .map_err(|error| {
                        Failure::config("invalid_checkout", sentence(&error.to_string()))
                    })?
                    .filter(|lender| lender.config().worktrees.is_some())
                    .ok_or_else(|| {
                        Failure::config(
                            "no_worktree_pool",
                            format!(
                                "{} says this is the worktree {} of the checkout at {}, which lends no worktrees any more; work in the checkout or another worktree.",
                                here.file().display(),
                                mark.name,
                                mark.checkout.display()
                            ),
                        )
                    })?;
                (lender, Some(mark.name.clone()))
            }
            None => (here.clone(), None),
        };
        let Some(settings) = lender.config().worktrees.clone() else {
            return Err(Failure::config(
                "no_worktree_pool",
                format!(
                    "{} has no [worktrees] table, so this checkout lends no worktrees; add one with count, prepare and reset to give it a pool.",
                    lender.file().display()
                ),
            ));
        };
        kept_local(&lender, "[worktrees]", standings.standing(lender.root()))?;
        let id = pool_id(&settings.root).map_err(|_| {
            Failure::config(
                "no_worktree_pool",
                format!(
                    "The pool at {} cannot be named on this machine.",
                    settings.root.display()
                ),
            )
        })?;
        let key = CredentialKey {
            server: lender.config().server.clone(),
            project: lender.config().project.clone(),
        };
        let state = StateDir::new(state_base, &key);
        let files = state.worktrees(&id);
        let pool = Self {
            checkout: lender,
            settings,
            id,
            state,
            files,
            standings,
        };
        Ok((pool, inside))
    }

    /// The pool's id.
    pub fn id(&self) -> &PoolId {
        &self.id
    }

    /// The pool's settings.
    pub fn settings(&self) -> &WorktreeSettings {
        &self.settings
    }

    /// The checkout that lends the pool.
    pub fn checkout(&self) -> &Checkout {
        &self.checkout
    }

    /// What this machine knows of the pool's worktrees.
    pub fn files(&self) -> &WorktreeState {
        &self.files
    }

    /// What this machine remembers of the pool's project.
    pub fn state(&self) -> &StateDir {
        &self.state
    }

    /// The pool's worktrees, in order.
    pub fn names(&self) -> Vec<WorktreeName> {
        self.settings.names()
    }

    /// The worktree `name`, when it is one of the pool's.
    ///
    /// # Errors
    /// A configuration [`Failure`] naming the pool's worktrees.
    pub fn known(&self, name: &WorktreeName) -> Result<WorktreeName, Failure> {
        let names = self.names();
        if names.contains(name) {
            return Ok(name.clone());
        }
        let listed: Vec<&str> = names.iter().map(WorktreeName::as_str).collect();
        Err(Failure::config(
            "unknown_worktree",
            format!(
                "This pool has no worktree {name}; its worktrees are {}.",
                listed.join(", ")
            ),
        ))
    }

    /// The directory of the worktree `name`.
    pub fn path(&self, name: &WorktreeName) -> PathBuf {
        self.settings.path_of(name)
    }

    /// Whether the worktree `name` was prepared: its own `.dibs.toml`, written once a preparation or
    /// a reset ended well, names it as this worktree.
    pub fn is_prepared(&self, name: &WorktreeName) -> bool {
        let file = self.path(name).join(FILE_NAME);
        let Ok(text) = fs::read_to_string(&file) else {
            return false;
        };
        Checkout::parse(&file, &text)
            .ok()
            .and_then(|config| config.pool_worktree)
            .is_some_and(|mark| mark.name == *name)
    }

    /// Whether the worktree `name` is a git worktree on disk.
    pub fn exists(&self, name: &WorktreeName) -> bool {
        self.path(name).join(".git").exists()
    }

    /// The git worktrees in the pool's directory that are none of its worktrees — left there when
    /// the pool was made smaller — by directory name, in order.
    pub fn beyond_count(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.settings.root) else {
            return Vec::new();
        };
        let names = self.names();
        let mut beyond: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().join(".git").exists())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|directory| !names.iter().any(|name| name.as_str() == directory))
            .collect();
        beyond.sort();
        beyond
    }

    /// What a take by the holder `session`/`agent` at `now` knows of the worktree `name`.
    pub fn facts(
        &self,
        name: &WorktreeName,
        session: SessionId,
        agent: Option<&AgentKey>,
        now: OffsetDateTime,
    ) -> Facts {
        let own = self
            .files
            .hold(name)
            .is_some_and(|record| record.is_of(session, agent));
        Facts {
            name: name.clone(),
            prepared: self.is_prepared(name),
            stopped: self.files.stopped_at(name).is_some(),
            broken: self.files.broken(name).is_some(),
            in_use: !own && self.in_use(name, now),
            locked: self.is_locked(name),
        }
    }

    /// Whether somebody still works in `name` by this machine's beats: a sign of life newer than
    /// its last release here, younger than an agent's silence.
    pub fn in_use(&self, name: &WorktreeName, now: OffsetDateTime) -> bool {
        let Some(beat) = self.files.beat_at(name) else {
            return false;
        };
        let after_release = self.files.free_since(name).is_none_or(|free| beat > free);
        after_release && now - beat < Silence::DEFAULT.agent
    }

    /// Writes the worktree's own `.dibs.toml`: the checkout's server, project and repository, and
    /// the `[pool_worktree]` table naming the checkout and the worktree. Refused where git tracks
    /// the file — the base the worktree was reset to carries one — or would not ignore it. Written
    /// beside it into a file made afresh, whatever lay at that name removed first, then renamed
    /// over it: a link the base carries is replaced, never followed.
    ///
    /// # Errors
    /// [`ConfigNotWritten`] saying why.
    pub fn write_config(&self, name: &WorktreeName) -> Result<(), ConfigNotWritten> {
        let dir = self.path(name);
        match self.standings.standing(&dir) {
            Ok(Standing::Private) => {}
            Ok(Standing::Tracked) => {
                return Err(ConfigNotWritten::Refused(format!(
                    "{FILE_NAME} is tracked at {}, and dibs writes that file in each worktree itself; reset the pool to a base that does not carry one",
                    self.settings.base
                )));
            }
            Ok(Standing::Exposed) => {
                return Err(ConfigNotWritten::Refused(format!(
                    "git does not ignore {FILE_NAME} there, so the file dibs writes would be committed; add /{FILE_NAME} to the checkout's .git/info/exclude"
                )));
            }
            Err(reason) => {
                return Err(ConfigNotWritten::Refused(format!(
                    "{reason}, so its {FILE_NAME} was not written"
                )));
            }
        }
        let config = self.checkout.config();
        let lender = fs::canonicalize(self.checkout.root())
            .unwrap_or_else(|_| self.checkout.root().to_path_buf());
        let file = WorktreeFile {
            server: config.server.as_str(),
            project: config.project.as_str(),
            repo: config.repo.as_ref().map(|repo| repo.as_str()),
            pool_worktree: PoolWorktreeFile {
                checkout: lender.to_string_lossy().into_owned(),
                name: name.to_string(),
            },
        };
        let text = toml::to_string(&file).map_err(io::Error::other)?;
        let path = dir.join(FILE_NAME);
        let temp = dir.join(format!("{FILE_NAME}.dibs-new"));
        match fs::remove_file(&temp) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        let mut written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        written.write_all(text.as_bytes())?;
        drop(written);
        fs::rename(&temp, &path)?;
        Ok(())
    }

    /// The lock of the worktree `name`, taken; `None` when another run still holds it after
    /// `patience`.
    ///
    /// # Errors
    /// The file system's failure.
    pub async fn lock(&self, name: &WorktreeName, patience: Duration) -> io::Result<Option<File>> {
        let lock = self.open_lock(name)?;
        let started = Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(Some(lock)),
                Err(TryLockError::WouldBlock) => {
                    let Some(left) = patience.checked_sub(started.elapsed()) else {
                        return Ok(None);
                    };
                    if left.is_zero() {
                        return Ok(None);
                    }
                    tokio::time::sleep(left.min(LOCK_POLL)).await;
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
    }

    /// Whether another run holds the lock of the worktree `name` now.
    pub fn is_locked(&self, name: &WorktreeName) -> bool {
        let Ok(lock) = self.open_lock(name) else {
            return false;
        };
        matches!(lock.try_lock(), Err(TryLockError::WouldBlock))
    }

    /// The log the project's commands write for the worktree `name` when no person reads their
    /// output: one per worktree, emptied as each step starts.
    pub fn log(&self, name: &WorktreeName) -> PathBuf {
        self.files.dir().join(format!("{name}.log"))
    }

    fn open_lock(&self, name: &WorktreeName) -> io::Result<File> {
        let path = self.state.worktree_lock(&self.id, name)?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
    }
}

/// Refuses the pool table `table` of `checkout` unless git, as `standing` says, neither tracks its
/// file nor leaves it unignored.
fn kept_local(
    checkout: &Checkout,
    table: &str,
    standing: Result<Standing, String>,
) -> Result<(), Failure> {
    let file = checkout.file();
    let file = file.display();
    let said = match standing {
        Ok(Standing::Private) => return Ok(()),
        Ok(Standing::Tracked) => format!(
            "{file} holds a {table} table, but git tracks the file, so dibs runs no command of it: a committed table would run on every machine that checks the branch out. Keep a pool's {FILE_NAME} out of git: untrack it (git rm --cached {FILE_NAME}) and add /{FILE_NAME} to .git/info/exclude."
        ),
        Ok(Standing::Exposed) => format!(
            "{file} holds a {table} table, but git does not ignore the file, so the next commit could carry it and dibs runs no command of it. Add /{FILE_NAME} to .git/info/exclude, keeping it untracked."
        ),
        Err(reason) => format!(
            "{file} holds a {table} table, but {reason}, so dibs cannot tell that the file is kept out of git and runs no command of it."
        ),
    };
    Err(Failure::config("worktrees_not_local", said))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout(dir: &Path) -> Checkout {
        let file = dir.join(FILE_NAME);
        let text = "server = \"https://dibs.example.com\"\nproject = \"widgets\"\n[worktrees]\ncount = 2\nprepare = \"true\"\nreset = \"true\"\n";
        Checkout::new(dir.to_path_buf(), Checkout::parse(&file, text).unwrap())
    }

    #[derive(Debug)]
    struct Says(Standing);

    impl ConfigStandings for Says {
        fn standing(&self, _dir: &Path) -> Result<Standing, String> {
            Ok(self.0)
        }
    }

    #[derive(Debug)]
    struct CannotTell;

    impl ConfigStandings for CannotTell {
        fn standing(&self, _dir: &Path) -> Result<Standing, String> {
            Err("git could not tell whether it tracks a file in /home/alice/widgets".to_owned())
        }
    }

    static TRACKED: Says = Says(Standing::Tracked);
    static EXPOSED: Says = Says(Standing::Exposed);
    static PRIVATE: Says = Says(Standing::Private);
    static UNKNOWN: CannotTell = CannotTell;

    #[test]
    fn a_pool_is_lent_only_from_a_file_git_neither_tracks_nor_leaves_unignored() {
        let home = tempfile::tempdir().unwrap();
        let here = checkout(&home.path().join("widgets"));
        let state = home.path().join("state");
        let refused = |standings: &'static dyn ConfigStandings| {
            Pool::locate_with(&here, &state, standings).unwrap_err()
        };
        let tracked = refused(&TRACKED);
        assert_eq!(tracked.code, "worktrees_not_local");
        assert!(
            tracked.message.contains("git tracks the file"),
            "{}",
            tracked.message
        );
        assert!(
            tracked.message.contains("git rm --cached .dibs.toml")
                && tracked.message.contains(".git/info/exclude"),
            "{}",
            tracked.message
        );
        let exposed = refused(&EXPOSED);
        assert!(
            exposed.message.contains("git does not ignore the file"),
            "{}",
            exposed.message
        );
        assert!(
            refused(&UNKNOWN).message.contains("cannot tell"),
            "a file git cannot answer for lends nothing"
        );
        let (pool, inside) = Pool::locate_with(&here, &state, &PRIVATE).unwrap();
        assert_eq!(pool.names().len(), 2);
        assert_eq!(inside, None);
    }
}
