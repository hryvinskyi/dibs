//! The real dibs router over the SQLite store on a temporary file, served on an ephemeral port of
//! the loopback address, for the tests of the crates that talk to it over HTTP. The clock, ids and
//! secrets are the fakes, for the use cases and the router alike, so a test can move time;
//! everything else is the production wiring.
//!
//! Every request the router receives is logged by its path before it is answered, so a test can
//! prove that something made no request, and every answer can be held back by a delay the test
//! sets, so a test can stand in for a slow server.
//!
//! The server runs on a runtime of its own, so a test may drive it from plain synchronous code (a
//! child process it spawns) or from a runtime of its own. Setting up projects and members goes
//! through the use cases directly, as no route creates a project.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration as StdDuration;

use axum::extract::{Request, State};
use axum::middleware::{self, Next};
use axum::response::Response;

use serde_json::Value;
use tempfile::TempDir;
use time::macros::datetime;
use time::{Duration, OffsetDateTime};
use tokio::runtime::Runtime;

use dibs_app::fakes::Fakes;
use dibs_app::use_cases::{CreateMemberInput, InitProjectInput, NewMember, UseCases};
use dibs_app::{Actor, Dependencies};
use dibs_domain::handle::Handle;
use dibs_domain::member::{DisplayName, Role};
use dibs_domain::project::ProjectSlug;
use dibs_domain::settings::SettingsDocument;
use dibs_server::adapters::BroadcastNotifier;
use dibs_server::http::{HttpOptions, router};
use dibs_server::stream::{EventStreams, Shutdown, StreamOptions};
use dibs_store_sqlite::SqliteStore;

/// When every test server's clock starts.
pub const START: OffsetDateTime = datetime!(2026-03-02 09:00:00 UTC);

/// One member, signed in.
#[derive(Clone)]
pub struct Person {
    /// Their handle.
    pub handle: String,
    /// Their token.
    pub token: String,
    actor: Actor,
}

/// Why a test server or one of its fixtures could not be set up.
#[derive(Debug)]
pub struct SetupError(String);

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SetupError {}

/// The result of setting something up.
pub type Setup<T> = Result<T, SetupError>;

/// Turns a failure into a [`SetupError`] saying what could not be set up.
fn failed<E: std::fmt::Debug>(what: &'static str) -> impl FnOnce(E) -> SetupError {
    move |error| SetupError(format!("{what}: {error:?}"))
}

/// The requests a server received and how long it holds back each answer.
#[derive(Default)]
struct Traffic {
    paths: Mutex<Vec<String>>,
    whole: Mutex<Vec<String>>,
    delay_ms: AtomicU64,
}

impl Traffic {
    fn paths(&self) -> MutexGuard<'_, Vec<String>> {
        self.paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn whole(&self) -> MutexGuard<'_, Vec<String>> {
        self.whole
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The most of a request body the log keeps.
const LOGGED_BODY: usize = 1024 * 1024;

/// Logs the request's path, and the whole of it — method, path, query, headers and body — waits
/// the delay, then lets the router answer.
async fn record(State(traffic): State<Arc<Traffic>>, request: Request, next: Next) -> Response {
    traffic.paths().push(request.uri().path().to_owned());
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, LOGGED_BODY)
        .await
        .unwrap_or_default();
    let headers: Vec<String> = parts
        .headers
        .iter()
        .map(|(name, value)| format!("{name}: {}", String::from_utf8_lossy(value.as_bytes())))
        .collect();
    traffic.whole().push(format!(
        "{} {}\n{}\n\n{}",
        parts.method,
        parts.uri,
        headers.join("\n"),
        String::from_utf8_lossy(&bytes)
    ));
    let request = Request::from_parts(parts, axum::body::Body::from(bytes));
    let delay = traffic.delay_ms.load(Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(StdDuration::from_millis(delay)).await;
    }
    next.run(request).await
}

