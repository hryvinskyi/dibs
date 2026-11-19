//! The browser pages for people: sign-in with a token; the frame every screen is drawn in — a rail
//! with the screens, the people, the slots and the worktrees held — around the five screens (Now,
//! Claims, Queue, Areas, Inbox), each of the last four with an inspector of the object selected, and
//! the search; the board, the threads and a thread with its reply form, in a frame of their own; the
//! event stream over the session cookie; and the static bundle.
//!
//! Pages are rendered on the server from the templates, which hold every word a page shows; the
//! scripts only enhance them. Every answer carries a content security policy that allows nothing but
//! this service's own scripts, styles, fonts and connections, so no inline script, style or handler
//! runs. Every form posts a token of the session it was rendered for, and the session cookie is
//! `SameSite=Strict`. Every text a person or an agent wrote is escaped by the templates.

mod areas;
mod assets;
mod board;
mod chrome;
mod claims;
mod conversation;
mod cookies;
mod inbox;
mod links;
mod live;
mod matching;
mod moments;
mod notice;
mod now;
mod queue;
mod render;
mod rows;
mod search;
mod session;
mod shell;
mod sign_in;
mod slots;
mod takeover;
mod text_query;
mod threads;
mod worktrees;
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
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// The routes of the pages, with the headers every answer of theirs carries.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/ui", get(to_now))
        .route("/ui/", get(now::page))
        .route("/ui/claims", get(claims::list))
        .route("/ui/claims/{item}", get(claims::inspect))
        .route("/ui/queue", get(queue::list))
        .route("/ui/queue/{item}", get(queue::inspect))
        .route("/ui/areas", get(areas::list))
        .route("/ui/areas/{area}", get(areas::inspect))
        .route("/ui/inbox", get(inbox::list))
        .route("/ui/inbox/{thread}", get(inbox::inspect))
        .route("/ui/search", get(search::page))
        .route("/ui/login", get(sign_in::form).post(sign_in::sign_in))
        .route("/ui/logout", post(sign_in::sign_out))
        .route("/ui/board", get(board::page))
        .route("/ui/board/fragment", get(board::fragment))
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

/// `GET /ui`: Now lives at `/ui/`.
async fn to_now() -> Redirect {
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
