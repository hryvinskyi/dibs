//! The real router over the SQLite store on a temporary file. The clock, ids and secrets are the
//! fakes, so a test can move time; everything else is the production wiring, the notifier that
//! wakes the event streams included.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use time::OffsetDateTime;
use time::macros::datetime;
use tower::ServiceExt;

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

/// When every test starts.
pub const START: OffsetDateTime = datetime!(2026-03-02 09:00:00 UTC);

/// The address requests come from unless a test says otherwise.
pub const PEER: &str = "203.0.113.10:50000";

/// One member signed in.
#[derive(Clone)]
pub struct Person {
    /// Their token.
    pub token: String,
    /// What the token authenticates.
    pub actor: Actor,
}

/// A database file, the use cases over it, and the router.
pub struct World {
    /// The fakes behind the clock, ids and secrets.
    pub fakes: Fakes,
    /// The store.
    pub store: Arc<SqliteStore>,
    /// The use cases, for setting up what no route of the router does.
    pub app: Arc<UseCases>,
    /// The router.
    pub router: Router,
    /// Ends the router's event streams.
    pub shutdown: Arc<Shutdown>,
    _dir: TempDir,
}

/// An answer: status, headers and JSON body (`Null` when the body is empty).
pub struct Answer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

impl World {
    /// A world with the default network options.
    pub async fn new() -> Self {
        Self::with_options(HttpOptions::default()).await
    }

    /// A world with these network options.
    pub async fn with_options(options: HttpOptions) -> Self {
        Self::with(options, StreamOptions::default()).await
    }

