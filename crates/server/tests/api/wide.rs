//! Wide claims through the router: a claim over many areas that holds none of them, the
//! announcement each holder is sent, the acknowledgement that grants, the verdicts in between, the
//! status that shows them, and the refusals.

use axum::http::StatusCode;
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Answer, Person, World, assert_error, two_repositories};

/// alice holds `api:billing`; bob and carol have sessions and an item each, and claim nothing.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    carol: Person,
    bob_session: u64,
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
        let alice_session = world.session(&alice, "laptop").await;
        let bob_session = world.session(&bob, "desk").await;
        let carol_session = world.session(&carol, "tower").await;
        let alice_item = world.item(&alice, Some("gh:acme/api#1"), "Invoices").await;
        let bob_item = world
            .item(&bob, Some("gh:acme/api#2"), "Upgrade the HTTP client")
            .await;
        let carol_item = world.item(&carol, None, "Search ranking").await;
        let team = Self {
            world,
            alice,
            bob,
            carol,
            bob_session,
            carol_session,
            bob_item,
            carol_item,
        };
        let held = team
            .claim(
                &team.alice,
                alice_session,
                alice_item,
                json!({"areas": ["api:billing"]}),
            )
            .await;
        assert_eq!(held.status, StatusCode::OK, "{}", held.body);
        team
    }

    async fn claim(&self, who: &Person, session: u64, item: u64, body: Value) -> Answer {
        let mut body = body;
        body["session_id"] = json!(session);
        self.world
            .post(who, &format!("/v1/items/{item}/claim"), body)
            .await
    }

    async fn wide(&self, areas: Value) -> Answer {
        self.claim(
            &self.bob,
            self.bob_session,
            self.bob_item,
            json!({"wide": {"areas": areas, "reason": "Upgrade the HTTP client everywhere"}}),
        )
        .await
    }

    async fn check(&self, who: &Person, session: u64, path: &str) -> Answer {
        self.world
            .post(
                who,
                "/v1/edit-check",
                json!({"session_id": session, "repo": "api", "paths": [path], "reserve": true}),
            )
            .await
    }
}

#[tokio::test]
async fn a_wide_claim_holds_nothing_and_announces_itself_to_each_holder() {
    let team = Team::new().await;
    let made = team.wide(json!(["api:billing", "search"])).await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    assert_eq!(made.body["claim"]["areas"], json!([]));
    assert_eq!(
        made.body["claim"]["wide"],
        json!({"areas": ["api:billing", "api:search"],
               "reason": "Upgrade the HTTP client everywhere"})
    );
    let announced = made.body["announced"].as_array().unwrap();
    assert_eq!(announced.len(), 1, "{}", made.body);
    assert_eq!(announced[0]["to"], "alice");
    assert_eq!(announced[0]["areas"], json!(["api:billing"]));
    let message = announced[0]["message_id"].as_u64().unwrap();

    let inbox = team.world.get(&team.alice, "/v1/inbox?unacked=1").await;
    let unacked = inbox.body["unacked"].as_array().unwrap();
    assert_eq!(unacked.len(), 1, "{}", inbox.body);
    assert_eq!(unacked[0]["message"]["id"], message);
    assert_eq!(unacked[0]["message"]["author"], "dibs");
    assert_eq!(unacked[0]["message"]["system"], true);
    assert_eq!(
        unacked[0]["thread"]["author"], "bob",
        "dibs writes it for bob"
    );
    assert_eq!(unacked[0]["message"]["importance"], "high");
    assert_eq!(unacked[0]["message"]["ack_required"], true);

    let status = team.world.get(&team.carol, "/v1/status").await;
    assert_eq!(
        status.body["holds"],
        json!([{"area": "api:billing", "holder": "alice",
        "item": status.body["holds"][0]["item"]}]),
        "a wide claim holds nothing"
    );
    let wide = status.body["wide"].as_array().unwrap();
    assert_eq!(wide.len(), 1, "{}", status.body);
    assert_eq!(wide[0]["claim"]["holder"], "bob");
    assert_eq!(wide[0]["item"]["id"], team.bob_item);
    assert_eq!(
        wide[0]["areas"],
        json!([
            {"area": "api:billing", "state": "waiting", "holder": "alice",
             "item": status.body["holds"][0]["item"]},
            {"area": "api:search", "state": "free", "holder": null, "item": null},
        ])
    );
    let events = team.world.get(&team.carol, "/v1/events").await;
    let kinds: Vec<&str> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"wide_claimed"), "{kinds:?}");
}

