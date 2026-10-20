//! What an integration may call: who it is, the members, the event feed, reading, creating and
//! importing work items, the ready list, reading the slots and reporting a finished workflow run;
//! never a conversation, a message, an acknowledgement or a slot hold. Every other route of the router answers it
//! `forbidden`, and each refusal comes before the request is read, so a body that would be refused
//! for its content still answers `forbidden`.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{World, assert_error};

#[tokio::test]
async fn an_integration_reaches_its_allow_list_and_nothing_else() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let board = world.member(&alice, "board", Role::Importer).await;
    let session = world.session(&alice, "laptop").await;
    let thread = world
        .thread(&alice, json!("project"), "For people", "hello")
        .await;
    let item = world.item(&alice, None, "Work").await;

    let allowed: Vec<(Method, String, Option<Value>, StatusCode)> = vec![
        (Method::GET, "/v1/me".into(), None, StatusCode::OK),
        (Method::GET, "/v1/events".into(), None, StatusCode::OK),
        (Method::GET, "/v1/items".into(), None, StatusCode::OK),
        (
            Method::GET,
            format!("/v1/items/{item}"),
            None,
            StatusCode::OK,
        ),
        (
            Method::POST,
            "/v1/items".into(),
            Some(json!({"ref": "gh:acme/widgets#9", "title": "Imported"})),
            StatusCode::CREATED,
        ),
        (Method::GET, "/v1/members".into(), None, StatusCode::OK),
        (
            Method::PUT,
            "/v1/items".into(),
            Some(
                json!({"source": "board", "items": [{"ref": "gh:acme/widgets#10", "title": "Imported"}]}),
            ),
            StatusCode::OK,
        ),
        (Method::GET, "/v1/ready".into(), None, StatusCode::OK),
        (Method::GET, "/v1/slots".into(), None, StatusCode::OK),
        (
            Method::GET,
            "/v1/slots/deploy".into(),
            None,
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            "/v1/slots/deploy/run-finished".into(),
            Some(json!({"workflow": "deploy.yml", "sha": "abc1234", "conclusion": "success"})),
            StatusCode::OK,
        ),
    ];
    for (method, path, body, status) in allowed {
        let answer = world
            .call(method.clone(), &path, Some(&board.token), body)
            .await;
        assert_eq!(answer.status, status, "{method} {path}: {}", answer.body);
    }

    let claim_body = || Some(json!({"session_id": session, "reason": "mine now"}));
    let refused: Vec<(Method, String, Option<Value>)> = vec![
        (
            Method::POST,
            "/v1/sessions".into(),
            Some(json!({"host": "worker", "label": "worker/mcp", "agent": "bot"})),
        ),
        (Method::POST, "/v1/sessions".into(), Some(json!({}))),
        (
            Method::POST,
            "/v1/heartbeat".into(),
            Some(json!({"session_id": session})),
        ),
        (
            Method::POST,
            format!("/v1/items/{item}/claim"),
            claim_body(),
        ),
        (
            Method::POST,
            format!("/v1/items/{item}/takeover"),
            claim_body(),
        ),
        (Method::POST, format!("/v1/items/{item}/release"), None),
        (Method::POST, format!("/v1/items/{item}/done"), None),
        (
            Method::POST,
            format!("/v1/items/{item}/dependencies"),
            Some(json!({"blocked_by": item})),
        ),
        (
            Method::POST,
            format!("/v1/items/{item}/dependencies"),
            Some(json!({})),
        ),
        (
            Method::DELETE,
            format!("/v1/items/{item}/dependencies?blocked_by={item}"),
            None,
        ),
        (Method::DELETE, "/v1/items/999/dependencies".into(), None),
        (Method::POST, "/v1/items/999/release".into(), None),
        (Method::GET, "/v1/status".into(), None),
        (
            Method::POST,
            "/v1/edit-check".into(),
            Some(json!({"session_id": session, "paths": [{"repo": "default", "path": "a"}]})),
        ),
        (
            Method::POST,
            "/v1/grants".into(),
            Some(json!({"item": item, "area": "billing", "to": "alice"})),
        ),
        (Method::DELETE, "/v1/grants/1".into(), None),
        (Method::DELETE, "/v1/grants/999".into(), None),
        (Method::POST, "/v1/threads".into(), Some(json!({}))),
        (Method::GET, "/v1/threads".into(), None),
        (Method::GET, "/v1/threads?item=x".into(), None),
        (Method::GET, format!("/v1/threads/{thread}"), None),
        (Method::GET, "/v1/threads/abc".into(), None),
        (
            Method::POST,
            format!("/v1/threads/{thread}/messages"),
            Some(json!({})),
        ),
        (Method::POST, format!("/v1/threads/{thread}/read"), None),
        (Method::GET, "/v1/inbox".into(), None),
        (Method::GET, "/v1/inbox?unread=maybe".into(), None),
        (Method::GET, "/v1/inbox?unacked=1".into(), None),
        (Method::POST, "/v1/messages/999/ack".into(), None),
        (Method::POST, "/v1/messages/abc/ack".into(), None),
        (Method::GET, "/v1/messages/999/status".into(), None),
        (Method::GET, "/v1/events/stream".into(), None),
        (Method::GET, "/v1/events/stream?after=x".into(), None),
        (Method::POST, "/v1/admin/members".into(), Some(json!({}))),
        (Method::POST, "/v1/admin/members/alice/tokens".into(), None),
        (Method::DELETE, "/v1/admin/tokens/zzzzzzzz".into(), None),
        (Method::PUT, "/v1/admin/settings".into(), Some(json!({}))),
        (
            Method::POST,
            format!("/v1/admin/items/{item}/force-release"),
            None,
        ),
        (
            Method::POST,
            "/v1/slots/deploy/take".into(),
            Some(json!({"session_id": session, "purpose": "ship"})),
        ),
        (
            Method::POST,
            "/v1/slots/deploy/release".into(),
            Some(json!({})),
        ),
        (Method::POST, "/v1/slots/deploy/leave".into(), None),
        (
            Method::POST,
            "/v1/slots/deploy/heartbeat".into(),
            Some(json!({"run_id": 1})),
        ),
        (
            Method::POST,
            "/v1/slots/deploy/takeover".into(),
            Some(json!({"session_id": session, "purpose": "x", "reason": "y"})),
        ),
        (
            Method::POST,
            "/v1/admin/slots/deploy/force-release".into(),
            Some(json!({"session_id": session, "reason": "y"})),
        ),
    ];
    for (method, path, body) in refused {
        let answer = world
            .call(method.clone(), &path, Some(&board.token), body)
            .await;
        assert_eq!(
            answer.status,
            StatusCode::FORBIDDEN,
            "{method} {path}: {}",
            answer.body
        );
        assert_error(&answer, StatusCode::FORBIDDEN, "forbidden");
    }
}

#[tokio::test]
async fn an_integration_sees_no_conversation_in_the_feed() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let carol = world.member(&alice, "carol", Role::Member).await;
    let board = world.member(&alice, "board", Role::Importer).await;
    world
        .thread(&alice, json!(["bob"]), "Private", "Just us")
        .await;
    world
        .thread(&alice, json!("project"), "Everyone", "Hello all")
        .await;
    let item = world.item(&alice, None, "After the threads").await;
    let kinds = |body: &Value| -> Vec<String> {
        body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap().to_owned())
            .collect()
    };
    let boards = world.get(&board, "/v1/events").await;
    assert_eq!(kinds(&boards.body), ["item_created"]);
    assert_eq!(boards.body["events"][0]["payload"]["item"], item);
    let carols = world.get(&carol, "/v1/events").await;
    assert_eq!(
        kinds(&carols.body),
        ["thread_started", "message_posted", "item_created"]
    );
    let bobs = world.get(&bob, "/v1/events").await;
    assert_eq!(kinds(&bobs.body).len(), 5);
    assert_eq!(boards.body["next"], bobs.body["next"]);
    assert_eq!(carols.body["next"], bobs.body["next"]);
}
