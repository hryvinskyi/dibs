//! The client versions the server serves: a `dibs` client older than the oldest one served is
//! answered `426` on every API route but the one that names the versions, while other agents and
//! the pages are not judged.

use axum::http::{Method, StatusCode, header};
use semver::Version;

use dibs_server::http::HttpOptions;
use dibs_server::versions::ClientVersions;

use crate::support::{PEER, Person, World, assert_error, request};

async fn world(min: &str, latest: &str) -> World {
    let versions = ClientVersions::new(
        Some(Version::parse(min).unwrap()),
        Some(Version::parse(latest).unwrap()),
    )
    .unwrap();
    World::with_options(HttpOptions {
        versions,
        ..HttpOptions::default()
    })
    .await
}

async fn as_agent(
    world: &World,
    who: &Person,
    path: &str,
    agent: Option<&str>,
) -> crate::support::Answer {
    let mut request = request(Method::GET, path, Some(&who.token), None);
    if let Some(agent) = agent {
        request
            .headers_mut()
            .insert(header::USER_AGENT, agent.parse().unwrap());
    }
    world.send(request, PEER).await
}

#[tokio::test]
async fn a_client_below_the_oldest_served_is_answered_426_with_how_to_update() {
    let world = world("0.4.0", "0.5.0").await;
    let alice = world.project("acme", "alice").await;
    for path in ["/v1/me", "/v1/status", "/v1/items", "/v1/nothing"] {
        let answer = as_agent(&world, &alice, path, Some("dibs/0.3.9")).await;
        assert_error(&answer, StatusCode::UPGRADE_REQUIRED, "client_too_old");
        let message = answer.body["error"]["message"].as_str().unwrap();
        assert!(message.contains("dibs self-update"), "{path}: {message}");
        assert_eq!(answer.body["error"]["details"]["min_client"], "0.4.0");
        assert_eq!(answer.headers[header::CACHE_CONTROL], "no-store");
    }
}

#[tokio::test]
async fn a_client_too_old_for_the_api_still_learns_the_versions() {
    let world = world("0.4.0", "0.5.0").await;
    let alice = world.project("acme", "alice").await;
    let answer = as_agent(&world, &alice, "/v1/version", Some("dibs/0.3.9")).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.body["min_client"], "0.4.0");
    assert_eq!(answer.body["latest_client"], "0.5.0");
    assert_eq!(answer.body["server"], env!("CARGO_PKG_VERSION"));
    let anonymous = world.call(Method::GET, "/v1/version", None, None).await;
    assert_error(&anonymous, StatusCode::UNAUTHORIZED, "unauthorized");
}

#[tokio::test]
async fn a_current_client_another_agent_and_the_health_probe_are_served() {
    let world = world("0.4.0", "0.5.0").await;
    let alice = world.project("acme", "alice").await;
    for agent in [
        Some("dibs/0.4.0"),
        Some("dibs/0.6.0"),
        Some("curl/8.5.0"),
        None,
    ] {
        let answer = as_agent(&world, &alice, "/v1/me", agent).await;
        assert_eq!(answer.status, StatusCode::OK, "{agent:?}: {}", answer.body);
    }
    let health = world.call(Method::GET, "/health", None, None).await;
    assert_eq!(health.status, StatusCode::OK);
}

#[tokio::test]
async fn without_versions_set_no_client_is_refused_and_none_is_recommended() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let answer = as_agent(&world, &alice, "/v1/me", Some("dibs/0.0.1")).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let versions = world.get(&alice, "/v1/version").await;
    assert_eq!(versions.body["min_client"], serde_json::Value::Null);
    assert_eq!(versions.body["latest_client"], serde_json::Value::Null);
}
