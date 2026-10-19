//! Who is asking: the bearer token of a request turned into the member it authenticates.
//!
//! A missing, malformed, unknown or revoked token is refused with one answer, the same for every
//! cause. Each refusal spends from the client address's bucket of failed sign-ins; while that bucket
//! is empty the request is turned away with `429` before its token is looked at.

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Instant;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, header};

use dibs_app::token::SecretToken;
use dibs_app::{Actor, AppError};

use crate::error::ApiError;
use crate::http::AppState;
use crate::rate_limit::retry_after_secs;

/// The authenticated caller of a route.
#[derive(Debug, Clone)]
pub struct Authed(pub Actor);

impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let client = client_address(parts, state);
        state
            .limiter
            .admit(client, Instant::now())
            .map_err(|wait| ApiError::too_many_attempts(retry_after_secs(wait)))?;
        let outcome = match bearer(&parts.headers) {
            Some(token) => state.app.authenticate.execute(token.expose()).await,
            None => Err(AppError::Unauthorized),
        };
        match outcome {
            Ok(actor) => Ok(Self(actor)),
            Err(AppError::Unauthorized) => {
                state.limiter.record_failure(client, Instant::now());
                Err(ApiError::unauthorized())
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// The address a request comes from, as the client-address policy tells it.
#[derive(Debug, Clone, Copy)]
pub struct ClientAddress(pub IpAddr);

impl FromRequestParts<AppState> for ClientAddress {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Infallible> {
        Ok(Self(client_address(parts, state)))
    }
}

/// The client's address: the connection's peer, or what a trusted proxy says it forwarded.
fn client_address(parts: &Parts, state: &AppState) -> IpAddr {
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)), |info| info.0);
    state.client_ip.client(peer, &parts.headers)
}

/// The token of an `Authorization: Bearer <token>` header in the shape of a secret, or `None`.
pub fn bearer(headers: &HeaderMap) -> Option<SecretToken> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    SecretToken::parse(token.trim()).ok()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    const TOKEN: &str = "dibs_abcdefgh0123456789ABCDEFGHIJKLMNOP_-xyz";

    fn with(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn a_bearer_token_is_read_from_the_header() {
        for value in [
            format!("Bearer {TOKEN}"),
            format!("bearer {TOKEN}"),
            format!("BEARER   {TOKEN} "),
        ] {
            assert_eq!(bearer(&with(&value)).unwrap().expose(), TOKEN, "{value}");
        }
    }

    #[test]
    fn anything_else_is_no_token() {
        for value in [
            TOKEN.to_owned(),
            format!("Basic {TOKEN}"),
            "Bearer".to_owned(),
            "Bearer dibs_short".to_owned(),
            "Bearer not-a-token-at-all-but-long-enough-to-pass".to_owned(),
        ] {
            assert!(bearer(&with(&value)).is_none(), "{value}");
        }
        assert!(bearer(&HeaderMap::new()).is_none());
    }
}
