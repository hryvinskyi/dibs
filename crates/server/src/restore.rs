//! Restoring a snapshot into a new database file, with no server running on it.
//!
//! A restore never replaces a database: it refuses a target that exists, and a target whose
//! write-ahead log or shared-memory file is still there, since SQLite would replay a stale log into
//! the restored file. It holds the target's lock for its whole run, so it refuses while a server
//! serves the target, and no server starts on the target until it is done. The snapshot is copied
//! under a hidden temporary name beside the target, opened there and brought up to the current
//! schema, closed, and only then renamed to the target, so the target appears whole and migrated
//! or not at all. The snapshot itself is never written to.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use dibs_store_sqlite::{OpenError, SqliteStore};

use crate::hold::{DatabaseHold, HoldError};
use crate::snapshot::sync_parent;

/// A database restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    /// The new database file.
    pub database: PathBuf,
    /// Its size in bytes.
    pub bytes: u64,
}

/// Why a restore refused or failed. Each message is a sentence for the operator.
#[derive(Debug, Error)]
pub enum RestoreError {
    /// The snapshot is not a file that can be read.
    #[error("there is no snapshot file at {0}")]
    NoSnapshot(PathBuf),
    /// The target, or a journal file of a database by its name, exists.
    #[error(
        "{0} exists; a restore writes a new database and never replaces one, so move it away first"
    )]
    TargetExists(PathBuf),
    /// A server, or another restore or migration, holds the target.
    #[error(
        "a running server or another restore holds {0}; stop it first, a restore is done offline"
    )]
    Held(PathBuf),
    /// The target's lock file could not be opened.
    #[error(
        "the lock file beside {0} could not be opened; check that its directory exists and is writable"
    )]
    Lock(PathBuf, #[source] HoldError),
    /// The snapshot could not be copied beside the target.
    #[error("the snapshot could not be copied into place beside {0}")]
    Copy(PathBuf, #[source] io::Error),
    /// The copy is not a database this server can open and migrate.
    #[error("{0} is not a database this server can open; nothing was restored")]
    NotADatabase(PathBuf, #[source] OpenError),
}

/// Restores the snapshot at `snapshot` into a new database at `target`.
///
/// # Errors
/// [`RestoreError`] saying why nothing was restored; the target does not exist afterwards.
pub async fn restore(snapshot: &Path, target: &Path) -> Result<Restored, RestoreError> {
    let hold = match DatabaseHold::take(target) {
        Ok(hold) => hold,
        Err(HoldError::Held { .. }) => return Err(RestoreError::Held(target.to_owned())),
        Err(error) => return Err(RestoreError::Lock(target.to_owned(), error)),
    };
    for existing in [
        target.to_owned(),
        with_suffix(target, "-wal"),
        with_suffix(target, "-shm"),
    ] {
        if existing.exists() {
            return Err(RestoreError::TargetExists(existing));
        }
    }
    if !snapshot.is_file() {
        return Err(RestoreError::NoSnapshot(snapshot.to_owned()));
    }
    let temporary = temporary_beside(target);
    remove_database(&temporary);
    let restored = restore_into(snapshot, &temporary, target).await;
    if restored.is_err() {
        remove_database(&temporary);
    }
    drop(hold);
    restored
}

async fn restore_into(
    snapshot: &Path,
    temporary: &Path,
    target: &Path,
) -> Result<Restored, RestoreError> {
    let copy_failed = |error| RestoreError::Copy(target.to_owned(), error);
    std::fs::copy(snapshot, temporary).map_err(copy_failed)?;
    std::fs::File::open(temporary)
        .and_then(|file| file.sync_all())
        .map_err(copy_failed)?;
    let store = SqliteStore::open(temporary)
        .await
        .map_err(|error| RestoreError::NotADatabase(snapshot.to_owned(), error))?;
    store.close().await;
    let bytes = std::fs::metadata(temporary).map_err(copy_failed)?.len();
    if target.exists() {
        return Err(RestoreError::TargetExists(target.to_owned()));
    }
    std::fs::rename(temporary, target).map_err(copy_failed)?;
    sync_parent(target).map_err(copy_failed)?;
    Ok(Restored {
        database: target.to_owned(),
        bytes,
    })
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The hidden name beside `target` a restore writes under.
fn temporary_beside(target: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(target.file_name().unwrap_or_default());
    name.push(".restoring");
    target.with_file_name(name)
}

/// Removes a database file and its journal files, if they exist.
fn remove_database(path: &Path) {
    for file in [
        path.to_owned(),
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
    ] {
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(file = %file.display(), error = %error, "a file of an unfinished restore could not be removed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_temporary_copy_is_hidden_beside_the_target() {
        assert_eq!(
            temporary_beside(Path::new("/data/dibs.db")),
            PathBuf::from("/data/.dibs.db.restoring")
        );
        assert_eq!(
            with_suffix(Path::new("/data/dibs.db"), "-wal"),
            PathBuf::from("/data/dibs.db-wal")
        );
    }
}
