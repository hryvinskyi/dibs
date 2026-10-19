//! The browser's session: the `dibs_ui` cookie resolved into the member it signed in, and the form
//! token that proves a form was rendered for that session.
//!
//! Only the cookie counts: a bearer token on a page request signs nobody in, and a cookie whose
//! value no browser session has — another site's, an API token pasted in its place, one that
//! expired or whose token was revoked — resolves to nobody. A page answers a request without a
//! session with the sign-in page, and one whose session ended with the session-ended page; the event
//! stream and the session check answer both with `401`.
//!
//! The form token is derived from the session's secret with SHA-256 under a label of its own, so it
//! is fixed for the session's lifetime, cannot be computed without the cookie no script can read,
//! and shows nothing of the cookie when a page is seen.

use std::fmt::Write as _;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use dibs_app::use_cases::UI_SESSION_LIFETIME;
use dibs_app::{Actor, AppError};

use crate::error::ApiError;
use crate::http::AppState;
use crate::ui::cookies;
use askama::Template;

use crate::ui::assets::BUNDLE_HASH;
use crate::ui::render::{html, see_other};

/// The cookie that carries the browser session's secret.
pub const SESSION_COOKIE: &str = "dibs_ui";

/// The label the form token is derived under.
const FORM_TOKEN_LABEL: &[u8] = b"dibs ui form token\0";

/// A signed-in browser: the member and the secret of its session.
#[derive(Debug, Clone)]
pub struct UiSession {
    actor: Actor,
    secret: String,
}

impl UiSession {
    /// The member signed in.
    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// The session's secret, as the cookie carries it.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// The form token every form of this session carries.
    pub fn form_token(&self) -> String {
        form_token(&self.secret)
    }

    /// Whether a submitted form token is this session's, compared in constant time.
    pub fn accepts(&self, submitted: Option<&str>) -> bool {
        submitted.is_some_and(|text| same_text(text, &self.form_token()))
    }

    /// The `Set-Cookie` value that keeps the session for its whole lifetime from now, matching the
    /// server's sliding expiry.
    pub fn cookie(&self) -> Option<HeaderValue> {
        cookies::set(
            SESSION_COOKIE,
            &self.secret,
            UI_SESSION_LIFETIME.whole_seconds(),
        )
        .ok()
    }
}

/// What the cookie of a request resolves to.
#[derive(Debug)]
pub enum Resolved {
    /// A live session.
    Signed(UiSession),
    /// No session cookie at all.
    Missing,
    /// A cookie no live session has.
    Ended,
    /// The store failed.
    Failed(ApiError),
}

/// Resolves the session cookie of a request, moving the session's end forward when it is live.
pub async fn resolve(headers: &HeaderMap, state: &AppState) -> Resolved {
    let Some(secret) = cookies::read(headers, SESSION_COOKIE) else {
        return Resolved::Missing;
    };
    match state.app.resolve_ui_session.execute(secret).await {
        Ok(actor) => Resolved::Signed(UiSession {
            actor,
            secret: secret.to_owned(),
        }),
        Err(AppError::Unauthorized) => Resolved::Ended,
        Err(error) => Resolved::Failed(error.into()),
    }
}

/// The signed-in browser of a page request. Without a session the request is answered with the
/// sign-in page; with one that ended, with the session-ended page and the cookie removed.
#[derive(Debug, Clone)]
pub struct PageSession(pub UiSession);

impl FromRequestParts<AppState> for PageSession {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Response> {
        match resolve(&parts.headers, state).await {
            Resolved::Signed(session) => Ok(Self(session)),
            Resolved::Missing => Err(see_other("/ui/login")),
            Resolved::Ended => Err(session_ended()),
            Resolved::Failed(error) => Err(error.into_response()),
        }
    }
}

/// The signed-in browser of a request a script makes. Without a live session the answer is `401`.
#[derive(Debug, Clone)]
pub struct ScriptSession(pub UiSession);

impl FromRequestParts<AppState> for ScriptSession {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match resolve(&parts.headers, state).await {
            Resolved::Signed(session) => Ok(Self(session)),
            Resolved::Missing | Resolved::Ended => Err(ApiError::unauthorized()),
            Resolved::Failed(error) => Err(error),
        }
    }
}

/// The page a request with an ended session is answered with.
#[derive(Template)]
#[template(path = "session_ended.html")]
struct SessionEndedPage {
    assets: &'static str,
}

/// The session-ended page, `401`, removing the cookie.
pub fn session_ended() -> Response {
    let page = SessionEndedPage {
        assets: BUNDLE_HASH,
    };
    let mut response = html(StatusCode::UNAUTHORIZED, &page);
    if let Ok(clear) = cookies::clear(SESSION_COOKIE) {
        response.headers_mut().append(header::SET_COOKIE, clear);
    }
    response
}

/// The form token of the session with this secret: SHA-256 over a label and the secret, in hex.
pub fn form_token(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FORM_TOKEN_LABEL);
    hasher.update(secret.as_bytes());
    let mut text = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Whether two texts are equal, taking the same time for every pair of the same length.
pub fn same_text(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_token_is_fixed_per_secret_and_shows_nothing_of_it() {
        let secret = "dibs_abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
        let token = form_token(secret);
        assert_eq!(token, form_token(secret));
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!token.contains("abcdefgh"));
        assert_ne!(token, form_token("dibs_another"));
    }

    #[test]
    fn texts_compare_equal_only_when_they_are() {
        assert!(same_text("abc", "abc"));
        assert!(same_text("", ""));
        assert!(!same_text("abc", "abd"));
        assert!(!same_text("abc", "ab"));
        assert!(!same_text("abc", ""));
    }
}
