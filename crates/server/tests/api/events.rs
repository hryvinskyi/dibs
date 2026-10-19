//! The event log through the router: the feed page by page, and the stream's answer, start, end and
//! limits. The stream's delivery over a real connection is in `stream`.

use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};

use dibs_domain::member::Role;
use dibs_server::http::HttpOptions;
use dibs_server::stream::StreamOptions;

use crate::support::{Person, World, assert_error};

const WAIT: Duration = Duration::from_secs(5);

fn stream_request(who: &Person, path: &str, last_event_id: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {}", who.token));
    if let Some(id) = last_event_id {
        builder = builder.header("Last-Event-ID", id);
    }
    builder.body(Body::empty()).unwrap()
}

/// A stream's body, read frame by frame.
struct Frames(Body);

impl Frames {
    /// The next piece of text the stream sends, or `None` once it ended.
    async fn next(&mut self) -> Option<String> {
        loop {
            let frame = tokio::time::timeout(WAIT, self.0.frame())
                .await
                .expect("the stream sent nothing in time")?
                .unwrap();
            if let Ok(data) = frame.into_data() {
                return Some(String::from_utf8(data.to_vec()).unwrap());
            }
        }
    }

    /// The next `count` events: their ids and data.
    async fn events(&mut self, count: usize) -> Vec<(u64, Value)> {
        let mut events = Vec::new();
        while events.len() < count {
            let text = self.next().await.expect("the stream ended");
            if text.starts_with(':') {
                continue;
            }
            let id_line = text.lines().next().unwrap();
            let id = id_line.strip_prefix("id: ").unwrap().parse().unwrap();
            let data: String = text
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .collect();
            events.push((id, serde_json::from_str(&data).unwrap()));
        }
        events
    }
}

async fn open(world: &World, request: Request<Body>) -> (StatusCode, Frames) {
    let response = world.open(request).await;
    (response.status(), Frames(response.into_body()))
}

#[tokio::test]
async fn the_feed_pages_by_cursor_and_names_members_by_handle() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let board = world.member(&alice, "board", Role::Importer).await;
    for n in 1..=3 {
        world
            .item(&alice, Some(&format!("gh:acme/widgets#{n}")), "Work")
            .await;
    }
    let all = world.get(&board, "/v1/events").await;
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    let events = all.body["events"].as_array().unwrap().clone();
    assert_eq!(events.len(), 3);
    let first = &events[0];
    assert_eq!(first["kind"], "item_created");
    assert_eq!(first["payload"]["external_ref"], "gh:acme/widgets#1");
    let by = first["payload"]["by"].as_u64().unwrap();
    assert_eq!(first["members"], json!({ by.to_string(): "alice" }));
    assert_eq!(first["at"], "2026-03-02T09:00:00Z");
    assert!(first.get("message").is_none());
    let ids: Vec<u64> = events.iter().map(|e| e["id"].as_u64().unwrap()).collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(all.body["next"], ids[2]);

    let page = world.get(&board, "/v1/events?after=0&limit=2").await;
    assert_eq!(page.body["events"].as_array().unwrap().len(), 2);
    assert_eq!(page.body["next"], ids[1]);
    let rest = world
        .get(&board, &format!("/v1/events?after={}", ids[1]))
        .await;
    assert_eq!(rest.body["events"][0]["id"], ids[2]);
    let end = world
        .get(&board, &format!("/v1/events?after={}", ids[2]))
        .await;
    assert_eq!(end.body, json!({"events": [], "next": ids[2]}));

    for path in [
        "/v1/events?after=x",
        "/v1/events?after=-1",
        "/v1/events?limit=0",
        "/v1/events?limit=501",
    ] {
        let answer = world.get(&board, path).await;
        assert_error(&answer, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    }
    let unauthorized = world.call(Method::GET, "/v1/events", None, None).await;
    assert_error(&unauthorized, StatusCode::UNAUTHORIZED, "unauthorized");
}

#[tokio::test]
async fn the_feed_carries_no_subject_or_text_and_stays_in_its_project() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let dave = world.project("globex", "dave").await;
    world
        .thread(&alice, json!(["bob"]), "Secret subject", "Secret body")
        .await;
    let feed = world.get(&bob, "/v1/events").await;
    let text = feed.body.to_string();
    assert!(!text.contains("Secret"), "{text}");
    let kinds: Vec<&str> = feed.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["thread_started", "message_posted"]);
    let empty = world.get(&dave, "/v1/events").await;
    assert_eq!(empty.body, json!({"events": [], "next": null}));
}

#[tokio::test]
async fn a_stream_answers_as_an_event_stream_and_replays_from_a_cursor() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let first = world.item(&alice, None, "One").await;
    world.item(&alice, None, "Two").await;
    let (status, mut frames) = open(
        &world,
        stream_request(&alice, "/v1/events/stream?after=0", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = frames.events(2).await;
    assert_eq!(events[0].1["payload"]["item"], first);
    assert_eq!(events[0].0, events[0].1["id"].as_u64().unwrap());

    let response = world
        .open(stream_request(&alice, "/v1/events/stream", None))
        .await;
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-cache, no-transform"
    );
}

