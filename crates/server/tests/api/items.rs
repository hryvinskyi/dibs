//! `/v1/items`: creating, finding and showing work items.

use axum::http::{Method, StatusCode};
use serde_json::json;

use dibs_domain::member::Role;

use crate::support::{World, assert_error, two_repositories};

#[tokio::test]
async fn creating_is_idempotent_on_the_reference() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let body = json!({"ref": "gh:acme/api#42", "title": "Invoice totals are off"});
    let first = world.post(&alice, "/v1/items", body.clone()).await;
    assert_eq!(first.status, StatusCode::CREATED);
    assert_eq!(first.body["created"], true);
    let item = &first.body["item"];
    assert_eq!(item["ref"], "gh:acme/api#42");
    assert_eq!(item["title"], "Invoice totals are off");
    assert_eq!(item["status"], "open");
    assert_eq!(item["created_by"], "alice");
    assert_eq!(item["created_at"], "2026-03-02T09:00:00Z");
    let again = world.post(&alice, "/v1/items", body).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["created"], false);
    assert_eq!(again.body["item"]["id"], item["id"]);
    let local = world
        .post(&alice, "/v1/items", json!({"title": "Tidy up"}))
        .await;
    assert_eq!(local.status, StatusCode::CREATED);
    assert_eq!(local.body["item"]["ref"], json!(null));
}

#[tokio::test]
async fn a_reference_with_slash_and_hash_round_trips_through_the_query() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let id = world
        .item(&alice, Some("gh:acme/api#42"), "Invoice totals")
        .await;
    world.item(&alice, Some("gh:acme/api#43"), "Other").await;
    let found = world
        .get(&alice, "/v1/items?ref=gh%3Aacme%2Fapi%2342")
        .await;
    assert_eq!(found.status, StatusCode::OK);
    let items = found.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], id);
    assert_eq!(items[0]["ref"], "gh:acme/api#42");
    let none = world
        .get(&alice, "/v1/items?ref=gh%3Aacme%2Fapi%2399")
        .await;
    assert_eq!(none.body, json!({"items": []}));
}

#[tokio::test]
async fn items_filter_by_status_and_holder() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let session = world.session(&bob, "desk").await;
    let a = world.item(&alice, None, "First").await;
    let b = world.item(&alice, None, "Second").await;
    world
        .post(
            &bob,
            &format!("/v1/items/{b}/claim"),
            json!({"session_id": session}),
        )
        .await;
    let ids = |answer: &crate::support::Answer| -> Vec<u64> {
        answer.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_u64().unwrap())
            .collect()
    };
    assert_eq!(ids(&world.get(&alice, "/v1/items").await), [a, b]);
    assert_eq!(ids(&world.get(&alice, "/v1/items?status=open").await), [a]);
    assert_eq!(
        ids(&world.get(&alice, "/v1/items?status=claimed").await),
        [b]
    );
    assert_eq!(ids(&world.get(&alice, "/v1/items?holder=bob").await), [b]);
    assert!(ids(&world.get(&alice, "/v1/items?holder=alice").await).is_empty());
    assert!(ids(&world.get(&alice, "/v1/items?holder=nobody").await).is_empty());
    let bad_status = world.get(&alice, "/v1/items?status=Open").await;
    assert_error(
        &bad_status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    assert_eq!(bad_status.body["error"]["details"]["field"], "status");
    let bad_holder = world.get(&alice, "/v1/items?holder=Bob%20B").await;
    assert_error(
        &bad_holder,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    let bad_ref = world.get(&alice, "/v1/items?ref=nocolon").await;
    assert_error(
        &bad_ref,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
}

#[tokio::test]
async fn an_item_is_shown_with_its_claim() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let session = world.session(&alice, "laptop").await;
    let item = world.item(&alice, None, "Fix it").await;
    let open = world.get(&alice, &format!("/v1/items/{item}")).await;
    assert_eq!(open.status, StatusCode::OK);
    assert_eq!(open.body["claim"], json!(null));
    world
        .post(
            &alice,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": session}),
        )
        .await;
    let held = world.get(&alice, &format!("/v1/items/{item}")).await;
    assert_eq!(held.body["item"]["status"], "claimed");
    assert_eq!(held.body["claim"]["holder"], "alice");
    assert_eq!(held.body["claim"]["session_id"], session);
}

#[tokio::test]
async fn an_id_that_names_no_item_is_not_found() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    for path in [
        "/v1/items/12345",
        "/v1/items/0",
        "/v1/items/9223372036854775808",
        "/v1/items/abc",
        "/v1/items/gh%3Aacme%2Fapi%2342",
    ] {
        let answer = world.get(&alice, path).await;
        assert_error(&answer, StatusCode::NOT_FOUND, "not_found");
    }
}

#[tokio::test]
async fn a_bad_create_is_refused() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let missing = world
        .post(&alice, "/v1/items", json!({"ref": "gh:acme/api#1"}))
        .await;
    assert_error(
        &missing,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    assert_eq!(
        missing.body["error"]["message"],
        "The field 'title' is required."
    );
    let bad_ref = world
        .post(
            &alice,
            "/v1/items",
            json!({"ref": "no scheme", "title": "x"}),
        )
        .await;
    assert_error(
        &bad_ref,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    assert_eq!(bad_ref.body["error"]["details"]["field"], "ref");
    let not_json = world
        .send(
            crate::support::request(
                Method::POST,
                "/v1/items",
                Some(&alice.token),
                Some("{not json".to_owned()),
            ),
            crate::support::PEER,
        )
        .await;
    assert_error(&not_json, StatusCode::BAD_REQUEST, "invalid_json");
    let mut plain = crate::support::request(Method::POST, "/v1/items", Some(&alice.token), None);
    *plain.body_mut() = axum::body::Body::from(r#"{"title":"x"}"#);
    let unsupported = world.send(plain, crate::support::PEER).await;
    assert_error(
        &unsupported,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    );
}

#[tokio::test]
async fn a_person_cannot_create_an_item_in_an_unlisted_repository() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let carol = world.member(&alice, "carol", Role::Importer).await;
    world.settings(&alice, two_repositories()).await;
    let listed = world
        .post(
            &alice,
            "/v1/items",
            json!({"ref": "gh:acme/web#3", "title": "Checkout"}),
        )
        .await;
    assert_eq!(listed.status, StatusCode::CREATED);
    assert_eq!(listed.body["item"]["repo"], "web");
    let refused = world
        .post(
            &alice,
            "/v1/items",
            json!({"ref": "gh:acme/serach#3", "title": "Typo"}),
        )
        .await;
    assert_error(
        &refused,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unlisted_repository",
    );
    assert_eq!(
        refused.body["error"]["details"]["remote"],
        "github.com/acme/serach"
    );
    let imported = world
        .post(
            &carol,
            "/v1/items",
            json!({"ref": "gh:acme/search#3", "title": "Blocker"}),
        )
        .await;
    assert_eq!(imported.status, StatusCode::CREATED);
    assert_eq!(imported.body["item"]["repo"], json!(null));
    assert_eq!(
        imported.body["item"]["unlisted_remote"],
        "github.com/acme/search"
    );
}
