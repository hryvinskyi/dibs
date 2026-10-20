//! The row traits of the store port, one module per kind of record, each a set of SQL statements
//! run inside the transaction.

mod claims;
mod dependencies;
mod events;
mod grants;
mod items;
mod members;
mod projects;
mod sessions;
mod settings;
mod threads;
mod tokens;
mod ui_sessions;
mod wide_asks;

use sqlx::Sqlite;
use sqlx::query::Query;
use sqlx::sqlite::{SqliteArguments, SqliteRow};

use dibs_app::ports::StoreResult;

use crate::SqliteTx;
use crate::error::store_error;

/// A statement with its arguments bound.
type Statement<'q> = Query<'q, Sqlite, SqliteArguments>;

impl SqliteTx {
    /// Runs a statement that returns no rows; answers how many rows it changed.
    async fn run(&mut self, statement: Statement<'_>) -> StoreResult<u64> {
        statement
            .execute(&mut *self.tx)
            .await
            .map(|done| done.rows_affected())
            .map_err(store_error)
    }

    /// Every row a query returns.
    async fn all(&mut self, statement: Statement<'_>) -> StoreResult<Vec<SqliteRow>> {
        statement
            .fetch_all(&mut *self.tx)
            .await
            .map_err(store_error)
    }

    /// The one row a query returns, if it returns any.
    async fn optional(&mut self, statement: Statement<'_>) -> StoreResult<Option<SqliteRow>> {
        statement
            .fetch_optional(&mut *self.tx)
            .await
            .map_err(store_error)
    }

    /// The row a query always returns.
    async fn one(&mut self, statement: Statement<'_>) -> StoreResult<SqliteRow> {
        statement
            .fetch_one(&mut *self.tx)
            .await
            .map_err(store_error)
    }
}
