//! Taking an area from debt work through the router: a feature claim waits for an area a debt claim
//! holds, the debt holder is asked, the edits in between are held with the way on, the narrowing
//! hands the area over, and the sweep escalates a request nobody answered.

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{Answer, Person, World, assert_error, two_repositories};

/// alice works on debt in `api:billing` and `api:search`; bob and carol, an arbiter, have a session
/// and an item each.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    alice_session: u64,
    bob_session: u64,
    debt: u64,
    feature: u64,
}

impl Team {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        world.member(&alice, "carol", Role::Member).await;
        let mut settings = two_repositories();
        settings["arbiters"] = json!(["carol"]);
        world.settings(&alice, settings).await;
        let alice_session = world.session(&alice, "laptop").await;
        let bob_session = world.session(&bob, "desk").await;
        let debt = world
            .item(&alice, Some("gh:acme/api#7"), "Tidy the invoice module")
            .await;
        let feature = world
            .item(&bob, Some("gh:acme/api#42"), "Currency on invoices")
            .await;
        let team = Self {
            world,
            alice,
            bob,
            alice_session,
            bob_session,
            debt,
            feature,
        };
        let held = team
            .claim(
                &team.alice,
                team.alice_session,
                team.debt,
                json!({"areas": ["api:billing", "api:search"], "kind": "debt"}),
            )
            .await;
        assert_eq!(held.status, StatusCode::OK, "{}", held.body);
        assert_eq!(held.body["claim"]["kind"], "debt");
        team
    }

    async fn claim(&self, who: &Person, session: u64, item: u64, body: Value) -> Answer {
        let mut body = body;
        body["session_id"] = json!(session);
        self.world
            .post(who, &format!("/v1/items/{item}/claim"), body)
            .await
    }

    async fn bob_claims(&self, areas: Value) -> Answer {
        self.claim(
            &self.bob,
            self.bob_session,
            self.feature,
            json!({ "areas": areas }),
        )
        .await
    }

    async fn check(&self, path: &str) -> Value {
        let answer = self
            .world
            .post(
                &self.bob,
                "/v1/edit-check",
                json!({"session_id": self.bob_session, "repo": "api", "paths": [path], "reserve": true}),
            )
            .await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        answer.body["verdicts"][0].clone()
    }

    async fn narrow(&self, who: &Person, item: u64, body: Value) -> Answer {
        self.world
            .post(who, &format!("/v1/items/{item}/claim/narrow"), body)
            .await
    }

    async fn event_kinds(&self) -> Vec<String> {
        let events = self.world.get(&self.bob, "/v1/events").await;
        events.body["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["kind"].as_str().map(str::to_owned))
            .collect()
    }
}

#[tokio::test]
async fn a_feature_claim_waits_for_a_debt_held_area_and_the_holder_is_asked() {
    let team = Team::new().await;
    let made = team
        .bob_claims(json!(["api:billing", "web:checkout"]))
        .await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let claim = &made.body["claim"];
    assert_eq!(claim["kind"], "feature");
    assert_eq!(claim["areas"], json!(["web:checkout"]));
    assert_eq!(claim["pending"]["api:billing"]["from"], team.debt);
    assert!(claim["pending"]["api:billing"]["asked_at"].is_string());
    assert_eq!(
        made.body["pending"],
        json!([{"area": "api:billing", "holder": "alice", "asked": true,
                "item": made.body["pending"][0]["item"]}])
    );
    assert_eq!(made.body["pending"][0]["item"]["id"], team.debt);

    let inbox = team.world.get(&team.alice, "/v1/inbox?unacked=1").await;
    let unacked = inbox.body["unacked"].as_array().unwrap();
    assert_eq!(unacked.len(), 1, "{}", inbox.body);
    let message = &unacked[0]["message"];
    assert_eq!(message["importance"], "urgent");
    assert_eq!(message["ack_required"], true);
    assert_eq!(message["author"], "dibs");
    assert_eq!(unacked[0]["thread"]["author"], "bob");
    assert!(
        unacked[0]["thread"]["subject"]
            .as_str()
            .unwrap()
            .starts_with("Yield api:billing to gh:acme/api#42, priority 2"),
        "{}",
        inbox.body
    );
    assert!(
        team.event_kinds()
            .await
            .contains(&"yield_requested".to_owned())
    );
}