    /// A world with these network and stream options.
    pub async fn with(options: HttpOptions, stream: StreamOptions) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            SqliteStore::open(&dir.path().join("dibs.db"))
                .await
                .unwrap(),
        );
        let fakes = Fakes::new(START);
        let notifier = Arc::new(BroadcastNotifier::new(64));
        let deps = Dependencies {
            store: store.clone(),
            notifier: notifier.clone(),
            ..fakes.dependencies()
        };
        let app = Arc::new(UseCases::new(&deps));
        let shutdown = Arc::new(Shutdown::new());
        let streams = EventStreams::new(notifier, shutdown.signal(), stream);
        let router = router(app.clone(), streams, options);
        Self {
            fakes,
            store,
            app,
            router,
            shutdown,
            _dir: dir,
        }
    }

    /// A new project with its admin.
    pub async fn project(&self, slug: &str, admin: &str) -> Person {
        let created = self
            .app
            .init_project
            .execute(InitProjectInput {
                slug: ProjectSlug::parse(slug).unwrap(),
                admin: new_member(admin),
            })
            .await
            .unwrap();
        self.sign_in(created.token.expose()).await
    }

    /// A new member of the admin's project.
    pub async fn member(&self, admin: &Person, handle: &str, role: Role) -> Person {
        let created = self
            .app
            .create_member
            .execute(
                &admin.actor,
                CreateMemberInput {
                    member: new_member(handle),
                    role,
                },
            )
            .await
            .unwrap();
        self.sign_in(created.token.expose()).await
    }

    /// Applies a settings document written as JSON.
    pub async fn settings(&self, admin: &Person, document: Value) {
        let document: SettingsDocument = serde_json::from_value(document).unwrap();
        self.app
            .apply_settings
            .execute(&admin.actor, document)
            .await
            .unwrap();
    }

    async fn sign_in(&self, token: &str) -> Person {
        let actor = self.app.authenticate.execute(token).await.unwrap();
        Person {
            token: token.to_owned(),
            actor,
        }
    }

    /// Sends a request from [`PEER`].
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Answer {
        let request = request(method, path, token, body.map(|b| b.to_string()));
        self.send(request, PEER).await
    }

    /// `GET` as `who`.
    pub async fn get(&self, who: &Person, path: &str) -> Answer {
        self.call(Method::GET, path, Some(&who.token), None).await
    }

    /// `POST` with a JSON body as `who`.
    pub async fn post(&self, who: &Person, path: &str, body: Value) -> Answer {
        self.call(Method::POST, path, Some(&who.token), Some(body))
            .await
    }

    /// `PUT` with a JSON body as `who`.
    pub async fn put(&self, who: &Person, path: &str, body: Value) -> Answer {
        self.call(Method::PUT, path, Some(&who.token), Some(body))
            .await
    }

    /// `DELETE` as `who`.
    pub async fn delete(&self, who: &Person, path: &str) -> Answer {
        self.call(Method::DELETE, path, Some(&who.token), None)
            .await
    }

    /// Sends a request from [`PEER`] and answers the response itself, its body unread, for answers
    /// that stream.
    pub async fn open(&self, mut request: Request<Body>) -> axum::response::Response {
        let peer: SocketAddr = PEER.parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        self.router.clone().oneshot(request).await.unwrap()
    }

    /// Serves the router on an ephemeral port of the loopback address until the world's shutdown is
    /// given; answers the address and the task that ends once serving stopped.
    pub async fn serve(&self) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let service = self
            .router
            .clone()
            .into_make_service_with_connect_info::<SocketAddr>();
        let mut stop = self.shutdown.signal();
        let served = tokio::spawn(async move {
            axum::serve(listener, service)
                .with_graceful_shutdown(async move { stop.stopped().await })
                .await
                .unwrap();
        });
        (address, served)
    }

    /// Starts a thread as `who` and answers its id.
    pub async fn thread(&self, who: &Person, to: Value, subject: &str, body: &str) -> u64 {
        let mut request = serde_json::json!({
            "subject": subject, "label": "laptop/mcp", "body": body,
        });
        if to == "project" {
            request["to_project"] = Value::Bool(true);
        } else {
            request["to"] = to;
        }
        let answer = self.post(who, "/v1/threads", request).await;
        assert_eq!(answer.status, StatusCode::CREATED, "{}", answer.body);
        answer.body["thread"]["id"].as_u64().unwrap()
    }

    /// Sends a request built by the test, as if from `peer`.
    pub async fn send(&self, mut request: Request<Body>, peer: &str) -> Answer {
        let peer: SocketAddr = peer.parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        Answer {
            status,
            headers,
            body,
        }
    }

    /// Registers a session of `who` and answers its id.
    pub async fn session(&self, who: &Person, host: &str) -> u64 {
        let answer = self
            .post(
                who,
                "/v1/sessions",
                serde_json::json!({"host": host, "label": format!("{host}/mcp"), "agent": "agent"}),
            )
            .await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        answer.body["session_id"].as_u64().unwrap()
    }

    /// Creates an item and answers its id.
    pub async fn item(&self, who: &Person, reference: Option<&str>, title: &str) -> u64 {
        let mut body = serde_json::json!({ "title": title });
        if let Some(reference) = reference {
            body["ref"] = Value::from(reference);
        }
        let answer = self.post(who, "/v1/items", body).await;
        assert!(answer.status.is_success(), "{}", answer.body);
        answer.body["item"]["id"].as_u64().unwrap()
    }
}

/// A request with an optional bearer token and an optional JSON body.
pub fn request(
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<String>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

/// A member about to join.
pub fn new_member(handle: &str) -> NewMember {
    NewMember {
        handle: Handle::parse(handle).unwrap(),
        display_name: DisplayName::parse(handle).unwrap(),
        external_login: None,
    }
}

/// Settings with two repositories: `api` with the areas `billing` and `search` and a shared
/// lock file, `web` with `checkout` and its own `billing`.
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
                "areas": [
                    {"name": "checkout", "paths": ["app/checkout/**"]},
                    {"name": "billing", "paths": ["app/billing/**"]}
                ]
            }
        ]
    })
}

/// Asserts the exact error shape: `{"error": {"code", "message"}}` and nothing else but `details`.
pub fn assert_error(answer: &Answer, status: StatusCode, code: &str) {
    assert_eq!(answer.status, status, "{}", answer.body);
    let error = answer.body["error"].as_object().unwrap();
    assert_eq!(error["code"], code, "{}", answer.body);
    let message = error["message"].as_str().unwrap();
    assert!(message.ends_with('.') && !message.is_empty(), "{message}");
    assert!(
        error
            .keys()
            .all(|key| ["code", "message", "details"].contains(&key.as_str())),
        "{}",
        answer.body
    );
    assert_eq!(answer.body.as_object().unwrap().len(), 1, "{}", answer.body);
    assert_eq!(answer.headers[header::CONTENT_TYPE], "application/json");
}
