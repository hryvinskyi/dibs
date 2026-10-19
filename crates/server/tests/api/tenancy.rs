//! Two projects in one database: a token of one never sees the other's records, and an id of the
//! other answers `404`, never `403`.

use axum::http::{Method, StatusCode};
use serde_json::json;

use crate::support::{World, assert_error};

#[tokio::test]
async fn another_projects_records_are_not_found() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let dave = world.project("globex", "dave").await;
    let alice_session = world.session(&alice, "laptop").await;
    let dave_session = world.session(&dave, "server").await;
    let item = world.item(&alice, Some("gh:acme/api#1"), "Ours").await;
    world
        .post(
            &alice,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": alice_session}),
        )
        .await;
    let theirs = world.item(&dave, None, "Theirs").await;
    world
        .post(
            &dave,
            &format!("/v1/items/{theirs}/claim"),
            json!({"session_id": dave_session}),
        )
        .await;

    let get = world.get(&dave, &format!("/v1/items/{item}")).await;
    assert_error(&get, StatusCode::NOT_FOUND, "not_found");
    for action in ["claim", "takeover", "release", "done"] {
        let answer = world
            .post(
                &dave,
                &format!("/v1/items/{item}/{action}"),
                json!({"session_id": dave_session, "reason": "mine now"}),
            )
            .await;
        assert_error(&answer, StatusCode::NOT_FOUND, "not_found");
    }
    let session = world
        .post(&dave, "/v1/heartbeat", json!({"session_id": alice_session}))
        .await;
    assert_error(&session, StatusCode::NOT_FOUND, "not_found");
    let check = world
        .post(
            &dave,
            "/v1/edit-check",
            json!({"session_id": alice_session, "repo": "default", "paths": ["a"]}),
        )
        .await;
    assert_error(&check, StatusCode::NOT_FOUND, "not_found");
    let grant = world
        .post(
            &dave,
            "/v1/grants",
            json!({"item": item, "area": "x", "to": "alice"}),
        )
        .await;
    assert_error(&grant, StatusCode::NOT_FOUND, "not_found");

    let by_ref = world.get(&dave, "/v1/items?ref=gh%3Aacme%2Fapi%231").await;
    assert_eq!(by_ref.body, json!({"items": []}));
    let listed = world.get(&dave, "/v1/items").await;
    let ids: Vec<u64> = listed.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [theirs]);
    let members = world.get(&dave, "/v1/members").await;
    assert_eq!(members.body["members"].as_array().unwrap().len(), 1);
    let status = world.get(&dave, "/v1/status").await;
    assert_eq!(status.body["live"].as_array().unwrap().len(), 1);
    assert_eq!(status.body["live"][0]["claim"]["holder"], "dave");
    let me = world
        .call(Method::GET, "/v1/me", Some(&dave.token), None)
        .await;
    assert_eq!(me.body["project"]["slug"], "globex");
}

#[tokio::test]
async fn another_projects_grant_is_not_found() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world
        .member(&alice, "bob", dibs_domain::member::Role::Member)
        .await;
    let dave = world.project("globex", "dave").await;
    world
        .settings(
            &alice,
            json!({"areas": [{"name": "billing", "paths": ["billing/**"]}]}),
        )
        .await;
    let alice_session = world.session(&alice, "laptop").await;
    let bob_session = world.session(&bob, "desk").await;
    let a = world.item(&alice, None, "A").await;
    let b = world.item(&bob, None, "B").await;
    world
        .post(
            &alice,
            &format!("/v1/items/{a}/claim"),
            json!({"session_id": alice_session, "areas": ["billing"]}),
        )
        .await;
    world
        .post(
            &bob,
            &format!("/v1/items/{b}/claim"),
            json!({"session_id": bob_session}),
        )
        .await;
    let grant = world
        .post(
            &alice,
            "/v1/grants",
            json!({"item": b, "area": "billing", "to": "bob"}),
        )
        .await;
    assert_eq!(grant.status, StatusCode::CREATED, "{}", grant.body);
    let id = grant.body["grant"]["id"].as_u64().unwrap();
    let answer = world
        .call(
            Method::DELETE,
            &format!("/v1/grants/{id}"),
            Some(&dave.token),
            None,
        )
        .await;
    assert_error(&answer, StatusCode::NOT_FOUND, "not_found");
}
