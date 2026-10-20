//! The admin routes: adding members, rotating and revoking tokens, applying settings, forcing a
//! release, and the refusal every other role gets from each of them whatever it sends.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error, request, two_repositories};

#[tokio::test]
async fn an_admin_adds_a_member_whose_token_works() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let added = world
        .post(
            &alice,
            "/v1/admin/members",
            json!({"handle": "bob", "display_name": "Bob", "external_login": "bob-gh"}),
        )
        .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    assert_eq!(
        added.body["member"],
        json!({"handle": "bob", "display_name": "Bob", "external_login": "bob-gh", "role": "member"})
    );
    let token = added.body["token"].as_str().unwrap().to_owned();
    assert!(token.starts_with("dibs_"));
    let me = world.call(Method::GET, "/v1/me", Some(&token), None).await;
    assert_eq!(me.body["member"]["handle"], "bob");

    let bot = world
        .post(
            &alice,
            "/v1/admin/members",
            json!({"handle": "board", "display_name": "Board", "role": "importer"}),
        )
        .await;
    assert_eq!(bot.body["member"]["role"], "importer");
    let second_admin = world
        .post(
            &alice,
            "/v1/admin/members",
            json!({"handle": "erin", "display_name": "Erin", "role": "admin"}),
        )
        .await;
    assert_eq!(second_admin.body["member"]["role"], "admin");
}

#[tokio::test]
async fn adding_a_member_is_refused_with_the_field_at_fault() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let taken = world
        .post(
            &alice,
            "/v1/admin/members",
            json!({"handle": "alice", "display_name": "Another"}),
        )
        .await;
    assert_error(&taken, StatusCode::CONFLICT, "handle_taken");
    for (body, field) in [
        (json!({"display_name": "Bob"}), "handle"),
        (json!({"handle": "Bob!", "display_name": "Bob"}), "handle"),
        (json!({"handle": "bob"}), "display_name"),
        (
            json!({"handle": "bob", "display_name": "Bob", "role": "owner"}),
            "role",
        ),
        (
            json!({"handle": "bob", "display_name": "Bob", "external_login": ""}),
            "external_login",
        ),
    ] {
        let answer = world.post(&alice, "/v1/admin/members", body).await;
        assert_error(&answer, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
        assert_eq!(answer.body["error"]["details"]["field"], field);
    }
}

#[tokio::test]
async fn a_rotated_token_replaces_every_old_one() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let rotated = world
        .post(&alice, "/v1/admin/members/bob/tokens", json!({}))
        .await;
    assert_eq!(rotated.status, StatusCode::CREATED, "{}", rotated.body);
    assert_eq!(rotated.body["member"]["handle"], "bob");
    let new = rotated.body["token"].as_str().unwrap();
    assert_ne!(new, bob.token);
    let old = world.get(&bob, "/v1/me").await;
    assert_error(&old, StatusCode::UNAUTHORIZED, "unauthorized");
    let fresh = world.call(Method::GET, "/v1/me", Some(new), None).await;
    assert_eq!(fresh.body["member"]["handle"], "bob");
    for handle in ["nobody", "Not%20A%20Handle"] {
        let missing = world
            .post(
                &alice,
                &format!("/v1/admin/members/{handle}/tokens"),
                json!({}),
            )
            .await;
        assert_error(&missing, StatusCode::NOT_FOUND, "not_found");
    }
}

#[tokio::test]
async fn a_token_is_revoked_by_its_prefix_once() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let prefix = &bob.token["dibs_".len().."dibs_".len() + 8];
    let revoked = world
        .delete(&alice, &format!("/v1/admin/tokens/{prefix}"))
        .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(revoked.body["token"]["prefix"], prefix);
    assert_eq!(revoked.body["token"]["member"], "bob");
    assert_eq!(revoked.body["token"]["created_at"], "2026-03-02T09:00:00Z");
    assert_eq!(revoked.body["token"]["revoked_at"], "2026-03-02T09:00:00Z");
    assert!(!revoked.body.to_string().contains(&bob.token));
    assert_error(
        &world.get(&bob, "/v1/me").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    let again = world
        .delete(&alice, &format!("/v1/admin/tokens/dibs_{prefix}"))
        .await;
    assert_eq!(again.body["token"]["revoked_at"], "2026-03-02T09:00:00Z");
    for text in ["zzzzzzzz", "short", "with!bang"] {
        let missing = world
            .delete(&alice, &format!("/v1/admin/tokens/{text}"))
            .await;
        assert_error(&missing, StatusCode::NOT_FOUND, "not_found");
    }
}

