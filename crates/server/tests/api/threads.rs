//! Conversations: starting a thread, replying, reading, marking read and the inbox, each with its
//! refusals; a thread the caller may not read answers as missing.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error, request};

struct Team {
    world: World,
    alice: Person,
    bob: Person,
    carol: Person,
}

impl Team {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let carol = world.member(&alice, "carol", Role::Member).await;
        world.member(&alice, "board", Role::Importer).await;
        Self {
            world,
            alice,
            bob,
            carol,
        }
    }
}

fn start(to: Value, body: &str) -> Value {
    json!({"subject": "Invoice totals", "to": to, "label": "laptop/mcp", "body": body})
}

#[tokio::test]
async fn a_thread_starts_and_its_people_read_and_answer() {
    let team = Team::new().await;
    let world = &team.world;
    let item = world
        .item(&team.alice, Some("gh:acme/widgets#7"), "Rounding")
        .await;
    let started = world
        .post(
            &team.alice,
            "/v1/threads",
            json!({
                "subject": "Invoice totals", "to": ["bob"], "item": item,
                "label": "laptop/mcp", "body": "Can you look?\nThe totals are off.",
            }),
        )
        .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
    let thread = started.body["thread"]["id"].as_u64().unwrap();
    let first = started.body["message"]["id"].as_u64().unwrap();
    assert_eq!(
        started.body["thread"],
        json!({
            "id": thread, "subject": "Invoice totals", "item": item, "author": "alice",
            "to_project": false, "to": ["bob"], "created_at": "2026-03-02T09:00:00Z",
        })
    );
    assert_eq!(
        started.body["message"],
        json!({
            "id": first, "thread": thread, "author": "alice", "label": "laptop/mcp",
            "body": "Can you look?\nThe totals are off.", "importance": "normal",
            "ack_required": false, "created_at": "2026-03-02T09:00:00Z",
        })
    );

    let inbox = world.get(&team.bob, "/v1/inbox?unread=true").await;
    assert_eq!(inbox.status, StatusCode::OK);
    assert_eq!(inbox.body["threads"].as_array().unwrap().len(), 1);
    assert_eq!(inbox.body["threads"][0]["thread"]["id"], thread);
    assert_eq!(inbox.body["threads"][0]["messages"], 1);
    assert_eq!(inbox.body["threads"][0]["unread"], 1);
    assert_eq!(
        inbox.body["threads"][0]["last_message_at"],
        "2026-03-02T09:00:00Z"
    );

    let marked = world
        .post(&team.bob, &format!("/v1/threads/{thread}/read"), json!({}))
        .await;
    assert_eq!(marked.body, json!({"newly_read": 1}));
    let again = world
        .post(&team.bob, &format!("/v1/threads/{thread}/read"), json!({}))
        .await;
    assert_eq!(again.body, json!({"newly_read": 0}));

    let reply = world
        .post(
            &team.bob,
            &format!("/v1/threads/{thread}/messages"),
            json!({"label": "web", "body": "On it."}),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["message"]["author"], "bob");
    assert_eq!(reply.body["message"]["label"], "web");
    assert_eq!(reply.body["message"]["thread"], thread);

    let read = world
        .get(&team.alice, &format!("/v1/threads/{thread}"))
        .await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body["thread"]["to"], json!(["bob"]));
    let authors: Vec<&str> = read.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["author"].as_str().unwrap())
        .collect();
    assert_eq!(authors, ["alice", "bob"]);

    let waiting = world.get(&team.bob, "/v1/inbox?unread=1").await;
    assert_eq!(waiting.body, json!({"threads": [], "offers": []}));
    let everything = world.get(&team.bob, "/v1/inbox").await;
    assert_eq!(everything.body["threads"][0]["unread"], 0);
    let alices = world.get(&team.alice, "/v1/inbox?unread=true").await;
    assert_eq!(alices.body["threads"][0]["unread"], 1);

    let about = world
        .get(&team.alice, &format!("/v1/threads?item={item}"))
        .await;
    assert_eq!(about.body["threads"][0]["thread"]["id"], thread);
    let other = world.item(&team.alice, None, "Other").await;
    let unrelated = world
        .get(&team.alice, &format!("/v1/threads?item={other}"))
        .await;
    assert_eq!(unrelated.body, json!({"threads": []}));
}

