//! The composition root: the store, the adapters and the use cases built once from the
//! configuration, and the router served until the process is told to stop.

use std::net::SocketAddr;
use std::sync::Arc;

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
use crate::http::{HttpOptions, router};

/// How many wake-ups a slow follower of the event log may fall behind before it misses some.
const WAKEUP_BACKLOG: usize = 1024;

/// Why the server stopped before it could serve, or while serving.
#[derive(Debug, Error)]
pub enum ServeError {
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

/// Opens the store, binds the address and serves until `shutdown` resolves; then waits for the
/// requests in flight and closes the store.
///
/// # Errors
/// [`ServeError`] naming what failed.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let store = Arc::new(
        SqliteStore::open(&config.database)
            .await
            .map_err(ServeError::Database)?,
    );
    let notifier = Arc::new(BroadcastNotifier::new(WAKEUP_BACKLOG));
    let app = Arc::new(use_cases(store.clone(), notifier));
    let options = HttpOptions {
        client_ip: ClientIpPolicy::new(config.proxy),
        ..HttpOptions::default()
    };
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(ServeError::Bind)?;
    let address = listener.local_addr().map_err(ServeError::Bind)?;
    tracing::info!(address = %address, "listening");
    let service = router(app, options).into_make_service_with_connect_info::<SocketAddr>();
    let served = axum::serve(listener, service)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(ServeError::Serve);
    store.close().await;
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