#[tokio::test]
async fn settings_are_applied_once_and_answered_in_their_normal_form() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let applied = world
        .put(&alice, "/v1/admin/settings", two_repositories())
        .await;
    assert_eq!(applied.status, StatusCode::OK, "{}", applied.body);
    assert_eq!(applied.body["changed"], true);
    let me = world.get(&alice, "/v1/me").await;
    assert_eq!(applied.body["settings"], me.body["settings"]);
    let again = world
        .put(
            &alice,
            "/v1/admin/settings",
            applied.body["settings"].clone(),
        )
        .await;
    assert_eq!(again.body["changed"], false);
    let events = world.get(&alice, "/v1/events").await;
    let changes = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "settings_changed")
        .count();
    assert_eq!(changes, 1);

    let invalid = world
        .put(
            &alice,
            "/v1/admin/settings",
            json!({"areas": [{"name": "Billing!", "paths": ["x/**"]}]}),
        )
        .await;
    assert_error(
        &invalid,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_settings",
    );
    let not_a_document = world
        .put(&alice, "/v1/admin/settings", json!("not settings"))
        .await;
    assert_error(&not_a_document, StatusCode::BAD_REQUEST, "invalid_json");
}

#[tokio::test]
async fn an_admin_forces_a_release() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let session = world.session(&bob, "desktop").await;
    let item = world.item(&alice, None, "Rounding").await;
    let claimed = world
        .post(
            &bob,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": session}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    let released = world
        .post(
            &alice,
            &format!("/v1/admin/items/{item}/force-release"),
            json!({}),
        )
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.body);
    assert_eq!(released.body["claim"]["holder"], "bob");
    assert_eq!(released.body["claim"]["item"], item);
    let shown = world.get(&alice, &format!("/v1/items/{item}")).await;
    assert_eq!(shown.body["item"]["status"], "open");
    assert_eq!(shown.body["claim"], Value::Null);
    let nothing = world
        .post(
            &alice,
            &format!("/v1/admin/items/{item}/force-release"),
            json!({}),
        )
        .await;
    assert_error(&nothing, StatusCode::CONFLICT, "no_claim");
    for id in ["999", "abc"] {
        let missing = world
            .post(
                &alice,
                &format!("/v1/admin/items/{id}/force-release"),
                json!({}),
            )
            .await;
        assert_error(&missing, StatusCode::NOT_FOUND, "not_found");
    }
}

/// Every admin route with a body that would be refused for its content, so a role refusal can only
/// come before the body is read.
fn admin_routes() -> Vec<(Method, &'static str, Option<Value>)> {
    vec![
        (Method::POST, "/v1/admin/members", Some(json!({}))),
        (Method::POST, "/v1/admin/members/alice/tokens", None),
        (Method::POST, "/v1/admin/members/nobody/tokens", None),
        (Method::DELETE, "/v1/admin/tokens/zzzzzzzz", None),
        (Method::DELETE, "/v1/admin/tokens/bad", None),
        (Method::PUT, "/v1/admin/settings", Some(json!({"areas": 7}))),
        (Method::POST, "/v1/admin/items/999/force-release", None),
        (Method::POST, "/v1/admin/items/abc/force-release", None),
        (Method::DELETE, "/v1/admin/members/alice", None),
        (Method::DELETE, "/v1/admin/members/nobody", None),
        (Method::DELETE, "/v1/admin/threads/999", None),
        (Method::DELETE, "/v1/admin/threads/abc", None),
    ]
}

#[tokio::test]
async fn only_an_admin_reaches_an_admin_route() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let board = world.member(&alice, "board", Role::Importer).await;
    let callers: [&Person; 2] = [&bob, &board];
    for caller in callers {
        for (method, path, body) in admin_routes() {
            let answer = world
                .call(method.clone(), path, Some(&caller.token), body)
                .await;
            assert_error(&answer, StatusCode::FORBIDDEN, "forbidden");
        }
    }
    for (n, (method, path, body)) in admin_routes().into_iter().enumerate() {
        let peer = format!("198.51.100.{}:50000", n + 1);
        let answer = world
            .send(
                request(method, path, None, body.map(|b| b.to_string())),
                &peer,
            )
            .await;
        assert_error(&answer, StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let members = world.get(&alice, "/v1/members").await;
    assert_eq!(members.body["members"].as_array().unwrap().len(), 3);
    assert_eq!(world.get(&bob, "/v1/me").await.status, StatusCode::OK);
}
