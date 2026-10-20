//! The HTTP router: every route, the API's and the browser pages', the layers around them, and the
//! state the handlers share.
//!
//! Every answer carries `Cache-Control: no-store` unless its handler set its own, so a cache in
//! front never keeps one. A request body is read up to [`BODY_LIMIT`]; a request is answered within
//! the configured timeout or refused with `timeout` — for the event stream, the time until its
//! answer starts, not the stream that follows. The request log names the method and the path only,
//! never a header.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::error_handling::HandleErrorLayer;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Request, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use tower::ServiceBuilder;
use tower::timeout::TimeoutLayer;
use tower::timeout::error::Elapsed;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

use dibs_app::ports::{Clock, TokenGenerator};
use dibs_app::use_cases::UseCases;

use crate::adapters::{OsTokens, SystemClock};
use crate::client_ip::ClientIpPolicy;
use crate::error::ApiError;
use crate::rate_limit::{AuthLimiter, LimitPolicy};
use crate::routes;
use crate::stream::EventStreams;
use crate::ui;
use crate::versions::{self, ClientVersions, VERSION_ROUTE};

/// The largest request body read, in bytes. A message of 16 KiB grows on the wire — JSON escapes a
/// control character to six bytes, a form percent-encodes a non-ASCII byte to three — and still
/// fits with the other fields.
pub const BODY_LIMIT: usize = 128 * 1024;

/// The largest body of an importer's batch, in bytes: room for the most items a batch may report,
/// each with a long title, its hints and its blockers.
pub const IMPORT_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// How long a request may take when nothing else is configured.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How the router treats the network around it, and the clock and secrets its pages use.
#[derive(Clone)]
pub struct HttpOptions {
    /// How the client's address is told.
    pub client_ip: ClientIpPolicy,
    /// The limit on failed sign-ins.
    pub limits: LimitPolicy,
    /// How long a request may take.
    pub timeout: Duration,
    /// The time the pages show and count from.
    pub clock: Arc<dyn Clock>,
    /// Where the secrets of the sign-in form come from.
    pub tokens: Arc<dyn TokenGenerator>,
    /// The oldest client served and the client recommended.
    pub versions: ClientVersions,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            client_ip: ClientIpPolicy::new(None),
            limits: LimitPolicy::default(),
            timeout: REQUEST_TIMEOUT,
            clock: Arc::new(SystemClock),
            tokens: Arc::new(OsTokens),
            versions: ClientVersions::default(),
        }
    }
}

impl fmt::Debug for HttpOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpOptions")
            .field("client_ip", &self.client_ip)
            .field("limits", &self.limits)
            .field("timeout", &self.timeout)
            .field("versions", &self.versions)
            .finish_non_exhaustive()
    }
}

/// What every handler can reach.
#[derive(Clone)]
pub struct AppState {
    /// The use cases.
    pub(crate) app: Arc<UseCases>,
    /// The failed sign-ins per address.
    pub(crate) limiter: Arc<AuthLimiter>,
    /// How the client's address is told.
    pub(crate) client_ip: Arc<ClientIpPolicy>,
    /// The event streams.
    pub(crate) streams: EventStreams,
    /// The time the pages show and count from.
    pub(crate) clock: Arc<dyn Clock>,
    /// Where the secrets of the sign-in form come from.
    pub(crate) tokens: Arc<dyn TokenGenerator>,
    /// The oldest client served and the client recommended.
    pub(crate) versions: Arc<ClientVersions>,
}

