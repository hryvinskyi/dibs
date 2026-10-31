//! A checkout's pool of worktrees as a command finds it: from any work tree of the repository that
//! lends it — its main work tree, one of the pool's worktrees, or another linked worktree — as a
//! worktree is never a pool of its own. The pool's worktrees, where each lives, what this machine
//! knows of each, the lock whatever resets, prepares or stops one holds, and the mark that says a
//! worktree was prepared.
//!
//! The pool is declared only by the settings file in the repository's git directory, which no
//! branch carries, so a command it names is only ever the member's own; its worktrees are told
//! apart by where they lie. A worktree is marked prepared in its own git directory, never in its
//! tree, and only when git says it is a worktree of the same repository.

use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use time::OffsetDateTime;

use dibs_client::checkout::Checkout;
use dibs_client::credentials::CredentialKey;
use dibs_client::git_dirs::GitDirs;
use dibs_client::state::{StateDir, WorktreeState};
use dibs_client::worktree_pool::{
    PoolWorktree, WorktreeSettings, pool_id, read_marker, read_settings, settings_file,
    write_marker,
};
use dibs_domain::ids::SessionId;
use dibs_domain::worktree::{AgentKey, PoolId, Silence, WorktreeName};

use crate::commands::worktree::select::Facts;
use crate::failure::{Failure, sentence};

/// Why a worktree was not marked prepared.
#[derive(Debug)]
pub enum NotMarked {
    /// The directory is no worktree of the checkout's repository.
    Refused(String),
    /// The file system failed.
    Io(io::Error),
}

impl NotMarked {
    /// What happened, as the end of a sentence about the worktree.
    pub fn reason(&self) -> String {
        match self {
            Self::Refused(reason) => reason.clone(),
            Self::Io(_) => "its mark of being prepared could not be written".to_owned(),
        }
    }
}

impl fmt::Display for NotMarked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(reason) => f.write_str(reason),
            Self::Io(error) => write!(f, "the file system failed: {error}"),
        }
    }
}

impl From<io::Error> for NotMarked {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// One checkout's pool of worktrees.
#[derive(Debug, Clone)]
pub struct Pool {
    checkout: Checkout,
    git: GitDirs,
    settings: WorktreeSettings,
    id: PoolId,
    state: StateDir,
    files: WorktreeState,
}

/// How often a lock held by another run is tried again.
const LOCK_POLL: Duration = Duration::from_millis(100);

impl Pool {
    /// The pool of the repository `here` is a work tree of, with this machine's state under
    /// `state_base`, and the worktree of it `here` is, when it is one.
    ///
    /// # Errors
    /// A configuration [`Failure`] saying that the repository declares no pool, or why its
    /// declaration cannot be used.
    pub fn locate(
        here: &Checkout,
        state_base: &Path,
    ) -> Result<(Self, Option<WorktreeName>), Failure> {
        Self::find(here, state_base)?.ok_or_else(|| no_pool(here))
    }

    /// [`Pool::locate`], `None` when the repository declares no pool.
    ///
    /// # Errors
    /// A configuration [`Failure`] saying why the pool it declares cannot be used.
    pub fn find(
        here: &Checkout,
        state_base: &Path,
    ) -> Result<Option<(Self, Option<WorktreeName>)>, Failure> {
        let Some(dirs) = GitDirs::around(here.root()) else {
            return Ok(None);
        };
        let unusable = |text: String| Failure::config("no_worktree_pool", text);
        let Some(main) = dirs.main_work_tree() else {
            return match fs::symlink_metadata(settings_file(&dirs)) {
                Ok(_) => Err(unusable(format!(
                    "{} declares a pool, but its repository has no main work tree to lend it from.",
                    settings_file(&dirs).display()
                ))),
                Err(_) => Ok(None),
            };
        };
        let Some(settings) = read_settings(&dirs, &main)
            .map_err(|error| Failure::config("invalid_checkout", sentence(&error.to_string())))?
        else {
            return Ok(None);
        };
        let lender = Checkout::discover(&main)
            .ok()
            .flatten()
            .filter(|lender| lender.root() == main)
            .unwrap_or_else(|| Checkout::new(main.clone(), here.config().clone()));
        let main_git = GitDirs::at(&main).ok_or_else(|| {
            unusable(format!(
                "The checkout at {} that lends the pool is no git work tree.",
                main.display()
            ))
        })?;
        let id = pool_id(&settings.root).map_err(|_| {
            unusable(format!(
                "The pool at {} cannot be named on this machine.",
                settings.root.display()
            ))
        })?;
        let key = CredentialKey {
            server: lender.config().server.clone(),
            project: lender.config().project.clone(),
        };
        let state = StateDir::new(state_base, &key);
        let files = state.worktrees(&id);
        let pool = Self {
            checkout: lender,
            git: main_git,
            settings,
            id,
            state,
            files,
        };
        let inside = pool.worktree_of(dirs.work_tree());
        Ok(Some((pool, inside)))
    }

