//! How failures of the database become the store port's errors.

use thiserror::Error;

use dibs_app::ports::StoreError;

/// Why the store could not be opened. The text is for logs.
#[derive(Debug, Error)]
pub enum OpenError {
    /// The file could not be opened as a database.
    #[error("the database file could not be opened")]
    Connect(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The schema could not be brought up to date.
    #[error("the database schema could not be brought up to date")]
    Migrate(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Why a copy of the database or a checkpoint of its journal failed. The text is for logs.
#[derive(Debug, Error)]
pub enum MaintenanceError {
    /// The copy's path cannot be handed to the database, which takes paths as UTF-8 text.
    #[error("the path of the copy is not UTF-8 text")]
    PathNotUtf8,
    /// The database could not be copied.
    #[error("the database could not be copied")]
    Copy(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The journal could not be checkpointed.
    #[error("the write-ahead log could not be checkpointed")]
    Checkpoint(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// A database failure as the port reports it: a broken uniqueness rule is a duplicate, everything
/// else a failure of the backend.
pub(crate) fn store_error(error: sqlx::Error) -> StoreError {
    let duplicate = error
        .as_database_error()
        .is_some_and(|database| database.is_unique_violation());
    if duplicate {
        StoreError::Duplicate
    } else {
        StoreError::Backend(Box::new(error))
    }
}

/// A value the store cannot keep or a row it cannot read: what is wrong, for the logs.
pub(crate) fn corrupt(what: impl Into<String>) -> StoreError {
    StoreError::Corrupt { what: what.into() }
}

/// A value the schema has no room for, such as an id above the largest integer SQLite keeps.
pub(crate) fn unstorable(what: &str) -> StoreError {
    StoreError::Backend(Box::new(std::io::Error::other(format!(
        "{what} does not fit the store"
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_that_is_no_database_error_is_a_backend_failure() {
        assert!(matches!(
            store_error(sqlx::Error::RowNotFound),
            StoreError::Backend(_)
        ));
        assert!(matches!(
            corrupt("claims.areas"),
            StoreError::Corrupt { what } if what == "claims.areas"
        ));
        assert!(matches!(unstorable("an id"), StoreError::Backend(_)));
    }
}
