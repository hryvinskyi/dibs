//! Importance and acknowledgements: messages written with an importance and asking to be
//! acknowledged, a recipient's acknowledgement and its refusals, what awaits a member in the inbox,
//! a thread read with who has acknowledged, and what an author sees of a message they sent.

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error};

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

    /// Alice starts a thread with bob whose first message asks to be acknowledged; answers the
    /// thread's and the message's ids.
    async fn asking(&self) -> (u64, u64) {
        let started = self
            .world
            .post(
                &self.alice,
                "/v1/threads",
                json!({
                    "subject": "Staging is down", "to": ["bob"], "label": "laptop/mcp",
                    "body": "Do not deploy until I say so.", "importance": "urgent",
                    "ack_required": true,
                }),
            )
            .await;
        assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
        (
            started.body["thread"]["id"].as_u64().unwrap(),
            started.body["message"]["id"].as_u64().unwrap(),
        )
    }
}

fn ids(list: &Value) -> Vec<u64> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["message"]["id"].as_u64().unwrap())
        .collect()
}

#[tokio::test]
async fn a_message_carries_its_importance_and_whether_it_asks_for_an_ack() {
    let team = Team::new().await;
    let world = &team.world;
    let (thread, message) = team.asking().await;
    let read = world.get(&team.bob, &format!("/v1/threads/{thread}")).await;
    assert_eq!(read.body["messages"][0]["id"], message);
    assert_eq!(read.body["messages"][0]["importance"], "urgent");
    assert_eq!(read.body["messages"][0]["ack_required"], true);
    let reply = world
        .post(
            &team.bob,
            &format!("/v1/threads/{thread}/messages"),
            json!({"label": "web", "body": "Holding."}),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    assert_eq!(reply.body["message"]["importance"], "normal");
    assert_eq!(reply.body["message"]["ack_required"], false);
    let asked_reply = world
        .post(
            &team.bob,
            &format!("/v1/threads/{thread}/messages"),
            json!({"label": "web", "body": "Who decides?", "importance": "high", "ack_required": true}),
        )
        .await;
    assert_eq!(asked_reply.body["message"]["importance"], "high");
    for bad in [json!("loud"), json!("URGENT")] {
        let refused = world
            .post(
                &team.alice,
                "/v1/threads",
                json!({"subject": "s", "to": ["bob"], "label": "web", "body": "b", "importance": bad}),
            )
            .await;
        assert_error(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        );
        assert_eq!(refused.body["error"]["details"]["field"], "importance");
    }
    let summaries = world.get(&team.bob, "/v1/threads").await;
    assert_eq!(summaries.body["threads"][0]["importance"], "urgent");
    assert_eq!(summaries.body["threads"][0]["awaiting_ack"], 1);
}

#[tokio::test]
async fn a_recipient_acks_once_and_the_thread_shows_who_has() {
    let team = Team::new().await;
    let world = &team.world;
    let (thread, message) = team.asking().await;
    let before = world
        .get(&team.alice, &format!("/v1/threads/{thread}"))
        .await;
    assert_eq!(
        before.body["messages"][0]["acks"],
        json!([{"member": "bob", "acked_at": null}])
    );
    world.fakes.clock.advance(Duration::minutes(3));
    let acked = world
        .post(&team.bob, &format!("/v1/messages/{message}/ack"), json!({}))
        .await;
    assert_eq!(acked.status, StatusCode::CREATED, "{}", acked.body);
    assert_eq!(
        acked.body,
        json!({"ack": {"message": message, "thread": thread, "member": "bob",
                       "at": "2026-03-02T09:03:00Z"}, "created": true})
    );
    let after = world
        .get(&team.alice, &format!("/v1/threads/{thread}"))
        .await;
    assert_eq!(
        after.body["messages"][0]["acks"],
        json!([{"member": "bob", "acked_at": "2026-03-02T09:03:00Z"}])
    );
    world.fakes.clock.advance(Duration::minutes(10));
    let again = world
        .post(&team.bob, &format!("/v1/messages/{message}/ack"), json!({}))
        .await;
    assert_eq!(
        again.status,
        StatusCode::OK,
        "a retry is no refusal: {}",
        again.body
    );
    assert_eq!(
        again.body,
        json!({"ack": {"message": message, "thread": thread, "member": "bob",
                       "at": "2026-03-02T09:03:00Z"}, "created": false}),
        "the first acknowledgement, with its time"
    );
    let events = world.get(&team.alice, "/v1/events").await;
    let last = events.body["events"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["kind"], "acked");
    let acked_events = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "acked")
        .count();
    assert_eq!(acked_events, 1, "a repeat appends no event");
    assert_eq!(
        last["payload"],
        json!({"thread": thread, "message": message, "by": team.bob.actor.member.get()})
    );
    let carols = world.get(&team.carol, "/v1/events").await;
    assert!(
        carols.body["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["kind"] != "acked"),
        "an ack of a thread one may not read is not seen"
    );
}

#[tokio::test]
async fn an_ack_is_refused_to_anyone_but_a_recipient_of_a_message_that_asks() {
    let team = Team::new().await;
    let world = &team.world;
    let (thread, message) = team.asking().await;
    let plain = world
        .post(
            &team.alice,
            &format!("/v1/threads/{thread}/messages"),
            json!({"label": "web", "body": "No answer needed."}),
        )
        .await;
    let plain = plain.body["message"]["id"].as_u64().unwrap();
    let ack = |who: &Person, id: String| {
        let path = format!("/v1/messages/{id}/ack");
        let who = who.clone();
        async move { world.post(&who, &path, json!({})).await }
    };
    assert_error(
        &ack(&team.alice, message.to_string()).await,
        StatusCode::FORBIDDEN,
        "not_a_recipient",
    );
    assert_error(
        &ack(&team.bob, plain.to_string()).await,
        StatusCode::CONFLICT,
        "ack_not_required",
    );
    assert_error(
        &ack(&team.carol, message.to_string()).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    for text in ["999", "abc", "0", "-1", "9223372036854775808"] {
        assert_error(
            &ack(&team.bob, text.to_owned()).await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
    }
}

#[tokio::test]
async fn the_inbox_lists_what_awaits_my_ack_when_asked() {
    let team = Team::new().await;
    let world = &team.world;
    let (_, first) = team.asking().await;
    world.fakes.clock.advance(Duration::minutes(1));
    let (_, second) = team.asking().await;
    let plain = world.get(&team.bob, "/v1/inbox").await;
    assert!(plain.body.get("unacked").is_none(), "{}", plain.body);
    assert_eq!(plain.body["threads"][0]["awaiting_ack"], 1);
    let asked = world.get(&team.bob, "/v1/inbox?unacked=1").await;
    assert_eq!(asked.status, StatusCode::OK);
    assert_eq!(ids(&asked.body["unacked"]), [second, first], "newest first");
    assert_eq!(
        asked.body["unacked"][0]["thread"]["subject"],
        "Staging is down"
    );
    assert_eq!(asked.body["unacked"][0]["message"]["author"], "alice");
    assert_eq!(asked.body["unacked"][0]["message"]["importance"], "urgent");
    world
        .post(&team.bob, &format!("/v1/messages/{second}/ack"), json!({}))
        .await;
    let left = world
        .get(&team.bob, "/v1/inbox?unread=true&unacked=true")
        .await;
    assert_eq!(ids(&left.body["unacked"]), [first]);
    let alices = world.get(&team.alice, "/v1/inbox?unacked=1").await;
    assert_eq!(alices.body["unacked"], json!([]), "never one's own message");
    assert_error(
        &world.get(&team.bob, "/v1/inbox?unacked=maybe").await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
}

#[tokio::test]
async fn its_author_sees_who_read_and_acked_a_message() {
    let team = Team::new().await;
    let world = &team.world;
    let started = world
        .post(
            &team.alice,
            "/v1/threads",
            json!({"subject": "Freeze", "to_project": true, "label": "web",
                   "body": "Freeze main.", "importance": "high", "ack_required": true}),
        )
        .await;
    let thread = started.body["thread"]["id"].as_u64().unwrap();
    let message = started.body["message"]["id"].as_u64().unwrap();
    world.fakes.clock.advance(Duration::minutes(2));
    world
        .post(&team.bob, &format!("/v1/threads/{thread}/read"), json!({}))
        .await;
    world.fakes.clock.advance(Duration::minutes(1));
    world
        .post(&team.bob, &format!("/v1/messages/{message}/ack"), json!({}))
        .await;
    let status = world
        .get(&team.alice, &format!("/v1/messages/{message}/status"))
        .await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    assert_eq!(status.body["message"]["id"], message);
    assert_eq!(status.body["thread"]["id"], thread);
    assert_eq!(
        status.body["recipients"],
        json!([
            {"member": "bob", "read_at": "2026-03-02T09:02:00Z", "acked_at": "2026-03-02T09:03:00Z"},
            {"member": "carol", "read_at": null, "acked_at": null},
        ]),
        "every person but its author, never the integration"
    );
    assert_error(
        &world
            .get(&team.bob, &format!("/v1/messages/{message}/status"))
            .await,
        StatusCode::FORBIDDEN,
        "not_author",
    );
    let private = team.asking().await.1;
    assert_error(
        &world
            .get(&team.carol, &format!("/v1/messages/{private}/status"))
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_error(
        &world.get(&team.alice, "/v1/messages/999/status").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}