    /// The worktree of the pool whose top is `work_tree`, when it is one.
    fn worktree_of(&self, work_tree: &Path) -> Option<WorktreeName> {
        let here = fs::canonicalize(work_tree).ok()?;
        self.names()
            .into_iter()
            .find(|name| fs::canonicalize(self.path(name)).is_ok_and(|path| path == here))
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

    /// Whether the worktree `name` was prepared: its git directory holds the mark a preparation
    /// or a reset that ended well leaves, naming it as this worktree of this checkout.
    pub fn is_prepared(&self, name: &WorktreeName) -> bool {
        let Some(dirs) = self.own_worktree(name) else {
            return false;
        };
        read_marker(&dirs).is_some_and(|mark| {
            mark.name == *name && same_dir(&mark.checkout, self.checkout.root())
        })
    }

    /// The git directories of the worktree `name`, when it is a linked worktree of the checkout's
    /// repository.
    fn own_worktree(&self, name: &WorktreeName) -> Option<GitDirs> {
        GitDirs::at(&self.path(name))
            .filter(|dirs| dirs.is_linked() && dirs.same_repository(&self.git))
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

    /// Marks the worktree `name` prepared, in its own git directory: nothing is written into its
    /// tree. Refused when the directory is no linked worktree of the checkout's repository.
    ///
    /// # Errors
    /// [`NotMarked`] saying why.
    pub fn mark_prepared(&self, name: &WorktreeName) -> Result<(), NotMarked> {
        let dirs = self.own_worktree(name).ok_or_else(|| {
            NotMarked::Refused(format!(
                "{} is no worktree of the repository of {}",
                self.path(name).display(),
                self.checkout.root().display()
            ))
        })?;
        let checkout = fs::canonicalize(self.checkout.root())
            .unwrap_or_else(|_| self.checkout.root().to_path_buf());
        write_marker(
            &dirs,
            &PoolWorktree {
                checkout,
                name: name.clone(),
            },
        )?;
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
    /// output: one per worktree, emptied as each step starts, in a directory of its own.
    pub fn log(&self, name: &WorktreeName) -> PathBuf {
        self.files.dir().join("logs").join(format!("{name}.log"))
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

/// The failure of a command run where the repository declares no pool.
fn no_pool(here: &Checkout) -> Failure {
    let home = GitDirs::around(here.root()).map_or_else(
        || ".git/dibs/worktrees.toml".to_owned(),
        |dirs| settings_file(&dirs).display().to_string(),
    );
    let mut text = format!(
        "This checkout lends no worktrees: {home} declares no pool. Write a [worktrees] table with count, prepare and reset there to give it one."
    );
    if let Some(ignored) = here.ignored_tables() {
        text.push(' ');
        text.push_str(&ignored);
    }
    Failure::config("no_worktree_pool", text)
}

/// Whether two paths name one directory as the file system resolves them.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use dibs_client::checkout::FILE_NAME;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository `widgets` with a committed `.dibs.toml`, one commit made.
    fn repository(dir: &Path) -> PathBuf {
        let main = fs::canonicalize(dir).unwrap().join("widgets");
        fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        fs::write(
            main.join(FILE_NAME),
            "server = \"https://dibs.example.com\"\nproject = \"widgets\"\n",
        )
        .unwrap();
        git(&main, &["add", "-A"]);
        git(
            &main,
            &[
                "-c",
                "user.name=a",
                "-c",
                "user.email=a@example.com",
                "commit",
                "-q",
                "-m",
                "first",
            ],
        );
        main
    }

    const POOL: &str = "[worktrees]\ncount = 2\nprepare = \"true\"\nreset = \"true\"\n";

    #[test]
    fn a_pool_is_declared_only_in_the_git_directory_and_found_from_every_work_tree() {
        let home = tempfile::tempdir().unwrap();
        let main = repository(home.path());
        let state = home.path().join("state");
        let here = Checkout::discover(&main).unwrap().unwrap();
        assert!(Pool::find(&here, &state).unwrap().is_none());
        let refused = Pool::locate(&here, &state).unwrap_err();
        assert_eq!(refused.code, "no_worktree_pool");
        assert!(
            refused
                .message
                .contains(&main.join(".git/dibs/worktrees.toml").display().to_string()),
            "{}",
            refused.message
        );

        fs::create_dir_all(main.join(".git/dibs")).unwrap();
        fs::write(main.join(".git/dibs/worktrees.toml"), POOL).unwrap();
        let (pool, inside) = Pool::locate(&here, &state).unwrap();
        assert_eq!(pool.names().len(), 2);
        assert_eq!(inside, None);
        assert_eq!(pool.checkout().root(), main);

        let worktree1 = pool.path(&pool.names()[0]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &worktree1.to_string_lossy(),
                "HEAD",
            ],
        );
        let in_worktree = Checkout::discover(&worktree1).unwrap().unwrap();
        let (from_inside, inside) = Pool::locate(&in_worktree, &state).unwrap();
        assert_eq!(inside, Some(pool.names()[0].clone()));
        assert_eq!(from_inside.id(), pool.id());
        assert_eq!(
            from_inside.checkout().root(),
            main,
            "never a pool of its own"
        );
        assert!(!pool.is_prepared(&pool.names()[0]));
        pool.mark_prepared(&pool.names()[0]).unwrap();
        assert!(pool.is_prepared(&pool.names()[0]));
        assert!(
            main.join(".git/worktrees/worktree1/dibs/pool-worktree.toml")
                .is_file(),
            "the mark lies in the worktree's git directory"
        );
    }

    #[test]
    fn a_directory_that_is_no_worktree_of_the_repository_is_never_marked() {
        let home = tempfile::tempdir().unwrap();
        let main = repository(home.path());
        fs::create_dir_all(main.join(".git/dibs")).unwrap();
        fs::write(main.join(".git/dibs/worktrees.toml"), POOL).unwrap();
        let here = Checkout::discover(&main).unwrap().unwrap();
        let (pool, _) = Pool::locate(&here, &home.path().join("state")).unwrap();
        let name = pool.names()[0].clone();
        let elsewhere = pool.path(&name).with_file_name("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let stranger = repository(&elsewhere);
        fs::rename(&stranger, pool.path(&name)).unwrap();
        let refused = pool.mark_prepared(&name).unwrap_err();
        assert!(
            refused
                .reason()
                .contains("is no worktree of the repository"),
            "{refused}"
        );
        assert!(!pool.is_prepared(&name));
        assert!(!pool.path(&name).join(".git/dibs").exists());
    }
}
