//! The browser pages for people: sign-in with a token; the frame every screen is drawn in — a rail
//! with the screens, the people, the slots and the worktrees held — around the five screens (Now,
//! Claims, Queue, Areas, Inbox), each of the last four with an inspector of the object selected, and
//! the search; the forms a page posts; the event stream over the session cookie; and the static
//! bundle.
//!
//! The routes:
//!
//! - `GET /ui/login`, `POST /ui/login`, `POST /ui/logout`: signing in with a token and out again;
//! - `GET /ui/` (Now; `GET /ui` answers `308` to it), `GET /ui/claims`, `GET /ui/claims/{item}`,
//!   `GET /ui/queue`, `GET /ui/queue/{item}`, `GET /ui/areas`, `GET /ui/areas/{repo}:{area}`,
//!   `GET /ui/inbox`, `GET /ui/inbox/{thread}` and `GET /ui/search`: the screens, and the object
//!   each selects in its inspector;
//! - `POST /ui/threads/{thread}/messages` and `POST /ui/threads/{thread}/messages/{message}/ack`:
//!   a reply to a conversation and an acknowledgement of one of its messages;
//! - `POST /ui/items/{item}/takeover`: taking over a silent claim;
//! - `GET /ui/events/stream` and `GET /ui/session`: the event stream and the check that the
//!   session still stands;
//! - `GET /ui/static/{hash}/{name}`: the stylesheet, the scripts and the fonts;
//! - `GET /ui/board`, `GET /ui/threads` and `GET /ui/threads/{thread}`: addresses people kept from
//!   earlier pages, answered `308` to Claims (the member the board was narrowed to, `member`, kept
//!   as `who`), to Inbox, and to that conversation in Inbox.
//!
//! Pages are rendered on the server from the templates, which hold every word a page shows; the
//! scripts only enhance them. Every answer carries a content security policy that allows nothing but
//! this service's own scripts, styles, fonts and connections, so no inline script, style or handler
//! runs. Every form posts a token of the session it was rendered for, and the session cookie is
//! `SameSite=Strict`. Every text a person or an agent wrote is escaped by the templates.

mod areas;
mod assets;
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
mod search;
mod session;
mod shell;
mod sign_in;
mod slots;
mod takeover;
mod text_query;
mod threads;
mod zone;

use axum::Router;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::http::AppState;
use crate::ui::links::query_value;

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
        .route("/ui/board", get(board_to_claims))
        .route("/ui/threads", get(to_inbox))
        .route("/ui/threads/{id}", get(thread_to_inbox))
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

/// The part of an old board address's query that Claims still reads.
#[derive(Debug, Deserialize)]
struct BoardQuery {
    member: Option<String>,
}

/// `GET /ui/board`: Claims shows what the board showed. A board narrowed to one member opens Claims
/// narrowed to that holder; the board's other filters have no counterpart and are dropped, and so
/// is a query that does not read.
async fn board_to_claims(query: Result<Query<BoardQuery>, QueryRejection>) -> Redirect {
    let member = query.ok().and_then(|Query(query)| query.member);
    Redirect::permanent(&claims_address(member.as_deref()))
}

/// The address of Claims, narrowed to `member` when one is named.
fn claims_address(member: Option<&str>) -> String {
    match member.filter(|member| !member.is_empty()) {
        Some(member) => format!("/ui/claims?who={}", query_value(member)),
        None => "/ui/claims".to_owned(),
    }
}

/// `GET /ui/threads`: the conversations are in Inbox.
async fn to_inbox() -> Redirect {
    Redirect::permanent("/ui/inbox")
}

/// `GET /ui/threads/{id}`: the conversation in Inbox. The segment is passed on as received, encoded
/// again, so Inbox decides whether it names a conversation; a segment that does not decode leads to
/// Inbox itself.
async fn thread_to_inbox(path: Result<Path<String>, PathRejection>) -> Redirect {
    match path {
        Ok(Path(segment)) => Redirect::permanent(&inbox_address(&segment)),
        Err(_) => Redirect::permanent("/ui/inbox"),
    }
}

/// The address in Inbox of the conversation a decoded path segment names, with every byte outside
/// the unreserved set encoded: no segment can make it a header no answer may carry, an address of
/// another site, or another path of this service. A dot segment, which a browser resolves to the
/// address above it, names no conversation and leads to Inbox itself.
fn inbox_address(segment: &str) -> String {
    if matches!(segment, "" | "." | "..") {
        return "/ui/inbox".to_owned();
    }
    format!("/ui/inbox/{}", query_value(segment))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_conversation_address_leads_to_the_same_segment_in_inbox_encoded_again() {
        assert_eq!(inbox_address("7"), "/ui/inbox/7");
        assert_eq!(inbox_address("a\r\nb"), "/ui/inbox/a%0D%0Ab");
        assert_eq!(
            inbox_address("//example.com"),
            "/ui/inbox/%2F%2Fexample.com"
        );
        assert_eq!(inbox_address("../claims"), "/ui/inbox/..%2Fclaims");
        for dots in ["", ".", ".."] {
            assert_eq!(inbox_address(dots), "/ui/inbox", "{dots:?}");
        }
    }

    #[test]
    fn an_old_board_address_keeps_only_its_member_as_the_holder_claims_shows() {
        assert_eq!(claims_address(None), "/ui/claims");
        assert_eq!(claims_address(Some("")), "/ui/claims");
        assert_eq!(claims_address(Some("bob")), "/ui/claims?who=bob");
        assert_eq!(
            claims_address(Some("b&o b\r\n")),
            "/ui/claims?who=b%26o%20b%0D%0A"
        );
    }
}
