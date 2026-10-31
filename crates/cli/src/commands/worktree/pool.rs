//! A checkout's pool of worktrees as a command finds it: from the checkout that lends it, or from
//! inside one of its worktrees, whose own `.dibs.toml` names that checkout — a worktree is never a
//! pool of its own. The pool's worktrees, where each lives, what this machine knows of each, the
//! lock whatever resets, prepares or stops one holds, and the `.dibs.toml` dibs writes into a
//! worktree so that it is a governed checkout of the same project.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use time::OffsetDateTime;

use dibs_client::checkout::{Checkout, FILE_NAME, WorktreeSettings, pool_id};
use dibs_client::credentials::CredentialKey;
use dibs_client::state::{StateDir, WorktreeState};
use dibs_domain::ids::SessionId;
use dibs_domain::worktree::{AgentKey, PoolId, Silence, WorktreeName};

use crate::commands::worktree::select::Facts;
use crate::failure::{Failure, sentence};

/// One checkout's pool of worktrees.
#[derive(Debug, Clone)]
pub struct Pool {
    checkout: Checkout,
    settings: WorktreeSettings,
    id: PoolId,
    state: StateDir,
    files: WorktreeState,
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
    /// `here` is, when it is one.
    ///
    /// # Errors
    /// A configuration [`Failure`] saying which file declares no pool, or why it cannot be read.
    pub fn locate(
        here: &Checkout,
        state_base: &Path,
    ) -> Result<(Self, Option<WorktreeName>), Failure> {
        let (lender, inside) = match &here.config().pool_worktree {
            Some(mark) => {
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
    /// the `[pool_worktree]` table naming the checkout and the worktree. Written beside it, then
    /// renamed over it.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn write_config(&self, name: &WorktreeName) -> io::Result<()> {
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
        let path = self.path(name).join(FILE_NAME);
        let temp = self.path(name).join(format!("{FILE_NAME}.dibs-new"));
        fs::write(&temp, text)?;
        fs::rename(&temp, &path)
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

    fn open_lock(&self, name: &WorktreeName) -> io::Result<File> {
        let path = self.state.worktree_lock(&self.id, name)?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
    }
}