#[tokio::test]
async fn a_claim_another_feature_holds_an_area_of_is_refused_whole_and_so_is_debt_work() {
    let team = Team::new().await;
    let refused = team
        .claim(
            &team.bob,
            team.bob_session,
            team.feature,
            json!({"areas": ["api:billing"], "kind": "chore"}),
        )
        .await;
    assert_error(
        &refused,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    let debt_work = team
        .claim(
            &team.bob,
            team.bob_session,
            team.feature,
            json!({"areas": ["api:billing"], "kind": "debt"}),
        )
        .await;
    assert_error(&debt_work, StatusCode::CONFLICT, "area_held");
    let made = team.bob_claims(json!(["api:billing"])).await;
    assert_eq!(made.status, StatusCode::OK, "{}", made.body);
    let other = team
        .world
        .item(&team.bob, Some("gh:acme/api#43"), "Tax on invoices")
        .await;
    let second = team
        .claim(
            &team.bob,
            team.bob_session,
            other,
            json!({"areas": ["web:checkout", "api:billing"]}),
        )
        .await;
    assert_error(&second, StatusCode::CONFLICT, "area_held");
    let claims = team.world.get(&team.bob, "/v1/status").await;
    let live = claims.body["live"].as_array().unwrap();
    assert_eq!(live.len(), 2, "nothing was claimed: {}", claims.body);
}

#[tokio::test]
async fn edits_in_between_are_held_with_the_way_on() {
    let team = Team::new().await;
    team.bob_claims(json!(["web:checkout"])).await;
    let ask = team.check("src/search/index.rs").await;
    assert_eq!(ask["verdict"], "held_by", "{ask}");
    assert_eq!(ask["holder"], "alice");
    assert_eq!(ask["reason"], "ask_handover");
    assert_eq!(ask["claim"], team.feature);
    team.bob_claims(json!(["api:billing"])).await;
    let waiting = team.check("src/billing/invoice.rs").await;
    assert_eq!(waiting["reason"], "awaiting_handover", "{waiting}");
    assert!(waiting.get("claim").is_none(), "{waiting}");
}

#[tokio::test]
async fn narrowing_hands_the_area_over_and_a_repeat_changes_nothing() {
    let team = Team::new().await;
    team.bob_claims(json!(["api:billing"])).await;
    let missing = team.narrow(&team.alice, team.debt, json!({})).await;
    assert_error(
        &missing,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    let not_theirs = team
        .narrow(&team.bob, team.debt, json!({"areas": ["api:billing"]}))
        .await;
    assert_error(&not_theirs, StatusCode::FORBIDDEN, "not_holder");

    let narrowed = team
        .narrow(&team.alice, team.debt, json!({"areas": ["api:billing"]}))
        .await;
    assert_eq!(narrowed.status, StatusCode::OK, "{}", narrowed.body);
    assert_eq!(narrowed.body["areas"], json!(["api:billing"]));
    assert_eq!(narrowed.body["claim"]["areas"], json!(["api:search"]));
    assert_eq!(narrowed.body["handed_over"][0]["area"], "api:billing");
    assert_eq!(narrowed.body["handed_over"][0]["holder"], "bob");
    assert_eq!(narrowed.body["handed_over"][0]["to"]["id"], team.feature);
    let edit = team.check("src/billing/invoice.rs").await;
    assert_eq!(edit["verdict"], "allowed", "{edit}");
    let kinds = team.event_kinds().await;
    let tail: Vec<&str> = kinds
        .iter()
        .map(String::as_str)
        .filter(|kind| {
            matches!(
                *kind,
                "yield_requested" | "claim_narrowed" | "area_handed_over"
            )
        })
        .collect();
    assert_eq!(
        tail,
        ["yield_requested", "claim_narrowed", "area_handed_over"]
    );

    let again = team
        .narrow(&team.alice, team.debt, json!({"areas": ["api:billing"]}))
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert_eq!(again.body["areas"], json!([]));
    assert_eq!(again.body["handed_over"], json!([]));
    assert_eq!(team.event_kinds().await.len(), kinds.len());
}

#[tokio::test]
async fn the_sweep_escalates_a_request_nobody_answered_for_thirty_minutes() {
    let team = Team::new().await;
    team.bob_claims(json!(["api:billing"])).await;
    team.world.fakes.clock.advance(Duration::minutes(29));
    let early = team.world.app.tick_handovers.execute().await.unwrap();
    assert_eq!(early.escalated, 0);
    team.world.fakes.clock.advance(Duration::minutes(1));
    let due = team.world.app.tick_handovers.execute().await.unwrap();
    assert_eq!(due.escalated, 1);
    let status = team.world.get(&team.alice, "/v1/status").await;
    let escalations = status.body["escalations"].as_array().unwrap();
    assert_eq!(escalations.len(), 1, "{}", status.body);
    assert_eq!(escalations[0]["by"], "bob");
    assert_eq!(escalations[0]["arbiters"], json!(["carol"]));
}

#[tokio::test]
async fn a_renewal_naming_another_kind_is_refused_and_a_takeover_names_its_own() {
    let team = Team::new().await;
    let renewed = team
        .claim(
            &team.alice,
            team.alice_session,
            team.debt,
            json!({"kind": "feature"}),
        )
        .await;
    assert_error(&renewed, StatusCode::CONFLICT, "kind_fixed");
    assert_eq!(renewed.body["error"]["details"]["kind"], "debt");
    team.world.fakes.clock.advance(Duration::hours(2));
    let taken = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{}/takeover", team.debt),
            json!({"session_id": team.bob_session, "reason": "Finishing it as a feature.",
                   "kind": "feature"}),
        )
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    assert_eq!(taken.body["claim"]["kind"], "feature");
}

#[tokio::test]
async fn an_area_another_claim_waits_for_is_refused_saying_who_waits() {
    let team = Team::new().await;
    team.bob_claims(json!(["api:billing"])).await;
    let carol = team.world.member(&team.alice, "dave", Role::Member).await;
    let dave_session = team.world.session(&carol, "tower").await;
    let daves = team
        .world
        .item(&carol, Some("gh:acme/api#50"), "Refunds")
        .await;
    let refused = team
        .claim(
            &carol,
            dave_session,
            daves,
            json!({"areas": ["api:billing"]}),
        )
        .await;
    assert_error(&refused, StatusCode::CONFLICT, "area_held");
    let details = &refused.body["error"]["details"];
    assert_eq!(details["holder"], "bob");
    assert_eq!(details["item"], team.feature);
    assert_eq!(details["waiting"], true);
    assert_eq!(details["own"], false);
    assert_eq!(
        refused.body["error"]["message"],
        format!(
            "The area api:billing is waited for by another member's claim #{}.",
            team.feature
        )
    );
    let check = team
        .world
        .post(
            &carol,
            "/v1/edit-check",
            json!({"session_id": dave_session, "repo": "api",
                   "paths": ["src/billing/a.rs"], "reserve": false}),
        )
        .await;
    let verdict = &check.body["verdicts"][0];
    assert_eq!(verdict["reason"], "ask_holder", "{verdict}");
    assert_eq!(verdict["waiting"]["holder"], "bob");
    assert_eq!(verdict["waiting"]["item"], team.feature);
    let other = team
        .world
        .item(&team.bob, Some("gh:acme/api#43"), "Tax")
        .await;
    let own = team
        .claim(
            &team.bob,
            team.bob_session,
            other,
            json!({"areas": ["api:billing"]}),
        )
        .await;
    assert_error(&own, StatusCode::CONFLICT, "area_held");
    assert_eq!(own.body["error"]["details"]["own"], true);
    assert!(own.body["error"]["details"].get("holder").is_none());
    assert_eq!(
        own.body["error"]["message"],
        format!(
            "Your claim #{} already waits for api:billing to be handed over.",
            team.feature
        )
    );
}

#[tokio::test]
async fn a_waiting_edit_says_when_the_request_goes_to_the_arbiters_under_the_projects_setting() {
    let team = Team::new().await;
    let mut settings = two_repositories();
    settings["arbiters"] = json!(["carol"]);
    settings["yield_escalates_after"] = json!("45m");
    team.world.settings(&team.alice, settings).await;
    let me = team.world.get(&team.bob, "/v1/me").await;
    assert_eq!(me.status, StatusCode::OK);
    team.bob_claims(json!(["api:billing"])).await;
    let waiting = team.check("src/billing/invoice.rs").await;
    assert_eq!(waiting["reason"], "awaiting_handover");
    let asked = dibs_app::ports::Clock::now(&*team.world.fakes.clock);
    assert_eq!(
        waiting["escalates_at"],
        (asked + Duration::minutes(45))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    );
    team.world.fakes.clock.advance(Duration::minutes(44));
    assert_eq!(
        team.world
            .app
            .tick_handovers
            .execute()
            .await
            .unwrap()
            .escalated,
        0
    );
    team.world.fakes.clock.advance(Duration::minutes(1));
    assert_eq!(
        team.world
            .app
            .tick_handovers
            .execute()
            .await
            .unwrap()
            .escalated,
        1
    );
    let invalid = team
        .world
        .put(
            &team.alice,
            "/v1/admin/settings",
            json!({"yield_escalates_after": "1m"}),
        )
        .await;
    assert_eq!(
        invalid.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        invalid.body
    );
}
