//! `/v1/edit-check` and `/v1/grants`: verdicts per path, and grants that change them.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Answer, Person, World, assert_error, two_repositories};

struct Team {
    world: World,
    alice: Person,
    bob: Person,
    alice_session: u64,
    bob_session: u64,
    alice_item: u64,
    bob_item: u64,
}

impl Team {
    /// alice holds `api:billing` under her item; bob has a live claim on his item with no area.
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        world.settings(&alice, two_repositories()).await;
        let alice_session = world.session(&alice, "laptop").await;
        let bob_session = world.session(&bob, "desk").await;
        let alice_item = world.item(&alice, None, "Billing").await;
        let bob_item = world.item(&bob, None, "Search").await;
        for (who, session, item, areas) in [
            (&alice, alice_session, alice_item, json!(["api:billing"])),
            (&bob, bob_session, bob_item, json!([])),
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
            alice_session,
            bob_session,
            alice_item,
            bob_item,
        }
    }

    async fn check(&self, who: &Person, session: u64, body: Value) -> Answer {
        let mut body = body;
        body["session_id"] = json!(session);
        self.world.post(who, "/v1/edit-check", body).await
    }
}

#[tokio::test]
async fn each_path_gets_its_verdict_in_order() {
    let team = Team::new().await;
    let answer = team
        .check(
            &team.bob,
            team.bob_session,
            json!({"repo": "api", "paths": ["src/billing/invoice.rs", "src/search/index.rs", "README.md", "Cargo.lock"]}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(
        answer.body["verdicts"],
        json!([
            {"repo": "api", "path": "src/billing/invoice.rs", "verdict": "held_by",
             "holder": "alice", "item": team.alice_item, "item_ref": null, "area": "api:billing",
             "reason": "ask_holder"},
            {"repo": "api", "path": "src/search/index.rs", "verdict": "allowed"},
            {"repo": "api", "path": "README.md", "verdict": "allowed"},
            {"repo": "api", "path": "Cargo.lock", "verdict": "allowed"}
        ])
    );
}

#[tokio::test]
async fn a_shared_file_names_who_else_edited_it() {
    let team = Team::new().await;
    let lock = json!({"repo": "api", "paths": ["Cargo.lock"], "reserve": true});
    let first = team
        .check(&team.alice, team.alice_session, lock.clone())
        .await;
    assert_eq!(first.body["verdicts"][0]["verdict"], "allowed");
    let second = team.check(&team.bob, team.bob_session, lock).await;
    assert_eq!(
        second.body["verdicts"][0],
        json!({"repo": "api", "path": "Cargo.lock", "verdict": "allowed_with_note",
               "rule": "warn-shared", "note": "merge with care",
               "touched_by": [{"holder": "alice", "item": team.alice_item, "item_ref": null}]})
    );
}

#[tokio::test]
async fn a_member_without_a_live_claim_needs_one() {
    let team = Team::new().await;
    let carol = team.world.member(&team.alice, "carol", Role::Member).await;
    let session = team.world.session(&carol, "tablet").await;
    let answer = team
        .check(
            &carol,
            session,
            json!({"repo": "web", "paths": ["app/checkout/cart.ts"]}),
        )
        .await;
    assert_eq!(answer.body["verdicts"][0]["verdict"], "needs_claim");
}

#[tokio::test]
async fn reserving_adds_the_area_to_the_claim() {
    let team = Team::new().await;
    let path = json!({"repo": "api", "paths": ["src/search/index.rs"], "reserve": true});
    let answer = team.check(&team.bob, team.bob_session, path).await;
    assert_eq!(answer.body["verdicts"][0]["verdict"], "allowed");
    let shown = team
        .world
        .get(&team.bob, &format!("/v1/items/{}", team.bob_item))
        .await;
    assert_eq!(shown.body["claim"]["areas"], json!(["api:search"]));
    let asked = team
        .check(
            &team.alice,
            team.alice_session,
            json!({"repo": "api", "paths": ["src/search/x.rs"]}),
        )
        .await;
    assert_eq!(asked.body["verdicts"][0]["verdict"], "held_by");
    assert_eq!(asked.body["verdicts"][0]["holder"], "bob");
}

#[tokio::test]
async fn a_check_without_reserve_writes_nothing() {
    let team = Team::new().await;
    team.check(
        &team.bob,
        team.bob_session,
        json!({"repo": "api", "paths": ["src/search/index.rs"]}),
    )
    .await;
    let shown = team
        .world
        .get(&team.bob, &format!("/v1/items/{}", team.bob_item))
        .await;
    assert_eq!(shown.body["claim"]["areas"], json!([]));
}

#[tokio::test]
async fn a_bad_check_is_refused() {
    let team = Team::new().await;
    let cases = [
        (
            json!({"paths": ["a.rs"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"repo": "api"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"repo": "api", "paths": ["/etc/passwd"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"repo": "api", "paths": ["src\\a.rs"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"repo": "API", "paths": ["a.rs"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            json!({"repo": "search", "paths": ["a.rs"]}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_repo",
        ),
    ];
    for (body, status, code) in cases {
        let answer = team.check(&team.bob, team.bob_session, body.clone()).await;
        assert_error(&answer, status, code);
    }
    let unknown = team
        .check(
            &team.bob,
            team.bob_session,
            json!({"repo": "search", "paths": ["a.rs"]}),
        )
        .await;
    assert_eq!(unknown.body["error"]["details"], json!({"repo": "search"}));
    let not_mine = team
        .check(
            &team.bob,
            team.alice_session,
            json!({"repo": "api", "paths": ["a.rs"]}),
        )
        .await;
    assert_error(&not_mine, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn a_grant_turns_held_into_granted_until_revoked() {
    let team = Team::new().await;
    let body = json!({"item": team.bob_item, "area": "api:billing", "to": "bob"});
    let given = team
        .world
        .post(&team.alice, "/v1/grants", body.clone())
        .await;
    assert_eq!(given.status, StatusCode::CREATED, "{}", given.body);
    assert_eq!(given.body["created"], true);
    assert_eq!(given.body["grant"]["from"], "alice");
    assert_eq!(given.body["grant"]["to"], "bob");
    assert_eq!(given.body["grant"]["area"], "api:billing");
    assert_eq!(given.body["grant"]["item"], team.bob_item);
    assert_eq!(given.body["item"]["id"], team.bob_item);
    assert_eq!(given.body["item"]["title"], "Search");
    let again = team.world.post(&team.alice, "/v1/grants", body).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["created"], false);
    assert_eq!(again.body["grant"]["id"], given.body["grant"]["id"]);
    let path = json!({"repo": "api", "paths": ["src/billing/invoice.rs"]});
    let granted = team.check(&team.bob, team.bob_session, path.clone()).await;
    assert_eq!(
        granted.body["verdicts"][0],
        json!({"repo": "api", "path": "src/billing/invoice.rs", "verdict": "granted",
               "by": "alice", "area": "api:billing"})
    );
    let id = given.body["grant"]["id"].as_u64().unwrap();
    let grant_path = format!("/v1/grants/{id}");
    let not_grantor = team
        .world
        .call(Method::DELETE, &grant_path, Some(&team.bob.token), None)
        .await;
    assert_error(&not_grantor, StatusCode::FORBIDDEN, "not_grantor");
    let revoked = team
        .world
        .call(Method::DELETE, &grant_path, Some(&team.alice.token), None)
        .await;
    assert_eq!(revoked.status, StatusCode::OK);
    assert_eq!(revoked.body["grant"]["id"], id);
    let held = team.check(&team.bob, team.bob_session, path).await;
    assert_eq!(held.body["verdicts"][0]["verdict"], "held_by");
    let gone = team
        .world
        .call(Method::DELETE, &grant_path, Some(&team.alice.token), None)
        .await;
    assert_error(&gone, StatusCode::NOT_FOUND, "not_found");
    let bad = team
        .world
        .call(
            Method::DELETE,
            "/v1/grants/x",
            Some(&team.alice.token),
            None,
        )
        .await;
    assert_error(&bad, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn a_grant_breaking_its_rules_is_refused() {
    let team = Team::new().await;
    let cases = [
        (
            &team.alice,
            json!({"item": team.alice_item, "area": "api:billing", "to": "alice"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "self_grant",
        ),
        (
            &team.bob,
            json!({"item": team.bob_item, "area": "api:billing", "to": "alice"}),
            StatusCode::FORBIDDEN,
            "not_holder",
        ),
        (
            &team.alice,
            json!({"item": team.alice_item, "area": "api:billing", "to": "bob"}),
            StatusCode::CONFLICT,
            "grantee_has_no_claim",
        ),
        (
            &team.alice,
            json!({"item": team.bob_item, "area": "api:billing", "to": "nobody"}),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            &team.alice,
            json!({"item": 4242, "area": "api:billing", "to": "bob"}),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            &team.alice,
            json!({"item": team.bob_item, "area": "api:nothing", "to": "bob"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown_area",
        ),
        (
            &team.alice,
            json!({"item": team.bob_item, "to": "bob"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
    ];
    for (who, body, status, code) in cases {
        let answer = team.world.post(who, "/v1/grants", body).await;
        assert_error(&answer, status, code);
    }
}