/// A running server.
pub struct TestServer {
    runtime: Runtime,
    fakes: Fakes,
    app: Arc<UseCases>,
    shutdown: Arc<Shutdown>,
    address: SocketAddr,
    traffic: Arc<Traffic>,
    _dir: TempDir,
}

impl TestServer {
    /// Starts a server on an ephemeral port with an empty database.
    ///
    /// # Errors
    /// When the runtime, the database or the port cannot be set up.
    pub fn start() -> Setup<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(failed("a runtime for the test server"))?;
        let dir = tempfile::tempdir().map_err(failed("a directory for the database"))?;
        let fakes = Fakes::new(START);
        let path = dir.path().join("dibs.db");
        let traffic = Arc::new(Traffic::default());
        let logged = traffic.clone();
        let (app, shutdown, address) = runtime.block_on(async {
            let result: Setup<_> = async {
                let store = Arc::new(
                    SqliteStore::open(&path)
                        .await
                        .map_err(failed("the database opens"))?,
                );
                let notifier = Arc::new(BroadcastNotifier::new(64));
                let deps = Dependencies {
                    store,
                    notifier: notifier.clone(),
                    ..fakes.dependencies()
                };
                let app = Arc::new(UseCases::new(&deps));
                let shutdown = Arc::new(Shutdown::new());
                let streams = EventStreams::new(
                    notifier,
                    shutdown.signal(),
                    StreamOptions {
                        heartbeat: StdDuration::from_secs(1),
                        ..StreamOptions::default()
                    },
                );
                let options = HttpOptions {
                    clock: fakes.clock.clone(),
                    tokens: fakes.tokens.clone(),
                    ..HttpOptions::default()
                };
                let service = router(app.clone(), streams, options)
                    .layer(middleware::from_fn_with_state(logged, record))
                    .into_make_service_with_connect_info::<SocketAddr>();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .map_err(failed("an ephemeral port"))?;
                let address = listener.local_addr().map_err(failed("the bound address"))?;
                let mut stop = shutdown.signal();
                tokio::spawn(async move {
                    let _ = axum::serve(listener, service)
                        .with_graceful_shutdown(async move { stop.stopped().await })
                        .await;
                });
                Ok((app, shutdown, address))
            }
            .await;
            result
        })?;
        Ok(Self {
            runtime,
            fakes,
            app,
            shutdown,
            address,
            traffic,
            _dir: dir,
        })
    }

    /// How many requests the server has received.
    pub fn requests(&self) -> usize {
        self.traffic.paths().len()
    }

    /// How many requests the server has received for this path (`/v1/heartbeat`).
    pub fn requests_to(&self, path: &str) -> usize {
        self.traffic
            .paths()
            .iter()
            .filter(|seen| *seen == path)
            .count()
    }

    /// The paths of every request received, oldest first.
    pub fn request_paths(&self) -> Vec<String> {
        self.traffic.paths().clone()
    }

    /// Every request received as text, oldest first: its method, path and query, its headers and
    /// its body.
    pub fn request_log(&self) -> Vec<String> {
        self.traffic.whole().clone()
    }

    /// Holds back every answer from now on by `delay`; zero answers at once again.
    pub fn delay_answers(&self, delay: StdDuration) {
        let millis = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
        self.traffic.delay_ms.store(millis, Ordering::SeqCst);
    }

    /// The server's base URL, `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// A new project with its admin.
    ///
    /// # Errors
    /// When the slug or handle is invalid or taken.
    pub fn project(&self, slug: &str, admin: &str) -> Setup<Person> {
        let created = self
            .runtime
            .block_on(self.app.init_project.execute(InitProjectInput {
                slug: ProjectSlug::parse(slug).map_err(failed("a project slug"))?,
                admin: new_member(admin)?,
            }))
            .map_err(failed("the project is created"))?;
        self.sign_in(admin, created.token.expose())
    }

    /// A new member of the admin's project.
    ///
    /// # Errors
    /// When the handle is invalid or taken.
    pub fn member(&self, admin: &Person, handle: &str, role: Role) -> Setup<Person> {
        let created = self
            .runtime
            .block_on(self.app.create_member.execute(
                &admin.actor,
                CreateMemberInput {
                    member: new_member(handle)?,
                    role,
                },
            ))
            .map_err(failed("the member is created"))?;
        self.sign_in(handle, created.token.expose())
    }

    /// Applies a settings document written as JSON.
    ///
    /// # Errors
    /// When the document is refused.
    pub fn settings(&self, admin: &Person, document: Value) -> Setup<()> {
        let document: SettingsDocument =
            serde_json::from_value(document).map_err(failed("a settings document"))?;
        self.runtime
            .block_on(self.app.apply_settings.execute(&admin.actor, document))
            .map_err(failed("the settings are applied"))?;
        Ok(())
    }

    /// Moves the server's clock forward.
    pub fn advance(&self, by: Duration) {
        self.fakes.clock.advance(by);
    }

    fn sign_in(&self, handle: &str, token: &str) -> Setup<Person> {
        let actor = self
            .runtime
            .block_on(self.app.authenticate.execute(token))
            .map_err(failed("the token signs in"))?;
        Ok(Person {
            handle: handle.to_owned(),
            token: token.to_owned(),
            actor,
        })
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.stop();
    }
}