#[tokio::test]
async fn a_stream_is_refused_to_an_integration_and_without_a_token() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let board = world.member(&alice, "board", Role::Importer).await;
    let refused = world
        .send(
            stream_request(&board, "/v1/events/stream", None),
            crate::support::PEER,
        )
        .await;
    assert_error(&refused, StatusCode::FORBIDDEN, "forbidden");
    let anonymous = world
        .call(Method::GET, "/v1/events/stream", None, None)
        .await;
    assert_error(&anonymous, StatusCode::UNAUTHORIZED, "unauthorized");
    for (path, header) in [
        ("/v1/events/stream?after=x", None),
        ("/v1/events/stream", Some("not-a-number")),
        ("/v1/events/stream?after=1", Some("-4")),
    ] {
        let answer = world
            .send(stream_request(&alice, path, header), crate::support::PEER)
            .await;
        assert_error(&answer, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    }
}

#[tokio::test]
async fn an_idle_stream_sends_keep_alive_comments() {
    let options = StreamOptions {
        heartbeat: Duration::from_millis(50),
        ..StreamOptions::default()
    };
    let world = World::with(HttpOptions::default(), options).await;
    let alice = world.project("acme", "alice").await;
    let (_, mut frames) = open(&world, stream_request(&alice, "/v1/events/stream", None)).await;
    assert_eq!(frames.next().await.unwrap(), ": keep-alive\n\n");
    assert_eq!(frames.next().await.unwrap(), ": keep-alive\n\n");
}

#[tokio::test]
async fn a_stream_outlives_the_request_timeout() {
    let http = HttpOptions {
        timeout: Duration::from_millis(100),
        ..HttpOptions::default()
    };
    let stream = StreamOptions {
        heartbeat: Duration::from_millis(60),
        ..StreamOptions::default()
    };
    let world = World::with(http, stream).await;
    let alice = world.project("acme", "alice").await;
    let (status, mut frames) =
        open(&world, stream_request(&alice, "/v1/events/stream", None)).await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..5 {
        assert_eq!(frames.next().await.unwrap(), ": keep-alive\n\n");
    }
    world.item(&alice, None, "Still delivered").await;
    let events = frames.events(1).await;
    assert_eq!(events[0].1["kind"], "item_created");
}

#[tokio::test]
async fn a_stream_ends_when_its_token_is_revoked() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let (_, mut frames) = open(&world, stream_request(&bob, "/v1/events/stream", None)).await;
    let prefix = &bob.token["dibs_".len().."dibs_".len() + 8];
    let revoked = world
        .delete(&alice, &format!("/v1/admin/tokens/{prefix}"))
        .await;
    assert_eq!(revoked.status, StatusCode::OK);
    world.item(&alice, None, "Wakes the stream").await;
    assert_eq!(frames.next().await, None);
}

#[tokio::test]
async fn a_stream_ends_when_the_server_stops() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let (_, mut frames) = open(&world, stream_request(&alice, "/v1/events/stream", None)).await;
    world.shutdown.stop();
    assert_eq!(frames.next().await, None);
    let (status, mut late) = open(&world, stream_request(&alice, "/v1/events/stream", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(late.next().await, None);
}

#[tokio::test]
async fn streams_beyond_the_limit_wait_for_one_to_close() {
    let options = StreamOptions {
        max_open: 1,
        ..StreamOptions::default()
    };
    let world = World::with(HttpOptions::default(), options).await;
    let alice = world.project("acme", "alice").await;
    let first = world
        .open(stream_request(&alice, "/v1/events/stream", None))
        .await;
    assert_eq!(first.status(), StatusCode::OK);
    let refused = world
        .send(
            stream_request(&alice, "/v1/events/stream", None),
            crate::support::PEER,
        )
        .await;
    assert_error(
        &refused,
        StatusCode::SERVICE_UNAVAILABLE,
        "too_many_streams",
    );
    drop(first);
    let next = world
        .open(stream_request(&alice, "/v1/events/stream", None))
        .await;
    assert_eq!(next.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_backlog_longer_than_a_page_arrives_whole_and_in_order() {
    let options = StreamOptions {
        page: 2,
        ..StreamOptions::default()
    };
    let world = World::with(HttpOptions::default(), options).await;
    let alice = world.project("acme", "alice").await;
    for n in 1..=7 {
        world.item(&alice, None, &format!("Item {n}")).await;
    }
    let feed = world.get(&alice, "/v1/events").await;
    let logged: Vec<u64> = feed.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_u64().unwrap())
        .collect();
    let (_, mut frames) = open(
        &world,
        stream_request(&alice, "/v1/events/stream?after=0", None),
    )
    .await;
    let streamed: Vec<u64> = frames.events(7).await.iter().map(|(id, _)| *id).collect();
    assert_eq!(streamed, logged);
}

#[tokio::test]
async fn a_member_holds_at_most_eight_streams() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let mut alices = Vec::new();
    for _ in 0..8 {
        let response = world
            .open(stream_request(&alice, "/v1/events/stream", None))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        alices.push(response);
    }
    let ninth = world
        .send(
            stream_request(&alice, "/v1/events/stream", None),
            crate::support::PEER,
        )
        .await;
    assert_error(&ninth, StatusCode::SERVICE_UNAVAILABLE, "too_many_streams");
    let message = ninth.body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("already have 8 event streams open"),
        "{message}"
    );
    let bobs = world
        .open(stream_request(&bob, "/v1/events/stream", None))
        .await;
    assert_eq!(bobs.status(), StatusCode::OK);
    alices.pop();
    let again = world
        .open(stream_request(&alice, "/v1/events/stream", None))
        .await;
    assert_eq!(again.status(), StatusCode::OK);
}
