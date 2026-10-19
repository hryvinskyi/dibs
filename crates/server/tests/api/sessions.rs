//! `/v1/me`, `/v1/members`, `/v1/sessions` and `/v1/heartbeat`.

use axum::http::StatusCode;
use serde_json::json;
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{World, assert_error, two_repositories};

#[tokio::test]
async fn me_answers_the_member_project_repositories_and_settings() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let single = world.get(&alice, "/v1/me").await;
    assert_eq!(single.status, StatusCode::OK);
    assert_eq!(
        single.body["member"],
        json!({"handle": "alice", "display_name": "alice", "external_login": null, "role": "admin"})
    );
    assert_eq!(single.body["project"], json!({"slug": "acme"}));
    assert_eq!(
        single.body["repos"],
        json!([{"id": "default", "remote": null}])
    );
    assert_eq!(single.body["settings"]["stale_after"], "1d");
    world.settings(&alice, two_repositories()).await;
    let several = world.get(&alice, "/v1/me").await;
    assert_eq!(
        several.body["repos"],
        json!([
            {"id": "api", "remote": "github.com/acme/api"},
            {"id": "web", "remote": "github.com/acme/web"}
        ])
    );
    assert_eq!(
        several.body["settings"]["repos"][0]["areas"][0]["name"],
        "billing"
    );
}

#[tokio::test]
async fn members_lists_the_project_by_handle() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    world.member(&alice, "carol", Role::Importer).await;
    world.member(&alice, "bob", Role::Member).await;
    let answer = world.get(&alice, "/v1/members").await;
    assert_eq!(answer.status, StatusCode::OK);
    let handles: Vec<&str> = answer.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["handle"].as_str().unwrap())
        .collect();
    assert_eq!(handles, ["alice", "bob", "carol"]);
    assert_eq!(answer.body["members"][2]["role"], "importer");
}

#[tokio::test]
async fn a_session_registers_once_per_label() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let body =
        json!({"host": "laptop", "label": "laptop/4f2a91c0", "agent": "some-agent", "push": true});
    let first = world.post(&alice, "/v1/sessions", body.clone()).await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.body["host"], "laptop");
    assert_eq!(first.body["agent"], "some-agent");
    assert_eq!(first.body["push"], true);
    assert_eq!(first.body["last_seen"], "2026-03-02T09:00:00Z");
    let again = world.post(&alice, "/v1/sessions", body).await;
    assert_eq!(again.body["session_id"], first.body["session_id"]);
    let quiet = world
        .post(
            &alice,
            "/v1/sessions",
            json!({"host": "laptop", "label": "laptop/mcp", "agent": "a"}),
        )
        .await;
    assert_eq!(quiet.body["push"], false);
    assert_ne!(quiet.body["session_id"], first.body["session_id"]);
}

#[tokio::test]
async fn a_session_with_a_bad_field_is_refused_naming_it() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let cases = [
        (
            json!({"label": "x", "agent": "a"}),
            "host",
            "The field 'host' is required.",
        ),
        (
            json!({"host": "my laptop", "label": "x", "agent": "a"}),
            "host",
            "The field 'host' is not valid: a host name is 1 to 255 characters with no spaces or control characters.",
        ),
        (
            json!({"host": "h", "label": "x"}),
            "agent",
            "The field 'agent' is required.",
        ),
        (
            json!({"host": "h", "label": " ", "agent": "a"}),
            "label",
            "",
        ),
        (
            json!({"host": "h", "label": "x", "agent": "Code Agent"}),
            "agent",
            "The field 'agent' is not valid: an agent label is 1 to 32 characters: lowercase letters, digits and '-', starting with a letter or digit.",
        ),
        (
            json!({"host": "h", "label": "x", "agent": "a".repeat(33)}),
            "agent",
            "",
        ),
        (
            json!({"host": "h", "label": "x", "agent": " agent"}),
            "agent",
            "",
        ),
    ];
    let longest = world
        .post(
            &alice,
            "/v1/sessions",
            json!({"host": "h", "label": "x", "agent": "a".repeat(32)}),
        )
        .await;
    assert_eq!(longest.status, StatusCode::OK, "{}", longest.body);
    for (body, field, message) in cases {
        let answer = world.post(&alice, "/v1/sessions", body).await;
        assert_error(&answer, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
        assert_eq!(answer.body["error"]["details"]["field"], field);
        if !message.is_empty() {
            assert_eq!(answer.body["error"]["message"], message);
        }
    }
    let wrong_type = world
        .post(
            &alice,
            "/v1/sessions",
            json!({"host": 7, "label": "x", "agent": "a"}),
        )
        .await;
    assert_error(&wrong_type, StatusCode::BAD_REQUEST, "invalid_json");
}

#[tokio::test]
async fn a_heartbeat_keeps_the_claims_of_its_machine_alive() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let session = world.session(&alice, "laptop").await;
    let item = world.item(&alice, None, "Fix the invoice total").await;
    let claimed = world
        .post(
            &alice,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": session}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK);
    world.fakes.clock.advance(Duration::minutes(5));
    let beat = world
        .post(&alice, "/v1/heartbeat", json!({"session_id": session}))
        .await;
    assert_eq!(beat.status, StatusCode::OK);
    assert_eq!(beat.body, json!({"refreshed": [item]}));
    let shown = world.get(&alice, &format!("/v1/items/{item}")).await;
    assert_eq!(shown.body["claim"]["last_seen"], "2026-03-02T09:05:00Z");
}

#[tokio::test]
async fn a_heartbeat_names_a_session_of_the_caller() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let bobs = world.session(&bob, "desk").await;
    let missing = world.post(&alice, "/v1/heartbeat", json!({})).await;
    assert_error(
        &missing,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    for session in [json!(bobs), json!(999), json!(0)] {
        let answer = world
            .post(&alice, "/v1/heartbeat", json!({"session_id": session}))
            .await;
        assert_error(&answer, StatusCode::NOT_FOUND, "not_found");
    }
}
