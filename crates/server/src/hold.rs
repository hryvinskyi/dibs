//! The hold a process keeps on a database file while it must be the only one changing the file as a
//! whole: a server for as long as it serves, a restore or a migration while it runs.
//!
//! SQLite lets many processes share a file in WAL mode, so its own locks cannot tell whether a
//! server is running on it. The hold is an exclusive advisory lock on a file beside the database,
//! named after it with `.lock` appended. The operating system releases the lock when the process
//! ends, however it ends, so a crashed server leaves no hold behind; the lock file itself stays and
//! is reused. Processes that only read or write rows through SQLite, such as the admin commands, do
//! not take it.

use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Why the hold could not be taken.
#[derive(Debug, Error)]
pub enum HoldError {
    /// Another process holds the database: a running server, a restore or a migration.
    #[error(
        "another process holds the database {database}: a running server, a restore or a migration"
    )]
    Held {
        /// The database file.
        database: PathBuf,
    },
    /// The lock file could not be opened or locked.
    #[error("the lock file {lock} beside the database could not be opened")]
    Io {
        /// The lock file.
        lock: PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
}

/// An exclusive hold on a database file, released when dropped or when the process ends.
#[derive(Debug)]
pub struct DatabaseHold {
    file: File,
    lock: PathBuf,
}

impl DatabaseHold {
    /// The lock file of the database at `database`.
    pub fn lock_path(database: &Path) -> PathBuf {
        let mut name: OsString = database.as_os_str().to_owned();
        name.push(".lock");
        PathBuf::from(name)
    }

    /// Takes the hold on the database at `database`, which need not exist yet. It does not wait.
    ///
    /// # Errors
    /// [`HoldError::Held`] when another process holds it; [`HoldError::Io`] when the lock file
    /// cannot be opened or locked, such as in a directory that does not exist.
    pub fn take(database: &Path) -> Result<Self, HoldError> {
        let lock = Self::lock_path(database);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)
            .map_err(|source| HoldError::Io {
                lock: lock.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file, lock }),
            Err(TryLockError::WouldBlock) => Err(HoldError::Held {
                database: database.to_owned(),
            }),
            Err(TryLockError::Error(source)) => Err(HoldError::Io { lock, source }),
        }
    }

    /// The lock file this hold locks.
    pub fn lock(&self) -> &Path {
        &self.lock
    }
}

impl Drop for DatabaseHold {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::warn!(error = %error, "the hold on the database could not be released; it ends with the process");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_file_sits_beside_the_database() {
        assert_eq!(
            DatabaseHold::lock_path(Path::new("/data/dibs.db")),
            PathBuf::from("/data/dibs.db.lock")
        );
    }
}