#[tokio::test]
async fn the_wide_claimers_edit_is_held_until_the_holder_acknowledges_then_granted() {
    let team = Team::new().await;
    let made = team.wide(json!(["api:billing", "api:search"])).await;
    let message = made.body["announced"][0]["message_id"].as_u64().unwrap();
    let held = team
        .check(&team.bob, team.bob_session, "src/billing/invoice.rs")
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.body);
    let verdict = &held.body["verdicts"][0];
    assert_eq!(verdict["verdict"], "held_by", "{}", held.body);
    assert_eq!(verdict["holder"], "alice");
    assert_eq!(verdict["reason"], "awaiting_ack");
    let free = team
        .check(&team.bob, team.bob_session, "src/search/index.rs")
        .await;
    assert_eq!(
        free.body["verdicts"][0]["verdict"], "allowed",
        "{}",
        free.body
    );

    let acked = team
        .world
        .post(
            &team.alice,
            &format!("/v1/messages/{message}/ack"),
            json!({}),
        )
        .await;
    assert_eq!(acked.status, StatusCode::CREATED, "{}", acked.body);
    let granted = acked.body["granted"].as_array().unwrap();
    assert_eq!(granted.len(), 1, "{}", acked.body);
    assert_eq!(granted[0]["area"], "api:billing");
    assert_eq!(granted[0]["from"], "alice");
    assert_eq!(granted[0]["to"], "bob");
    assert_eq!(granted[0]["item"], team.bob_item);

    let now = team
        .check(&team.bob, team.bob_session, "src/billing/invoice.rs")
        .await;
    assert_eq!(
        now.body["verdicts"][0],
        json!({"repo": "api", "path": "src/billing/invoice.rs", "verdict": "granted",
               "by": "alice", "area": "api:billing"})
    );
    let again = team
        .world
        .post(
            &team.alice,
            &format!("/v1/messages/{message}/ack"),
            json!({}),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["granted"], json!([]));
    let status = team.world.get(&team.bob, "/v1/status").await;
    assert_eq!(status.body["wide"][0]["areas"][0]["state"], "granted");
}

#[tokio::test]
async fn a_member_who_claims_inside_a_wide_claim_is_told_and_asked() {
    let team = Team::new().await;
    team.wide(json!(["api:billing", "api:search"])).await;
    let claimed = team
        .claim(
            &team.carol,
            team.carol_session,
            team.carol_item,
            json!({"areas": ["api:search"]}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    assert_eq!(claimed.body["claim"]["areas"], json!(["api:search"]));
    let inside = claimed.body["inside_wide"].as_array().unwrap();
    assert_eq!(inside.len(), 1, "{}", claimed.body);
    assert_eq!(inside[0]["area"], "api:search");
    assert_eq!(inside[0]["holder"], "bob");
    assert_eq!(inside[0]["item"]["ref"], "gh:acme/api#2");
    let message = inside[0]["message_id"].as_u64().unwrap();
    let inbox = team.world.get(&team.carol, "/v1/inbox?unacked=1").await;
    assert_eq!(inbox.body["unacked"][0]["message"]["id"], message);
    assert_eq!(claimed.body["announced"], json!([]));
}

#[tokio::test]
async fn wide_claims_are_refused_with_the_rule_they_break() {
    let team = Team::new().await;
    team.wide(json!(["api:billing"])).await;
    let overlap = team
        .claim(
            &team.carol,
            team.carol_session,
            team.carol_item,
            json!({"wide": {"areas": ["web:checkout", "api:billing"], "reason": "Rename"}}),
        )
        .await;
    assert_error(&overlap, StatusCode::CONFLICT, "wide_overlap");
    assert_eq!(overlap.body["error"]["details"]["holder"], "bob");
    assert_eq!(overlap.body["error"]["details"]["area"], "api:billing");
    assert_eq!(
        overlap.body["error"]["details"]["item_ref"],
        "gh:acme/api#2"
    );
    let both = team
        .claim(
            &team.carol,
            team.carol_session,
            team.carol_item,
            json!({"areas": ["web:checkout"],
                   "wide": {"areas": ["web:checkout"], "reason": "Rename"}}),
        )
        .await;
    assert_error(&both, StatusCode::UNPROCESSABLE_ENTITY, "wide_and_held");
    let empty = team
        .claim(
            &team.carol,
            team.carol_session,
            team.carol_item,
            json!({"wide": {"areas": [], "reason": "Rename"}}),
        )
        .await;
    assert_error(&empty, StatusCode::UNPROCESSABLE_ENTITY, "invalid_wide");
    for body in [
        json!({"wide": {"areas": ["web:checkout"]}}),
        json!({"wide": {"areas": ["web:checkout"], "reason": "  "}}),
        json!({"wide": {"reason": "Rename"}}),
    ] {
        let refused = team
            .claim(&team.carol, team.carol_session, team.carol_item, body)
            .await;
        assert_error(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        );
    }
    let unknown = team
        .claim(
            &team.carol,
            team.carol_session,
            team.carol_item,
            json!({"wide": {"areas": ["web:search"], "reason": "Rename"}}),
        )
        .await;
    assert_error(&unknown, StatusCode::UNPROCESSABLE_ENTITY, "unknown_area");
}
