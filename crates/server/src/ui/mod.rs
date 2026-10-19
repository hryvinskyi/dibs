//! The browser pages for people: sign-in with a token, the status page, the board, the threads and
//! a thread with its reply form, the event stream over the session cookie, and the static bundle.
//!
//! Pages are rendered on the server from the templates, which hold every word a page shows; the
//! scripts only enhance them. Every answer carries a content security policy that allows nothing but
//! this service's own scripts, styles and connections, so no inline script, style or handler runs.
//! Every form posts a token of the session it was rendered for, and the session cookie is
//! `SameSite=Strict`. Every text a person or an agent wrote is escaped by the templates.

mod assets;
mod board;
mod chrome;
mod cookies;
mod live;
mod moments;
mod notice;
mod render;
mod rows;
mod session;
mod sign_in;
mod status;
mod takeover;
mod threads;
mod zone;

use axum::Router;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::http::AppState;

pub use assets::BUNDLE_HASH;
pub use session::SESSION_COOKIE;
pub use sign_in::LOGIN_COOKIE;
pub use zone::ZONE_COOKIE;

use session::UiSession;

/// The content security policy of every page answer.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// The routes of the pages, with the headers every answer of theirs carries.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui", get(to_status))
        .route("/ui/", get(status::page))
        .route("/ui/login", get(sign_in::form).post(sign_in::sign_in))
        .route("/ui/logout", post(sign_in::sign_out))
        .route("/ui/board", get(board::page))
        .route("/ui/threads", get(threads::list))
        .route("/ui/threads/{id}", get(threads::page))
        .route("/ui/threads/{id}/messages", post(threads::reply))
        .route(
            "/ui/threads/{id}/messages/{message}/ack",
            post(threads::ack),
        )
        .route("/ui/items/{id}/takeover", post(takeover::take_over))
        .route("/ui/events/stream", get(live::stream))
        .route("/ui/session", get(live::session))
        .route("/ui/static/{hash}/{name}", get(assets::file))
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
}

/// `GET /ui`: the status page lives at `/ui/`.
async fn to_status() -> Redirect {
    Redirect::permanent("/ui/")
}

/// A page answer of a signed-in browser, carrying the session cookie again so that the browser keeps
/// it as long as the server's sliding expiry does.
fn signed_page(session: &UiSession, response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    if let Some(cookie) = session.cookie() {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}
