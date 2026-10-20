//! Delivery while a member works: the messages still to be delivered in the inbox, the first of a
//! member's sessions claiming a message or an offer and every later one refused, and the limit on
//! the high and urgent messages a member writes within an hour.

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error};

struct Team {
    world: World,
    alice: Person,
    bob: Person,
}

impl Team {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        Self { world, alice, bob }
    }

    async fn send(&self, from: &Person, to: &str, importance: &str) -> u64 {
        let started = self
            .world
            .post(
                from,
                "/v1/threads",
                json!({
                    "subject": "Staging", "to": [to], "label": "laptop/mcp",
                    "body": "Hold every deploy.", "importance": importance,
                }),
            )
            .await;
        assert_eq!(started.status, StatusCode::CREATED, "{}", started.body);
        started.body["message"]["id"].as_u64().unwrap()
    }

    async fn deliverable(&self, who: &Person) -> Vec<u64> {
        let inbox = self
            .world
            .get(who, "/v1/inbox?unread=true&unacked=1&deliverable=1")
            .await;
        assert_eq!(inbox.status, StatusCode::OK, "{}", inbox.body);
        inbox.body["deliverable"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["message"]["id"].as_u64().unwrap())
            .collect()
    }
}

#[tokio::test]
async fn a_message_is_delivered_by_the_first_session_that_claims_it_and_never_again() {
    let team = Team::new().await;
    let world = &team.world;
    let urgent = team.send(&team.alice, "bob", "urgent").await;
    team.send(&team.alice, "bob", "normal").await;
    assert_eq!(team.deliverable(&team.bob).await, [urgent]);
    let plain = world.get(&team.bob, "/v1/inbox").await;
    assert_eq!(plain.body.get("deliverable"), None, "only when asked for");
    let first = world.session(&team.bob, "laptop").await;
    let second = world.session(&team.bob, "desktop").await;
    let claimed = world
        .post(
            &team.bob,
            &format!("/v1/messages/{urgent}/deliver"),
            json!({"session_id": first}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::CREATED, "{}", claimed.body);
    assert_eq!(claimed.body["delivery"]["subject"], urgent.to_string());
    assert_eq!(claimed.body["delivery"]["session_id"], first);
    let again = world
        .post(
            &team.bob,
            &format!("/v1/messages/{urgent}/deliver"),
            json!({"session_id": second}),
        )
        .await;
    assert_error(&again, StatusCode::CONFLICT, "delivered");
    assert_eq!(again.body["error"]["details"]["session_id"], first);
    assert!(team.deliverable(&team.bob).await.is_empty());
    let own = world.session(&team.alice, "laptop").await;
    let author = world
        .post(
            &team.alice,
            &format!("/v1/messages/{urgent}/deliver"),
            json!({"session_id": own}),
        )
        .await;
    assert_error(&author, StatusCode::FORBIDDEN, "not_a_recipient");
    let foreign = world
        .post(
            &team.bob,
            &format!("/v1/messages/{urgent}/deliver"),
            json!({"session_id": own}),
        )
        .await;
    assert_error(&foreign, StatusCode::NOT_FOUND, "not_found");
    let missing = world
        .post(
            &team.bob,
            &format!("/v1/messages/{urgent}/deliver"),
            json!({}),
        )
        .await;
    assert_error(
        &missing,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
}

#[tokio::test]
async fn an_offer_is_delivered_once_while_it_stands() {
    let team = Team::new().await;
    let world = &team.world;
    world
        .settings(
            &team.alice,
            json!({"slots": [{"name": "deploy:prod", "max_hold": "30m", "offer_for": "5m"}]}),
        )
        .await;
    let a = world.session(&team.alice, "laptop").await;
    let b = world.session(&team.bob, "laptop").await;
    for (who, session) in [(&team.alice, a), (&team.bob, b)] {
        let taken = world
            .post(
                who,
                "/v1/slots/deploy:prod/take",
                json!({"session_id": session, "purpose": "ship"}),
            )
            .await;
        assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    }
    let released = world
        .post(&team.alice, "/v1/slots/deploy:prod/release", json!({}))
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.body);
    let inbox = world.get(&team.bob, "/v1/inbox?unread=true").await;
    let until = inbox.body["offers"][0]["until"]
        .as_str()
        .unwrap()
        .to_owned();
    let bob = &team.bob;
    let claim = |session: u64, until: &str| {
        let body = json!({"session_id": session, "until": until});
        async move {
            world
                .post(bob, "/v1/slots/deploy:prod/offer/deliver", body)
                .await
        }
    };
    let won = claim(b, &until).await;
    assert_eq!(won.status, StatusCode::CREATED, "{}", won.body);
    assert!(
        won.body["delivery"]["subject"]
            .as_str()
            .unwrap()
            .starts_with("offer:deploy:prod:"),
        "{}",
        won.body
    );
    assert_error(&claim(b, &until).await, StatusCode::CONFLICT, "delivered");
    assert_error(
        &claim(b, "2026-03-02T09:30:00Z").await,
        StatusCode::CONFLICT,
        "offer_gone",
    );
    assert_error(
        &claim(b, "soon").await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    world.fakes.clock.advance(Duration::minutes(6));
    assert_error(&claim(b, &until).await, StatusCode::CONFLICT, "offer_gone");
}

#[tokio::test]
async fn the_eleventh_raised_message_within_an_hour_is_refused_saying_when_it_frees() {
    let team = Team::new().await;
    for _ in 0..10 {
        team.send(&team.alice, "bob", "high").await;
        team.world.fakes.clock.advance(Duration::minutes(1));
    }
    let refused = team
        .world
        .post(
            &team.alice,
            "/v1/threads",
            json!({
                "subject": "Again", "to": ["bob"], "label": "laptop/mcp",
                "body": "Still holding?", "importance": "urgent",
            }),
        )
        .await;
    assert_error(
        &refused,
        StatusCode::TOO_MANY_REQUESTS,
        "importance_limited",
    );
    let message = refused.body["error"]["message"].as_str().unwrap();
    assert!(message.contains("after 10:00 UTC"), "{message}");
    assert_eq!(refused.body["error"]["details"]["limit"], 10);
    assert_eq!(
        refused.body["error"]["details"]["frees_at"],
        "2026-03-02T10:00:00Z"
    );
    team.send(&team.alice, "bob", "normal").await;
    team.send(&team.bob, "alice", "urgent").await;
    team.world.fakes.clock.advance(Duration::minutes(50));
    team.send(&team.alice, "bob", "urgent").await;
}

#[tokio::test]
async fn acknowledging_an_ended_wide_announcement_grants_nothing_and_says_so() {
    let team = Team::new().await;
    let world = &team.world;
    world
        .settings(
            &team.alice,
            json!({"areas": [{"name": "billing", "paths": ["b/**"]}]}),
        )
        .await;
    let a = world.session(&team.alice, "laptop").await;
    let b = world.session(&team.bob, "laptop").await;
    let held = world
        .item(&team.alice, Some("gh:acme/widgets#1"), "Held")
        .await;
    let wide = world
        .item(&team.bob, Some("gh:acme/widgets#2"), "Wide")
        .await;
    let claimed = world
        .post(
            &team.alice,
            &format!("/v1/items/{held}/claim"),
            json!({"session_id": a, "areas": ["billing"]}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    let widened = world
        .post(
            &team.bob,
            &format!("/v1/items/{wide}/claim"),
            json!({"session_id": b, "wide": {"areas": ["billing"], "reason": "Rename the client"}}),
        )
        .await;
    assert!(widened.status.is_success(), "{}", widened.body);
    let announced = team.deliverable(&team.alice).await;
    assert_eq!(announced.len(), 1);
    let released = world
        .post(&team.bob, &format!("/v1/items/{wide}/release"), json!({}))
        .await;
    assert!(released.status.is_success(), "{}", released.body);
    assert!(team.deliverable(&team.alice).await.is_empty());
    let unacked = world.get(&team.alice, "/v1/inbox?unacked=1").await;
    assert_eq!(unacked.body["unacked"], Value::Array(Vec::new()));
    let acked = world
        .post(
            &team.alice,
            &format!("/v1/messages/{}/ack", announced[0]),
            json!({}),
        )
        .await;
    assert_eq!(acked.status, StatusCode::CREATED, "{}", acked.body);
    assert_eq!(acked.body["granted"], json!([]));
    assert_eq!(acked.body["announcement_ended"], true);
}
