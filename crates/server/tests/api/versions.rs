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

#[tokio::test]
async fn a_client_older_than_protected_tags_is_served_the_settings_without_them() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    world
        .settings(
            &alice,
            serde_json::json!({
                "protected_refs": [
                    {"ref": "refs/heads/main", "pushers": ["alice"]},
                    {"ref": "refs/tags/v*", "pushers": ["alice"]}
                ]
            }),
        )
        .await;
    let rules = |answer: &crate::support::Answer| -> Vec<String> {
        answer.body["settings"]["protected_refs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| rule["ref"].as_str().unwrap().to_owned())
            .collect()
    };
    let older = as_agent(&world, &alice, "/v1/me", Some("dibs/0.1.0")).await;
    assert_eq!(older.status, StatusCode::OK, "{}", older.body);
    assert_eq!(
        rules(&older),
        ["refs/heads/main"],
        "the branch rule is served, the tag rule it cannot read is not"
    );
    for agent in [
        Some("dibs/0.1.1"),
        Some("dibs/0.2.0 (x86_64-unknown-linux-gnu)"),
        Some("curl/8.0"),
        None,
    ] {
        let answer = as_agent(&world, &alice, "/v1/me", agent).await;
        assert_eq!(
            rules(&answer),
            ["refs/heads/main", "refs/tags/v*"],
            "{agent:?}"
        );
    }
}

#[tokio::test]
async fn a_client_older_than_branch_patterns_is_served_the_settings_and_slots_without_them() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    world
        .settings(
            &alice,
            serde_json::json!({
                "protected_refs": [
                    {"ref": "refs/heads/main", "pushers": ["alice"]},
                    {"ref": "refs/heads/release/*", "pushers": ["alice"]},
                    {"ref": "refs/tags/v*", "pushers": ["alice"]}
                ],
                "slots": [{"name": "release:cut", "max_hold": "1h",
                           "freezes": ["refs/heads/release/*", "refs/heads/main"]}]
            }),
        )
        .await;
    let rules = |answer: &crate::support::Answer| -> Vec<String> {
        answer.body["settings"]["protected_refs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| rule["ref"].as_str().unwrap().to_owned())
            .collect()
    };
    let frozen = |slot: &serde_json::Value| -> Vec<String> {
        slot["freezes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|frozen| frozen.as_str().unwrap().to_owned())
            .collect()
    };
    let whole_freezes = ["default:refs/heads/release/*", "default:refs/heads/main"];
    for (agent, expected_rules, expected_freezes) in [
        (
            Some("dibs/0.1.0"),
            vec!["refs/heads/main"],
            vec!["default:refs/heads/main"],
        ),
        (
            Some("dibs/0.1.1"),
            vec!["refs/heads/main", "refs/tags/v*"],
            vec!["default:refs/heads/main"],
        ),
        (
            Some("dibs/0.1.2"),
            vec!["refs/heads/main", "refs/heads/release/*", "refs/tags/v*"],
            whole_freezes.to_vec(),
        ),
        (
            Some("curl/8.0"),
            vec!["refs/heads/main", "refs/heads/release/*", "refs/tags/v*"],
            whole_freezes.to_vec(),
        ),
        (
            None,
            vec!["refs/heads/main", "refs/heads/release/*", "refs/tags/v*"],
            whole_freezes.to_vec(),
        ),
    ] {
        let me = as_agent(&world, &alice, "/v1/me", agent).await;
        assert_eq!(me.status, StatusCode::OK, "{agent:?}: {}", me.body);
        assert_eq!(rules(&me), expected_rules, "{agent:?}");
        assert_eq!(
            frozen(&me.body["settings"]["slots"][0]),
            expected_freezes,
            "{agent:?}"
        );
        let slots = as_agent(&world, &alice, "/v1/slots", agent).await;
        assert_eq!(slots.status, StatusCode::OK, "{agent:?}: {}", slots.body);
        assert_eq!(
            frozen(&slots.body["slots"][0]),
            expected_freezes,
            "the slots' answers too: {agent:?}"
        );
        let one = as_agent(&world, &alice, "/v1/slots/release:cut", agent).await;
        assert_eq!(frozen(&one.body["slot"]), expected_freezes, "{agent:?}");
        let status = as_agent(&world, &alice, "/v1/status", agent).await;
        assert_eq!(status.status, StatusCode::OK, "{agent:?}: {}", status.body);
        assert_eq!(
            frozen(&status.body["slots"][0]),
            expected_freezes,
            "{agent:?}"
        );
    }
}
