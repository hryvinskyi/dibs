//! dibs granting a request on the holder's behalf through the router: at the ask, when the
//! holder's claim is in review, and in the sweep once the project's `grant_after` has passed; and
//! an admin revoking any grant, which sticks.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{Answer, Person, World, two_repositories};

/// A project with two repositories: alice (admin), bob holding `api:billing` on his item, and
/// carol working on hers with no area.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    carol: Person,
    carol_session: u64,
    bob_item: u64,
    carol_item: u64,
}

impl Team {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let carol = world.member(&alice, "carol", Role::Member).await;
        world.settings(&alice, two_repositories()).await;
        let bob_session = world.session(&bob, "desk").await;
        let carol_session = world.session(&carol, "tablet").await;
        let bob_item = world.item(&bob, None, "Billing").await;
        let carol_item = world.item(&carol, None, "Search").await;
        for (who, session, item, areas) in [
            (&bob, bob_session, bob_item, json!(["api:billing"])),
            (&carol, carol_session, carol_item, json!([])),
        ] {
            let answer = world
                .post(
                    who,
                    &format!("/v1/items/{item}/claim"),
                    json!({"session_id": session, "areas": areas}),
                )
                .await;
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        }
        Self {
            world,
            alice,
            bob,
            carol,
            carol_session,
            bob_item,
            carol_item,
        }
    }

    async fn review(&self) {
        let answer = self
            .world
            .post(
                &self.bob,
                &format!("/v1/items/{}/review", self.bob_item),
                json!({"pr": "acme/api#9", "more": false}),
            )
            .await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    }

    async fn ask(&self) -> Answer {
        self.world
            .post(
                &self.carol,
                "/v1/grant-requests",
                json!({"item": self.carol_item, "area": "api:billing"}),
            )
            .await
    }

    /// The messages of the conversation, as `who` reads them.
    async fn messages(&self, who: &Person, thread: u64) -> Vec<Value> {
        let read = self.world.get(who, &format!("/v1/threads/{thread}")).await;
        assert_eq!(read.status, StatusCode::OK, "{}", read.body);
        read.body["messages"].as_array().unwrap().clone()
    }
}

#[tokio::test]
async fn a_request_to_a_claim_in_review_is_granted_at_once_and_an_admin_revokes_it_for_good() {
    let team = Team::new().await;
    team.review().await;

    let asked = team.ask().await;

    assert_eq!(asked.status, StatusCode::CREATED, "{}", asked.body);
    assert_eq!(asked.body["outcome"], "granted");
    assert_eq!(asked.body["holder"], "bob");
    assert_eq!(asked.body["holder_item"]["id"], team.bob_item);
    assert_eq!(asked.body["decided"], "in_review");
    let grant = asked.body["grant_id"].as_u64().expect("dibs gave a grant");
    let thread = asked.body["thread_id"]
        .as_u64()
        .expect("the request's conversation");
    let first = team.messages(&team.bob, thread).await.remove(0);
    assert_eq!(first["author"], "carol");
    assert_eq!(
        first["importance"], "normal",
        "nothing is left for bob to decide: {first}"
    );
    assert!(
        first["body"]
            .as_str()
            .unwrap()
            .contains("dibs granted it at once on your behalf"),
        "{first}"
    );
    for who in [&team.bob, &team.carol] {
        let messages = team.messages(who, thread).await;
        let reply = messages.last().unwrap();
        assert_eq!(reply["author"], "dibs");
        let text = reply["body"].as_str().unwrap();
        assert!(
            text.starts_with(&format!(
                "Granted by dibs: carol may edit api:billing for work item #{} \"Search\", on bob's behalf, because the claim holding the area is in review.",
                team.carol_item
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "dibs revoke-grant {grant}, or DELETE /v1/grants/{grant}"
            )),
            "{text}"
        );
    }
    let check = team
        .world
        .post(
            &team.carol,
            "/v1/edit-check",
            json!({"session_id": team.carol_session,
                   "repo": "api", "paths": ["src/billing/invoice.rs"]}),
        )
        .await;
    assert_eq!(
        check.body["verdicts"][0]["verdict"], "granted",
        "{}",
        check.body
    );

    let path = format!("/v1/grants/{grant}");
    let stranger = team
        .world
        .call(Method::DELETE, &path, Some(&team.carol.token), None)
        .await;
    assert_eq!(stranger.status, StatusCode::FORBIDDEN, "{}", stranger.body);
    assert_eq!(stranger.body["error"]["code"], "not_grantor");
    let revoked = team
        .world
        .call(Method::DELETE, &path, Some(&team.alice.token), None)
        .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(revoked.body["grant"]["id"], grant);
    assert_eq!(revoked.body["grant"]["from"], "bob");
    assert_eq!(revoked.body["grant"]["decided"], "in_review");

    let events = team.world.get(&team.alice, "/v1/events").await;
    let ended = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|event| event["kind"] == "grant_revoked")
        .cloned()
        .expect("the revoke is in the log");
    let admin = team.alice.actor.member.get();
    assert_eq!(ended["payload"]["by"], admin);
    assert_eq!(ended["payload"]["cause"], "revoked");
    assert_eq!(ended["members"][admin.to_string()], "alice");

    let again = team.ask().await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
    assert_eq!(
        again.body["outcome"], "asked",
        "the revoke sticks while both claims last: {}",
        again.body
    );
    assert_eq!(again.body["grant_id"], Value::Null);
}

