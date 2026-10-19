//! `GET /ui/login`, `POST /ui/login` and `POST /ui/logout`: signing a browser in with a token and
//! out again.
//!
//! The sign-in form is protected before any session exists: rendering it sets a one-hour cookie
//! holding a fresh secret and prints the same secret in the form, and a sign-in is read only when
//! both agree. The cookie is `SameSite=Strict`, so a form another site posts arrives without it.
//! A token that is refused spends from the address's allowance of failed sign-ins, as a bad bearer
//! token does.

use std::time::Instant;

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::extract::rejection::FormRejection;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use dibs_app::AppError;
use dibs_app::use_cases::UI_SESSION_LIFETIME;

use crate::auth::ClientAddress;
use crate::error::ApiError;
use crate::http::AppState;
use crate::rate_limit::retry_after_secs;
use crate::ui::assets::BUNDLE_HASH;
use crate::ui::cookies;
use crate::ui::notice::{Problem, notice};
use crate::ui::render::{html, see_other};
use crate::ui::session::{Resolved, SESSION_COOKIE, resolve, same_text};

/// The cookie that ties a sign-in to the form rendered for it.
pub const LOGIN_COOKIE: &str = "dibs_login";

/// How long a rendered sign-in form can be submitted, in seconds.
const LOGIN_FORM_SECS: i64 = 3600;

/// Why the sign-in form is shown again. The wording of each is the template's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInProblem {
    /// The token is not one the service knows, or no longer works.
    BadToken,
    /// The token is an integration's, which has no browser session.
    Integration,
    /// The form was not rendered for this browser, or too long ago.
    FormExpired,
    /// Too many refused tokens from this address; the number is the minutes to wait.
    TooManyAttempts(u64),
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    assets: &'static str,
    csrf: String,
    problem: Option<SignInProblem>,
}

/// The sign-in form, with a fresh form cookie.
fn login_page(state: &AppState, status: StatusCode, problem: Option<SignInProblem>) -> Response {
    let nonce = state.tokens.generate();
    let page = LoginPage {
        assets: BUNDLE_HASH,
        csrf: nonce.expose().to_owned(),
        problem,
    };
    let mut response = html(status, &page);
    match cookies::set(LOGIN_COOKIE, nonce.expose(), LOGIN_FORM_SECS) {
        Ok(cookie) => {
            response.headers_mut().append(header::SET_COOKIE, cookie);
            response
        }
        Err(_) => ApiError::internal().into_response(),
    }
}

/// `GET /ui/login`: the sign-in form.
pub async fn form(State(state): State<AppState>) -> Response {
    login_page(&state, StatusCode::OK, None)
}

/// The fields of the sign-in form.
#[derive(Deserialize)]
pub struct SignInForm {
    csrf: Option<String>,
    token: Option<String>,
}

/// `POST /ui/login`: opens a browser session for the pasted token and goes to the status page; a
/// refused token, an expired form or too many attempts show the form again with one sentence.
pub async fn sign_in(
    State(state): State<AppState>,
    ClientAddress(client): ClientAddress,
    headers: HeaderMap,
    form: Result<Form<SignInForm>, FormRejection>,
) -> Response {
    let Ok(Form(form)) = form else {
        return login_page(
            &state,
            StatusCode::BAD_REQUEST,
            Some(SignInProblem::FormExpired),
        );
    };
    let issued = cookies::read(&headers, LOGIN_COOKIE);
    let fresh = matches!(
        (issued, form.csrf.as_deref()),
        (Some(issued), Some(sent)) if same_text(issued, sent)
    );
    if !fresh {
        return login_page(
            &state,
            StatusCode::FORBIDDEN,
            Some(SignInProblem::FormExpired),
        );
    }
    if let Err(wait) = state.limiter.admit(client, Instant::now()) {
        let minutes = retry_after_secs(wait).div_ceil(60).max(1);
        return login_page(
            &state,
            StatusCode::TOO_MANY_REQUESTS,
            Some(SignInProblem::TooManyAttempts(minutes)),
        );
    }
    let token = form.token.unwrap_or_default();
    match state.app.open_ui_session.execute(token.trim()).await {
        Ok(opened) => {
            let session = cookies::set(
                SESSION_COOKIE,
                opened.secret.expose(),
                UI_SESSION_LIFETIME.whole_seconds(),
            );
            let (Ok(session), Ok(used)) = (session, cookies::clear(LOGIN_COOKIE)) else {
                return ApiError::internal().into_response();
            };
            let mut response = see_other("/ui/");
            response.headers_mut().append(header::SET_COOKIE, session);
            response.headers_mut().append(header::SET_COOKIE, used);
            response
        }
        Err(AppError::Unauthorized) => {
            state.limiter.record_failure(client, Instant::now());
            login_page(&state, StatusCode::OK, Some(SignInProblem::BadToken))
        }
        Err(AppError::Forbidden) => {
            login_page(&state, StatusCode::OK, Some(SignInProblem::Integration))
        }
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// The fields of a form that carries only its token.
#[derive(Deserialize)]
pub struct TokenForm {
    csrf: Option<String>,
}

/// `POST /ui/logout`: ends the browser's session and goes to the sign-in form. A browser whose
/// session already ended is only sent there.
pub async fn sign_out(
    State(state): State<AppState>,
    headers: HeaderMap,
    form: Result<Form<TokenForm>, FormRejection>,
) -> Response {
    let session = match resolve(&headers, &state).await {
        Resolved::Signed(session) => session,
        Resolved::Missing | Resolved::Ended => return signed_out(),
        Resolved::Failed(error) => return error.into_response(),
    };
    let sent = form.ok().and_then(|Form(form)| form.csrf);
    if !session.accepts(sent.as_deref()) {
        return notice(
            &state,
            &session,
            StatusCode::FORBIDDEN,
            Problem::FormExpired,
            "/ui/",
        )
        .await;
    }
    match state.app.close_ui_session.execute(session.secret()).await {
        Ok(()) => signed_out(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// The way to the sign-in form, removing the session cookie.
fn signed_out() -> Response {
    let mut response = see_other("/ui/login");
    if let Ok(clear) = cookies::clear(SESSION_COOKIE) {
        response.headers_mut().append(header::SET_COOKIE, clear);
    }
    response
}