/// The router over these use cases, its event streams woken and ended through `streams`.
pub fn router(app: Arc<UseCases>, streams: EventStreams, options: HttpOptions) -> Router {
    let versions = Arc::new(options.versions);
    let state = AppState {
        app,
        limiter: Arc::new(AuthLimiter::new(options.limits)),
        client_ip: Arc::new(options.client_ip),
        streams,
        clock: options.clock,
        tokens: options.tokens,
        versions: versions.clone(),
    };
    let api = Router::new()
        .route("/health", get(routes::health))
        .route(VERSION_ROUTE, get(routes::version::version))
        .route("/v1/me", get(routes::me::me))
        .route("/v1/members", get(routes::members::list))
        .route("/v1/sessions", post(routes::sessions::register))
        .route("/v1/heartbeat", post(routes::sessions::heartbeat))
        .route(
            "/v1/items",
            get(routes::items::list)
                .post(routes::items::create)
                .merge(put(routes::items::upsert).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT))),
        )
        .route("/v1/ready", get(routes::ready::list))
        .route("/v1/items/{id}", get(routes::items::get))
        .route(
            "/v1/items/{id}/dependencies",
            post(routes::dependencies::add).delete(routes::dependencies::remove),
        )
        .route("/v1/items/{id}/claim", post(routes::claims::claim))
        .route("/v1/items/{id}/claim/narrow", post(routes::claims::narrow))
        .route("/v1/items/{id}/takeover", post(routes::claims::take_over))
        .route("/v1/items/{id}/release", post(routes::claims::release))
        .route("/v1/items/{id}/done", post(routes::claims::done))
        .route("/v1/items/{id}/review", post(routes::review::start))
        .route("/v1/items/{id}/review/drop", post(routes::review::drop))
        .route(
            "/v1/items/{id}/footprint",
            put(routes::radar::footprint).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT)),
        )
        .route(
            "/v1/prs/merged",
            post(routes::review::merged).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT)),
        )
        .route("/v1/prs/closed", post(routes::review::closed))
        .route("/v1/prs/state", post(routes::reports::pr_state))
        .route("/v1/review-claims", get(routes::review::claims))
        .route("/v1/claims", get(routes::radar::claims))
        .route("/v1/pr-status", get(routes::review::status))
        .route("/v1/status", get(routes::status::status))
        .route("/v1/edit-check", post(routes::edit_check::check))
        .route("/v1/grants", post(routes::grants::grant))
        .route("/v1/grants/{id}", delete(routes::grants::revoke))
        .route(
            "/v1/threads",
            get(routes::threads::list).post(routes::threads::start),
        )
        .route("/v1/threads/{id}", get(routes::threads::read))
        .route("/v1/threads/{id}/messages", post(routes::threads::post))
        .route("/v1/threads/{id}/read", post(routes::threads::mark_read))
        .route("/v1/inbox", get(routes::threads::inbox))
        .route("/v1/messages/{id}/ack", post(routes::messages::ack))
        .route("/v1/messages/{id}/deliver", post(routes::messages::deliver))
        .route("/v1/messages/{id}/status", get(routes::messages::status))
        .route("/v1/slots", get(routes::slots::list))
        .route("/v1/slots/{name}", get(routes::slots::get))
        .route("/v1/slots/{name}/take", post(routes::slots::take))
        .route("/v1/slots/{name}/release", post(routes::slots::release))
        .route("/v1/slots/{name}/leave", post(routes::slots::leave))
        .route("/v1/slots/{name}/heartbeat", post(routes::slots::heartbeat))
        .route("/v1/slots/{name}/takeover", post(routes::slots::take_over))
        .route(
            "/v1/slots/{name}/offer/deliver",
            post(routes::slots::deliver_offer),
        )
        .route(
            "/v1/slots/{name}/run-finished",
            post(routes::slots::run_finished),
        )
        .route("/v1/events", get(routes::events::list))
        .route("/v1/events/stream", get(routes::events::stream))
        .route("/v1/escalations", post(routes::escalations::escalate))
        .route("/v1/feedback", post(routes::reports::feedback))
        .route("/v1/stats", get(routes::reports::stats))
        .route("/v1/admin/members", post(routes::admin::add_member))
        .route(
            "/v1/admin/members/{handle}",
            delete(routes::admin::remove_member),
        )
        .route(
            "/v1/admin/members/{handle}/tokens",
            post(routes::admin::rotate_token),
        )
        .route(
            "/v1/admin/threads/{id}",
            delete(routes::admin::delete_thread),
        )
        .route(
            "/v1/admin/tokens/{prefix}",
            delete(routes::admin::revoke_token),
        )
        .route("/v1/admin/settings", put(routes::admin::apply_settings))
        .route(
            "/v1/admin/items/{id}/force-release",
            post(routes::admin::force_release),
        )
        .route(
            "/v1/admin/slots/{name}/force-release",
            post(routes::slots::force_release),
        )
        .merge(ui::routes())
        .fallback(no_route)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state)
        .layer(middleware::from_fn_with_state(versions, versions::gate));
    api.layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(timed_out))
                .layer(TimeoutLayer::new(options.timeout)),
        )
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    tracing::info_span!(
                        "request",
                        method = %request.method(),
                        path = %request.uri().path(),
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .layer(CatchPanicLayer::custom(panicked))
}

async fn no_route() -> ApiError {
    ApiError::no_route()
}

async fn method_not_allowed() -> ApiError {
    ApiError::method_not_allowed()
}

async fn timed_out(error: tower::BoxError) -> ApiError {
    if error.is::<Elapsed>() {
        ApiError::timeout()
    } else {
        tracing::error!(error = %error, "a request failed in a layer");
        ApiError::internal()
    }
}

fn panicked(_: Box<dyn std::any::Any + Send + 'static>) -> Response {
    tracing::error!("a request handler panicked");
    ApiError::internal().into_response()
}