#[tokio::test]
async fn the_sweep_grants_a_request_once_the_grant_delay_has_passed() {
    let team = Team::new().await;
    let asked = team.ask().await;
    assert_eq!(asked.status, StatusCode::CREATED, "{}", asked.body);
    assert_eq!(asked.body["outcome"], "asked");
    assert_eq!(asked.body["grant_id"], Value::Null);
    assert_eq!(asked.body["decided"], Value::Null);
    let thread = asked.body["thread_id"].as_u64().unwrap();

    team.world.fakes.clock.advance(Duration::minutes(9));
    let early = team.world.app.tick_grants.execute().await.unwrap();
    assert_eq!(early.granted, 0);
    team.world.fakes.clock.advance(Duration::minutes(1));
    let swept = team.world.app.tick_grants.execute().await.unwrap();

    assert_eq!((swept.granted, swept.settled), (1, 0));
    assert!(swept.failed.is_empty(), "{:?}", swept.failed);
    let reply = team.messages(&team.bob, thread).await.pop().unwrap();
    assert_eq!(reply["author"], "dibs");
    assert!(
        reply["body"]
            .as_str()
            .unwrap()
            .contains("on bob's behalf, because it waited the project's grant_after delay."),
        "{reply}"
    );
    let in_force = team.ask().await;
    assert_eq!(in_force.status, StatusCode::OK, "{}", in_force.body);
    assert_eq!(in_force.body["outcome"], "granted");
    assert_eq!(
        (&in_force.body["grant_id"], &in_force.body["decided"]),
        (&Value::Null, &Value::Null),
        "a grant given before this ask: {}",
        in_force.body
    );
    let events = team.world.get(&team.alice, "/v1/events").await;
    let granted = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "granted")
        .cloned()
        .unwrap();
    assert_eq!(granted["payload"]["decided"], "waited");
}

#[tokio::test]
async fn a_request_a_revoke_stands_against_is_left_to_the_holder_and_the_sweep_writes_nothing() {
    let team = Team::new().await;
    team.review().await;
    let granted = team.ask().await;
    let grant = granted.body["grant_id"]
        .as_u64()
        .expect("dibs gave a grant");
    let revoked = team
        .world
        .call(
            Method::DELETE,
            &format!("/v1/grants/{grant}"),
            Some(&team.alice.token),
            None,
        )
        .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);

    let again = team.ask().await;

    assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
    assert_eq!(again.body["outcome"], "asked");
    assert_eq!(again.body["left_to_holder"], true, "{}", again.body);
    assert_eq!(again.body["grant_id"], Value::Null);
    let thread = again.body["thread_id"].as_u64().unwrap();
    let messages = team.messages(&team.bob, thread).await;
    let [asking] = &messages[..] else {
        panic!("only the asking message: {messages:?}");
    };
    assert_eq!(asking["author"], "carol");
    assert_eq!(asking["importance"], "high");
    let text = asking["body"].as_str().unwrap();
    assert!(
        text.contains(
            "A revoke of your grant to carol in api:billing stands while both claims last, so dibs leaves this request to you to grant or answer: it does not grant it on your behalf."
        ),
        "{text}"
    );
    assert!(!text.contains("have passed since this request"), "{text}");
    let inbox = team.world.get(&team.bob, "/v1/inbox").await;
    assert_eq!(inbox.status, StatusCode::OK, "{}", inbox.body);
    assert_eq!(
        inbox.body["grant_asks"][0]["left_to_holder"], true,
        "bob's inbox says it is his to decide: {}",
        inbox.body
    );

    team.world.fakes.clock.advance(Duration::minutes(10));
    let before = team.world.get(&team.alice, "/v1/events").await.body["events"]
        .as_array()
        .unwrap()
        .len();
    let swept = team.world.app.tick_grants.execute().await.unwrap();

    assert!(swept.failed.is_empty(), "{:?}", swept.failed);
    assert_eq!(
        (swept.granted, swept.settled, swept.left_to_holders),
        (0, 0, 0),
        "the request left to bob is never judged again"
    );
    let after = team.world.get(&team.alice, "/v1/events").await.body["events"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(after, before, "the sweep writes nothing");
    assert_eq!(team.messages(&team.bob, thread).await.len(), 1);
}
