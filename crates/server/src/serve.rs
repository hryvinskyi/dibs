//! The composition root: the store, the adapters and the use cases built once from the
//! configuration, and the router served until the process is told to stop.
//!
//! A server holds its database file for as long as it runs (see [`crate::hold`]), so a second
//! server, a restore or a migration refuses that file meanwhile. Stopping goes in this order: the
//! listener closes, so no connection is accepted any more; the event streams end; the requests in
//! flight are given [`SHUTDOWN_GRACE`] to finish; the sweep stops, finishing a sweep it is
//! running, the retention stops, finishing a pass it is running, and the snapshot schedule stops,
//! finishing a snapshot it is writing; the write-ahead log is checkpointed into the database; the
//! store closes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::net::TcpListener;

use dibs_app::Dependencies;
use dibs_app::use_cases::UseCases;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_store_sqlite::SqliteStore;

use crate::adapters::{BroadcastNotifier, OsTokens, Sha256Hasher, SystemClock, TimeIds};
use crate::client_ip::ClientIpPolicy;
use crate::config::Config;
use crate::hold::{DatabaseHold, HoldError};
use crate::http::{HttpOptions, router};
use crate::retention::{RETAIN_EVERY, run_retention};
use crate::snapshot::{Snapshots, run_schedule};
use crate::stream::{EventStreams, Shutdown, StreamOptions};
use crate::sweep::{SWEEP_EVERY, run_sweeper};

/// How many wake-ups a slow follower of the event log may fall behind before it misses some.
const WAKEUP_BACKLOG: usize = 1024;

/// How long the requests in flight are given to finish once the server is told to stop.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Why the server stopped before it could serve, or while serving.
#[derive(Debug, Error)]
pub enum ServeError {
    /// Another process holds the database file.
    #[error("another server, a restore or a migration holds the database named by DIBS_DATABASE")]
    Held(#[source] HoldError),
    /// The lock file beside the database could not be opened.
    #[error("the lock file beside the database named by DIBS_DATABASE could not be opened")]
    Lock(#[source] HoldError),
    /// The database could not be opened or migrated.
    #[error("the database named by DIBS_DATABASE could not be opened")]
    Database(#[source] dibs_store_sqlite::OpenError),
    /// The address could not be bound.
    #[error("the address named by DIBS_LISTEN could not be bound")]
    Bind(#[source] std::io::Error),
    /// Serving failed.
    #[error("the server stopped on an error")]
    Serve(#[source] std::io::Error),
}

/// The use cases over a store, with the production adapters of every other port.
pub fn use_cases(store: Arc<SqliteStore>, notifier: Arc<BroadcastNotifier>) -> UseCases {
    let deps = Dependencies {
        store,
        clock: Arc::new(SystemClock),
        ids: Arc::new(TimeIds::default()),
        tokens: Arc::new(OsTokens),
        hasher: Arc::new(Sha256Hasher),
        notifier,
        hot_rules: Arc::new(HotRuleRegistry::standard()),
        ref_schemes: Arc::new(RefSchemeRegistry::standard()),
    };
    UseCases::new(&deps)
}

/// Takes the hold on the database, opens the store, starts the snapshot schedule when snapshots
/// are configured, binds the address and serves until `shutdown` resolves; then stops in the order
/// the module describes.
///
/// # Errors
/// [`ServeError`] naming what failed.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let hold = DatabaseHold::take(&config.database).map_err(|error| match error {
        HoldError::Held { .. } => ServeError::Held(error),
        HoldError::Io { .. } => ServeError::Lock(error),
    })?;
    let store = Arc::new(
        SqliteStore::open(&config.database)
            .await
            .map_err(ServeError::Database)?,
    );
    let notifier = Arc::new(BroadcastNotifier::new(WAKEUP_BACKLOG));
    let app = Arc::new(use_cases(store.clone(), notifier.clone()));
    let stopping = Shutdown::new();
    let sweeper = tokio::spawn(run_sweeper(app.clone(), SWEEP_EVERY, stopping.signal()));
    let retention = tokio::spawn(run_retention(app.clone(), RETAIN_EVERY, stopping.signal()));
    let schedule = match &config.snapshots {
        Some(snapshots) => {
            let every = snapshots.every;
            let snapshots = Arc::new(Snapshots::new(
                store.clone(),
                Arc::new(SystemClock),
                snapshots,
            ));
            Some(tokio::spawn(run_schedule(
                snapshots,
                every,
                stopping.signal(),
            )))
        }
        None => {
            tracing::warn!("snapshots are off; set DIBS_SNAPSHOT_DIR to take them");
            None
        }
    };
    let streams = EventStreams::new(notifier, stopping.signal(), StreamOptions::default());
    let options = HttpOptions {
        client_ip: ClientIpPolicy::new(config.proxy),
        versions: config.versions,
        ..HttpOptions::default()
    };
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(ServeError::Bind)?;
    let address = listener.local_addr().map_err(ServeError::Bind)?;
    tracing::info!(address = %address, "listening");
    let service = router(app, streams, options).into_make_service_with_connect_info::<SocketAddr>();
    let mut began = stopping.signal();
    let serving = axum::serve(listener, service)
        .with_graceful_shutdown(async move { began.stopped().await })
        .into_future();
    tokio::pin!(serving);
    let served = tokio::select! {
        served = &mut serving => served.map_err(ServeError::Serve),
        () = shutdown => {
            tracing::info!(grace_s = SHUTDOWN_GRACE.as_secs(), "stopping: no new connections, the event streams end, requests in flight may finish");
            stopping.stop();
            match tokio::time::timeout(SHUTDOWN_GRACE, &mut serving).await {
                Ok(served) => served.map_err(ServeError::Serve),
                Err(_) => {
                    tracing::warn!("requests were still running when the grace period ended; stopping without them");
                    Ok(())
                }
            }
        }
    };
    stopping.stop();
    if let Err(error) = sweeper.await {
        tracing::error!(error = %error, "the sweep ended abnormally");
    }
    if let Err(error) = retention.await {
        tracing::error!(error = %error, "the retention ended abnormally");
    }
    if let Some(schedule) = schedule
        && let Err(error) = schedule.await
    {
        tracing::error!(error = %error, "the snapshot schedule ended abnormally");
    }
    match store.checkpoint().await {
        Ok(checkpoint) if checkpoint.complete => {
            tracing::info!(
                frames = checkpoint.frames,
                "the write-ahead log is checkpointed"
            );
        }
        Ok(checkpoint) => {
            tracing::warn!(
                frames = checkpoint.frames,
                moved = checkpoint.moved,
                "the write-ahead log is checkpointed in part; a connection was still in use"
            );
        }
        Err(error) => {
            tracing::error!(error = %error, cause = ?error, "the write-ahead log could not be checkpointed");
        }
    }
    store.close().await;
    drop(hold);
    tracing::info!("stopped");
    served
}

/// Resolves when the process is asked to stop: Ctrl-C, or `SIGTERM` where there are signals.
pub async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}
