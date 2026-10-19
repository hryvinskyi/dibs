//! SQLite adapter for the store port.
//!
//! One database file holds every project. The file runs in WAL mode with foreign keys on, its schema
//! comes from the migrations embedded in this crate and is brought up to date on every open, and
//! every unit of work is a transaction opened with `BEGIN IMMEDIATE`: it takes the write lock at its
//! start, so a use case decides against state no other writer changes until it commits or drops.
//!
//! Every query is scoped by the project it is given; the lookups the port lets run without one
//! (a token or a browser session by its hash, a project by its id or slug, the sweep of expired
//! browser sessions) are the only ones that do not filter by it.

mod codec;
mod error;
mod rows;

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Sqlite, Transaction};

use dibs_app::ports::{Store, StoreResult, Tx};

pub use error::OpenError;

/// How many connections the store keeps open at most.
pub const POOL_SIZE: u32 = 8;

/// How long a statement waits for another connection's lock before it fails.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The store kept in one SQLite file.
#[derive(Debug, Clone)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Opens the database file at `path`, creating it when it does not exist, and applies every
    /// migration it has not had yet.
    ///
    /// # Errors
    /// [`OpenError::Connect`] when the file cannot be opened as a database, [`OpenError::Migrate`]
    /// when its schema cannot be brought up to date.
    pub async fn open(path: &Path) -> Result<Self, OpenError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let pool = SqlitePoolOptions::new()
            .max_connections(POOL_SIZE)
            .connect_with(options)
            .await
            .map_err(|error| OpenError::Connect(Box::new(error)))?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|error| OpenError::Migrate(Box::new(error)))?;
        Ok(Self { pool })
    }

    /// Waits for every transaction to end, then closes every connection.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

#[async_trait]
impl Store for SqliteStore {
    async fn begin(&self) -> StoreResult<Box<dyn Tx>> {
        let tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(error::store_error)?;
        Ok(Box::new(SqliteTx { tx }))
    }
}

/// A transaction of the SQLite store. Dropping it uncommitted rolls it back before its connection
/// serves anyone else.
pub(crate) struct SqliteTx {
    tx: Transaction<'static, Sqlite>,
}

#[async_trait]
impl Tx for SqliteTx {
    async fn commit(self: Box<Self>) -> StoreResult<()> {
        self.tx.commit().await.map_err(error::store_error)
    }
}
