//! What every route shares: `/health`, `Cache-Control`, unknown routes and methods, the body limit
//! and the request timeout.

use std::time::Duration as StdDuration;

use axum::http::{Method, StatusCode, header};
use serde_json::json;
use time::Duration;

use dibs_app::ports::Store;
use dibs_domain::member::Role;
use dibs_server::http::{BODY_LIMIT, HttpOptions};

use crate::support::{PEER, World, assert_error, request};

#[tokio::test]
async fn health_answers_without_a_token() {
    let world = World::new().await;
    let answer = world.call(Method::GET, "/health", None, None).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body, json!({"ok": true}));
    assert_eq!(answer.headers[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn every_answer_is_marked_no_store() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let answers = [
        world.get(&alice, "/v1/me").await,
        world.get(&alice, "/v1/items/999").await,
        world.call(Method::GET, "/v1/me", None, None).await,
        world.call(Method::GET, "/nowhere", None, None).await,
    ];
    for answer in answers {
        assert_eq!(
            answer.headers[header::CACHE_CONTROL],
            "no-store",
            "{}",
            answer.body
        );
    }
}

#[tokio::test]
async fn unknown_routes_and_methods_answer_in_the_error_shape() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let route = world.get(&alice, "/v1/nothing").await;
    assert_error(&route, StatusCode::NOT_FOUND, "not_found");
    let method = world
        .call(Method::DELETE, "/v1/items", Some(&alice.token), None)
        .await;
    assert_error(
        &method,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    );
}

/// alice's stale claim on an item bob may take over, with the reason bob sends.
async fn takeover_with_reason(world: &World, body: String) -> crate::support::Answer {
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let alice_session = world.session(&alice, "laptop").await;
    let bob_session = world.session(&bob, "desk").await;
    let item = world.item(&alice, None, "Long story").await;
    world
        .post(
            &alice,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": alice_session}),
        )
        .await;
    world.fakes.clock.advance(Duration::days(2));
    let text = format!(r#"{{"session_id": {bob_session}, "reason": {body}}}"#);
    let request = request(
        Method::POST,
        &format!("/v1/items/{item}/takeover"),
        Some(&bob.token),
        Some(text),
    );
    world.send(request, PEER).await
}

#[tokio::test]
async fn a_16_kib_message_of_line_breaks_fits() {
    let world = World::new().await;
    let reason = "a\n".repeat(8192);
    let answer = takeover_with_reason(&world, serde_json::to_string(&reason).unwrap()).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
}

#[tokio::test]
async fn a_16_kib_message_of_escaped_cyrillic_fits() {
    let world = World::new().await;
    let escaped: String = "ж"
        .repeat(8192)
        .chars()
        .map(|c| format!("\\u{:04x}", u32::from(c)))
        .collect();
    let body = format!("\"{escaped}\"");
    assert!(body.len() > 48 * 1024);
    let answer = takeover_with_reason(&world, body).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let title = "x".repeat(BODY_LIMIT);
    let answer = world
        .post(&alice, "/v1/items", json!({"title": title}))
        .await;
    assert_error(&answer, StatusCode::PAYLOAD_TOO_LARGE, "body_too_large");
}

#[tokio::test]
async fn a_request_that_takes_too_long_answers_timeout() {
    let options = HttpOptions {
        timeout: StdDuration::from_millis(200),
        ..HttpOptions::default()
    };
    let world = World::with_options(options).await;
    let alice = world.project("acme", "alice").await;
    let held = world.store.begin().await.unwrap();
    let answer = world.get(&alice, "/v1/members").await;
    assert_error(&answer, StatusCode::SERVICE_UNAVAILABLE, "timeout");
    drop(held);
    let after = world.get(&alice, "/v1/members").await;
    assert_eq!(after.status, StatusCode::OK);
}