fn new_member(handle: &str) -> Setup<NewMember> {
    Ok(NewMember {
        handle: Handle::parse(handle).map_err(failed("a handle"))?,
        display_name: DisplayName::parse(handle).map_err(failed("a display name"))?,
        external_login: None,
    })
}

/// Settings with two repositories: `api` (`https://github.com/acme/api.git`) with the areas
/// `billing` and `search` and a shared lock file, and `web` (`git@github.com:acme/web.git`) with
/// `checkout`.
pub fn two_repositories() -> Value {
    serde_json::json!({
        "stale_after": "1h",
        "repos": [
            {
                "id": "api",
                "remote": "https://github.com/acme/api.git",
                "areas": [
                    {"name": "billing", "paths": ["src/billing/**"]},
                    {"name": "search", "paths": ["src/search/**"]}
                ],
                "hot": [{"paths": ["Cargo.lock"], "kind": "warn-shared", "note": "merge with care"}]
            },
            {
                "id": "web",
                "remote": "git@github.com:acme/web.git",
                "areas": [{"name": "checkout", "paths": ["app/checkout/**"]}]
            }
        ]
    })
}

/// A server that answers every request with one fixed response, as a proxy or a challenge page in
/// front of the real one would. Answers its base URL; it serves until the process ends.
///
/// # Errors
/// When no port can be bound.
pub fn fixed_answer(status: u16, content_type: &str, body: &str) -> Setup<String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(failed("an ephemeral port"))?;
    let address = listener.local_addr().map_err(failed("the bound address"))?;
    let response = format!(
        "HTTP/1.1 {status} Fixed\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Ok(format!("http://{address}"))
}

/// A server that accepts connections and never answers. Answers its base URL; it listens until
/// the process ends.
///
/// # Errors
/// When no port can be bound.
pub fn silent() -> Setup<String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(failed("an ephemeral port"))?;
    let address = listener.local_addr().map_err(failed("the bound address"))?;
    std::thread::spawn(move || {
        let mut open = Vec::new();
        for stream in listener.incoming() {
            open.extend(stream.ok());
        }
    });
    Ok(format!("http://{address}"))
}

/// A base URL nothing listens on.
///
/// # Errors
/// When no port can be bound.
pub fn unreachable() -> Setup<String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(failed("an ephemeral port"))?;
    let address = listener.local_addr().map_err(failed("the bound address"))?;
    drop(listener);
    Ok(format!("http://{address}"))
}
