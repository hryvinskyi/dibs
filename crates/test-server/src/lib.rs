//! The real dibs router over the SQLite store on a temporary file, served on an ephemeral port of
//! the loopback address, for the tests of the crates that talk to it over HTTP. The clock, ids and
//! secrets are the fakes, so a test can move time; everything else is the production wiring.
//!
//! The server runs on a runtime of its own, so a test may drive it from plain synchronous code (a
//! child process it spawns) or from a runtime of its own. Setting up projects and members goes
//! through the use cases directly, as no route creates a project.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration as StdDuration;

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

/// A running server.
pub struct TestServer {
    runtime: Runtime,
    fakes: Fakes,
    app: Arc<UseCases>,
    shutdown: Arc<Shutdown>,
    address: SocketAddr,
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
                let service = router(app.clone(), streams, HttpOptions::default())
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
            _dir: dir,
        })
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
