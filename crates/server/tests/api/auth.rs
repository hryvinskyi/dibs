//! Sign-in: one answer for every bad token, and the limit on failed attempts per client address.

use axum::http::{HeaderName, Method, StatusCode, header};

use dibs_app::token::SecretToken;
use dibs_server::client_ip::{Cidr, ClientIpPolicy, ProxyTrust};
use dibs_server::http::HttpOptions;
use dibs_server::rate_limit::LimitPolicy;

use crate::support::{Answer, World, assert_error, request};

const UNKNOWN: &str = "dibs_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

async fn me_with(world: &World, authorization: Option<&str>, peer: &str) -> Answer {
    let mut request = request(Method::GET, "/v1/me", None, None);
    if let Some(value) = authorization {
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, value.parse().unwrap());
    }
    world.send(request, peer).await
}

#[tokio::test]
async fn every_bad_token_gets_the_same_answer() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world
        .member(&alice, "bob", dibs_domain::member::Role::Member)
        .await;
    world
        .app
        .revoke_token
        .execute(
            &alice.actor,
            SecretToken::parse(&bob.token).unwrap().prefix(),
        )
        .await
        .unwrap();
    let revoked = format!("Bearer {}", bob.token);
    let unknown = format!("Bearer {UNKNOWN}");
    let cases = [
        None,
        Some(unknown.as_str()),
        Some("Bearer dibs_short"),
        Some("Basic YWxpY2U6c2VjcmV0"),
        Some(revoked.as_str()),
    ];
    let mut bodies = Vec::new();
    for (n, authorization) in cases.iter().enumerate() {
        let answer = me_with(&world, *authorization, &format!("198.51.100.{n}:1000")).await;
        assert_error(&answer, StatusCode::UNAUTHORIZED, "unauthorized");
        assert_eq!(answer.headers[header::WWW_AUTHENTICATE], "Bearer");
        bodies.push(answer.body);
    }
    assert!(bodies.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(
        bodies[0]["error"]["message"],
        "The token is not valid; sign in again with a current token."
    );
    let good = me_with(
        &world,
        Some(&format!("Bearer {}", alice.token)),
        "198.51.100.9:1",
    )
    .await;
    assert_eq!(good.status, StatusCode::OK);
}

#[tokio::test]
async fn ten_failures_lock_the_address_out_even_for_a_good_token() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bad = format!("Bearer {UNKNOWN}");
    for _ in 0..10 {
        let answer = me_with(&world, Some(&bad), "198.51.100.1:1000").await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    }
    let locked = me_with(&world, Some(&bad), "198.51.100.1:2000").await;
    assert_error(&locked, StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
    assert_eq!(locked.headers[header::RETRY_AFTER], "6");
    let good = format!("Bearer {}", alice.token);
    let still = me_with(&world, Some(&good), "198.51.100.1:3000").await;
    assert_eq!(still.status, StatusCode::TOO_MANY_REQUESTS);
    let elsewhere = me_with(&world, Some(&good), "198.51.100.2:1000").await;
    assert_eq!(elsewhere.status, StatusCode::OK);
    let health = world.call(Method::GET, "/health", None, None).await;
    assert_eq!(health.status, StatusCode::OK);
}

#[tokio::test]
async fn a_good_token_spends_nothing() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let good = format!("Bearer {}", alice.token);
    for _ in 0..30 {
        let answer = me_with(&world, Some(&good), "198.51.100.1:1000").await;
        assert_eq!(answer.status, StatusCode::OK);
    }
}

fn behind_proxy(header: &str) -> HttpOptions {
    HttpOptions {
        client_ip: ClientIpPolicy::new(Some(ProxyTrust::new(
            vec![Cidr::parse("172.18.0.0/16").unwrap()],
            HeaderName::from_bytes(header.as_bytes()).unwrap(),
        ))),
        ..HttpOptions::default()
    }
}

async fn me_forwarded(world: &World, header: &str, client: &str, peer: &str) -> StatusCode {
    let mut request = request(Method::GET, "/v1/me", Some(UNKNOWN), None);
    request.headers_mut().insert(
        HeaderName::from_bytes(header.as_bytes()).unwrap(),
        client.parse().unwrap(),
    );
    world.send(request, peer).await.status
}

#[tokio::test]
async fn behind_a_trusted_proxy_the_limit_keys_on_the_forwarded_client() {
    let world = World::with_options(behind_proxy("x-forwarded-for")).await;
    world.project("acme", "alice").await;
    for _ in 0..10 {
        let status = me_forwarded(
            &world,
            "x-forwarded-for",
            "203.0.113.7, 172.18.0.5",
            "172.18.0.2:1",
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let locked = me_forwarded(&world, "x-forwarded-for", "203.0.113.7", "172.18.0.2:1").await;
    assert_eq!(locked, StatusCode::TOO_MANY_REQUESTS);
    let other = me_forwarded(&world, "x-forwarded-for", "203.0.113.8", "172.18.0.2:1").await;
    assert_eq!(other, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_configured_header_names_the_client() {
    let world = World::with_options(behind_proxy("cf-connecting-ip")).await;
    world.project("acme", "alice").await;
    for _ in 0..10 {
        me_forwarded(&world, "cf-connecting-ip", "203.0.113.7", "172.18.0.2:1").await;
    }
    let locked = me_forwarded(&world, "cf-connecting-ip", "203.0.113.7", "172.18.0.3:1").await;
    assert_eq!(locked, StatusCode::TOO_MANY_REQUESTS);
    let other = me_forwarded(&world, "cf-connecting-ip", "203.0.113.9", "172.18.0.3:1").await;
    assert_eq!(other, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_forged_header_from_an_untrusted_peer_is_ignored() {
    let world = World::with_options(behind_proxy("x-forwarded-for")).await;
    world.project("acme", "alice").await;
    for n in 0..10 {
        let forged = format!("192.0.2.{n}");
        let status = me_forwarded(&world, "x-forwarded-for", &forged, "203.0.113.50:1").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let locked = me_forwarded(&world, "x-forwarded-for", "192.0.2.200", "203.0.113.50:1").await;
    assert_eq!(locked, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn the_limiter_stays_bounded_under_many_addresses() {
    let options = HttpOptions {
        limits: LimitPolicy {
            max_addresses: 50,
            ..LimitPolicy::default()
        },
        ..HttpOptions::default()
    };
    let world = World::with_options(options).await;
    world.project("acme", "alice").await;
    for n in 0..300u32 {
        let peer = format!("10.{}.{}.1:1", n / 250, n % 250);
        let answer = me_with(&world, Some(&format!("Bearer {UNKNOWN}")), &peer).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    }
    for _ in 0..9 {
        me_with(&world, Some(&format!("Bearer {UNKNOWN}")), "10.0.0.1:1").await;
    }
    let first_again = me_with(&world, Some(&format!("Bearer {UNKNOWN}")), "10.0.0.1:1").await;
    assert_eq!(
        first_again.status,
        StatusCode::UNAUTHORIZED,
        "the oldest address was evicted, so its failures start over"
    );
}