#[tokio::test]
async fn a_thread_addressed_to_the_project_is_open_to_every_person() {
    let team = Team::new().await;
    let world = &team.world;
    let thread = world
        .thread(&team.alice, json!("project"), "Deploy at five", "Heads up.")
        .await;
    let read = world
        .get(&team.carol, &format!("/v1/threads/{thread}"))
        .await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body["thread"]["to_project"], true);
    assert_eq!(read.body["thread"]["to"], json!([]));
    let listed = world.get(&team.carol, "/v1/threads").await;
    assert_eq!(listed.body["threads"][0]["thread"]["id"], thread);
}

#[tokio::test]
async fn a_thread_the_caller_may_not_read_is_missing() {
    let team = Team::new().await;
    let world = &team.world;
    let thread = world
        .thread(&team.alice, json!(["bob"]), "Private", "Just us.")
        .await;
    for path in [
        format!("/v1/threads/{thread}"),
        "/v1/threads/999".to_owned(),
    ] {
        assert_error(
            &world.get(&team.carol, &path).await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
    }
    let post = world
        .post(
            &team.carol,
            &format!("/v1/threads/{thread}/messages"),
            json!({"label": "web", "body": "Me too"}),
        )
        .await;
    assert_error(&post, StatusCode::NOT_FOUND, "not_found");
    let mark = world
        .post(
            &team.carol,
            &format!("/v1/threads/{thread}/read"),
            json!({}),
        )
        .await;
    assert_error(&mark, StatusCode::NOT_FOUND, "not_found");
    for text in ["abc", "0", "-1", "9223372036854775808"] {
        let answer = world.get(&team.alice, &format!("/v1/threads/{text}")).await;
        assert_error(&answer, StatusCode::NOT_FOUND, "not_found");
    }
    assert_eq!(
        world.get(&team.carol, "/v1/threads").await.body,
        json!({"threads": []})
    );
    assert_eq!(
        world.get(&team.carol, "/v1/inbox").await.body,
        json!({"threads": [], "offers": []})
    );
}

#[tokio::test]
async fn starting_a_thread_is_refused_with_the_field_at_fault() {
    let team = Team::new().await;
    let world = &team.world;
    let cases = [
        (
            start(json!(["nobody"]), "hello"),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            start(json!(["board"]), "hello"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_a_recipient",
        ),
        (
            start(json!([]), "hello"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "no_recipients",
        ),
        (
            json!({"subject": "s", "label": "web", "body": "b"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"subject": "s", "to": ["bob"], "to_project": true, "label": "web", "body": "b"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"to": ["bob"], "label": "web", "body": "b"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"subject": "s", "to": ["bob"], "body": "b"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            start(json!(["bob"]), "   "),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"subject": "two\nlines", "to": ["bob"], "label": "web", "body": "b"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"subject": "s", "to": ["bob"], "item": 999, "label": "web", "body": "b"}),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            json!({"subject": "s", "to": ["bob"], "item": 0, "label": "web", "body": "b"}),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
    ];
    for (body, status, code) in cases {
        let answer = world.post(&team.alice, "/v1/threads", body.clone()).await;
        assert_error(&answer, status, code);
        if code == "invalid_request" {
            assert!(
                answer.body["error"]["details"]["field"].is_string(),
                "{body}: {}",
                answer.body
            );
        }
    }
    let threads = world.get(&team.alice, "/v1/threads").await;
    assert_eq!(threads.body, json!({"threads": []}));
    let not_json = world
        .send(
            request(
                Method::POST,
                "/v1/threads",
                Some(&team.alice.token),
                Some("{".to_owned()),
            ),
            crate::support::PEER,
        )
        .await;
    assert_error(&not_json, StatusCode::BAD_REQUEST, "invalid_json");
}

#[tokio::test]
async fn a_reply_and_the_queries_are_refused_with_the_field_at_fault() {
    let team = Team::new().await;
    let world = &team.world;
    let thread = world
        .thread(&team.alice, json!(["bob"]), "Billing", "hello")
        .await;
    for body in [
        json!({"body": "no label"}),
        json!({"label": "web"}),
        json!({"label": "web", "body": "a\u{0}b"}),
    ] {
        let answer = world
            .post(&team.bob, &format!("/v1/threads/{thread}/messages"), body)
            .await;
        assert_error(&answer, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    }
    for path in [
        "/v1/threads?item=x",
        "/v1/threads?item=0",
        "/v1/inbox?unread=yes",
    ] {
        let answer = world.get(&team.bob, path).await;
        assert_error(&answer, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    }
}

#[tokio::test]
async fn a_16_kib_message_fits_however_it_is_encoded() {
    let team = Team::new().await;
    let world = &team.world;
    let thread = world
        .thread(&team.alice, json!(["bob"]), "Long ones", "hello")
        .await;
    for text in [format!("x{}", "\n".repeat(16_383)), "ж".repeat(8_192)] {
        let answer = world
            .post(
                &team.bob,
                &format!("/v1/threads/{thread}/messages"),
                json!({"label": "web", "body": text}),
            )
            .await;
        assert_eq!(answer.status, StatusCode::CREATED, "{}", answer.body);
        assert_eq!(answer.body["message"]["body"], text);
    }
}

#[tokio::test]
async fn no_route_here_needs_more_than_a_token() {
    let team = Team::new().await;
    let world = &team.world;
    for (method, path) in [
        (Method::GET, "/v1/threads"),
        (Method::POST, "/v1/threads"),
        (Method::GET, "/v1/threads/1"),
        (Method::POST, "/v1/threads/1/messages"),
        (Method::POST, "/v1/threads/1/read"),
        (Method::GET, "/v1/inbox"),
    ] {
        let answer = world.call(method, path, None, None).await;
        assert_error(&answer, StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let answer = world
        .call(
            Method::GET,
            "/v1/inbox",
            Some(&format!("dibs_{}", "q".repeat(43))),
            None,
        )
        .await;
    assert_error(&answer, StatusCode::UNAUTHORIZED, "unauthorized");
}

#[tokio::test]
async fn another_projects_thread_is_not_found() {
    let team = Team::new().await;
    let world = &team.world;
    let dave = world.project("globex", "dave").await;
    let thread = world
        .thread(&team.alice, json!("project"), "Ours", "Only acme.")
        .await;
    assert_error(
        &world.get(&dave, &format!("/v1/threads/{thread}")).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let reply = world
        .post(
            &dave,
            &format!("/v1/threads/{thread}/messages"),
            json!({"label": "web", "body": "hi"}),
        )
        .await;
    assert_error(&reply, StatusCode::NOT_FOUND, "not_found");
    let to_alice = world
        .post(&dave, "/v1/threads", start(json!(["alice"]), "hi"))
        .await;
    assert_error(&to_alice, StatusCode::NOT_FOUND, "not_found");
    assert_eq!(
        world.get(&dave, "/v1/threads").await.body,
        json!({"threads": []})
    );
}

#[tokio::test]
async fn marking_read_counts_what_the_inbox_called_unread() {
    let team = Team::new().await;
    let world = &team.world;
    let thread = world
        .thread(&team.alice, json!(["bob"]), "Counts", "one")
        .await;
    for body in ["two", "three"] {
        world
            .post(
                &team.alice,
                &format!("/v1/threads/{thread}/messages"),
                json!({"label": "web", "body": body}),
            )
            .await;
    }
    for body in ["mine", "also mine"] {
        world
            .post(
                &team.bob,
                &format!("/v1/threads/{thread}/messages"),
                json!({"label": "web", "body": body}),
            )
            .await;
    }
    for who in [&team.bob, &team.alice] {
        let inbox = world.get(who, "/v1/inbox").await;
        let unread = inbox.body["threads"][0]["unread"].clone();
        let marked = world
            .post(who, &format!("/v1/threads/{thread}/read"), json!({}))
            .await;
        assert_eq!(marked.body["newly_read"], unread, "{}", inbox.body);
        let after = world.get(who, "/v1/inbox").await;
        assert_eq!(after.body["threads"][0]["unread"], 0);
    }
}
