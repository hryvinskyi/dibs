//! What an operator does to the database file besides using it: a consistent copy while it serves,
//! and a checkpoint of its write-ahead log before it stops.

use std::path::Path;

use crate::SqliteStore;
use crate::error::MaintenanceError;

/// What a checkpoint of the write-ahead log did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    /// Every frame of the log reached the database and the log was emptied. When another
    /// connection was still reading or writing, the log keeps what it could not move.
    pub complete: bool,
    /// How many frames the log held.
    pub frames: i64,
    /// How many of them reached the database.
    pub moved: i64,
}

impl SqliteStore {
    /// Writes a consistent copy of the whole database into a new file at `target` with
    /// `VACUUM INTO`. The copy reads one snapshot of the database as any reader does: in WAL mode
    /// writers carry on meanwhile, and nothing written after the copy began is in it. The file must
    /// not exist yet.
    ///
    /// # Errors
    /// [`MaintenanceError::PathNotUtf8`] when `target` is not UTF-8 text,
    /// [`MaintenanceError::Copy`] when the copy cannot be written (an existing file, a missing or
    /// read-only directory, a full disk).
    pub async fn vacuum_into(&self, target: &Path) -> Result<(), MaintenanceError> {
        let target = target.to_str().ok_or(MaintenanceError::PathNotUtf8)?;
        sqlx::query("VACUUM INTO ?1")
            .bind(target)
            .execute(&self.pool)
            .await
            .map_err(|error| MaintenanceError::Copy(Box::new(error)))?;
        Ok(())
    }

    /// Moves every frame of the write-ahead log into the database and empties the log, as far as
    /// the connections still in use let it.
    ///
    /// # Errors
    /// [`MaintenanceError::Checkpoint`] when the database refuses the checkpoint.
    pub async fn checkpoint(&self) -> Result<Checkpoint, MaintenanceError> {
        let (busy, frames, moved): (i64, i64, i64) =
            sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
                .fetch_one(&self.pool)
                .await
                .map_err(|error| MaintenanceError::Checkpoint(Box::new(error)))?;
        Ok(Checkpoint {
            complete: busy == 0,
            frames,
            moved,
        })
    }
}
