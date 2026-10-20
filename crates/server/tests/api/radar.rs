//! The radar through the router: a member's checkout says which branch their claim works on, an
//! integration lists the branches and reports what each changes, the overlap that makes is told to
//! both holders and shown in the status, a merge moves the base under the other branch, and every
//! refusal.

use axum::http::StatusCode;
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error};

/// Two repositories: `api` with `billing` and `search`, where search depends on billing and
/// `schema/**` is a published contract that all of `web` consumes; `web` with `checkout`.
fn settings() -> Value {
    json!({
        "repos": [
            {
                "id": "api",
                "remote": "https://github.com/acme/api.git",
                "areas": [{"name": "billing", "paths": ["src/billing/**"]},
                          {"name": "search", "paths": ["src/search/**"]}],
                "contracts": [{"paths": ["schema/**"], "consumers": ["web:*"]}],
                "area_deps": {"search": ["billing"]}
            },
            {
                "id": "web",
                "remote": "git@github.com:acme/web.git",
                "areas": [{"name": "checkout", "paths": ["app/checkout/**"]}]
            }
        ]
    })
}

/// alice (admin) and bob each claim an item from their own session; an integration watches.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    worker: Person,
    alice_session: u64,
    bob_session: u64,
    alices: u64,
    bobs: u64,
}

impl Team {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let worker = world.member(&alice, "worker", Role::Importer).await;
        world.settings(&alice, settings()).await;
        let alice_session = world.session(&alice, "laptop").await;
        let bob_session = world.session(&bob, "desk").await;
        let alices = world.item(&alice, Some("gh:acme/api#1"), "Rates").await;
        let bobs = world.item(&bob, Some("gh:acme/api#2"), "Refunds").await;
        for (who, session, item) in [(&alice, alice_session, alices), (&bob, bob_session, bobs)] {
            let claimed = world
                .post(
                    who,
                    &format!("/v1/items/{item}/claim"),
                    json!({"session_id": session}),
                )
                .await;
            assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
        }
        Self {
            world,
            alice,
            bob,
            worker,
            alice_session,
            bob_session,
            alices,
            bobs,
        }
    }

    async fn footprint(&self, item: u64, body: Value) -> crate::support::Answer {
        self.world
            .put(&self.worker, &format!("/v1/items/{item}/footprint"), body)
            .await
    }

    /// The subjects of the threads `who` reads.
    async fn subjects(&self, who: &Person) -> Vec<String> {
        let threads = self.world.get(who, "/v1/threads").await;
        threads.body["threads"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|thread| thread["thread"]["subject"].as_str().map(str::to_owned))
            .collect()
    }
}

#[tokio::test]
async fn an_edit_and_a_heartbeat_record_the_branch_and_the_integration_reads_it() {
    let team = Team::new().await;
    let edit = team
        .world
        .post(
            &team.bob,
            "/v1/edit-check",
            json!({"session_id": team.bob_session, "repo": "api", "paths": ["src/billing/a.rs"],
                   "reserve": true, "branch": "bob/2-refunds"}),
        )
        .await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
    let beat = team
        .world
        .post(
            &team.alice,
            "/v1/heartbeat",
            json!({"session_id": team.alice_session, "repo": "web", "branch": "alice/1-rates"}),
        )
        .await;
    assert_eq!(beat.status, StatusCode::OK, "{}", beat.body);
    let status = team.world.get(&team.alice, "/v1/status").await;
    let branches: Vec<Value> = status.body["live"]
        .as_array()
        .unwrap()
        .iter()
        .map(|held| held["claim"]["branches"].clone())
        .collect();
    assert_eq!(
        branches,
        [
            json!({"web": "alice/1-rates"}),
            json!({"api": "bob/2-refunds"})
        ]
    );
    let review = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{}/review", team.bobs),
            json!({"pr": "acme/api#9"}),
        )
        .await;
    assert_eq!(review.status, StatusCode::OK, "{}", review.body);
    let watched = team.world.get(&team.worker, "/v1/review-claims").await;
    assert_eq!(
        watched.body["claims"][0]["branches"],
        json!({"api": "bob/2-refunds"}),
        "{}",
        watched.body
    );

    for (body, field) in [
        (
            json!({"session_id": team.bob_session, "branch": "bob/x"}),
            "repo",
        ),
        (
            json!({"session_id": team.bob_session, "repo": "api", "branch": "bad name"}),
            "branch",
        ),
        (
            json!({"session_id": team.bob_session, "repo": "Api", "branch": "bob/x"}),
            "repo",
        ),
        (
            json!({"session_id": team.bob_session, "repo": "Bad!"}),
            "repo",
        ),
    ] {
        let refused = team.world.post(&team.bob, "/v1/heartbeat", body).await;
        assert_error(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        );
        assert!(refused.body.to_string().contains(field), "{}", refused.body);
    }
    let refused = team
        .world
        .post(
            &team.bob,
            "/v1/edit-check",
            json!({"session_id": team.bob_session, "repo": "api", "paths": ["a"],
                   "branch": "a..b"}),
        )
        .await;
    assert_error(
        &refused,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
}

#[tokio::test]
async fn one_shared_file_reported_through_the_footprint_route_tells_both_holders_once() {
    let team = Team::new().await;
    for (who, session, branch) in [
        (&team.alice, team.alice_session, "alice/1-rates"),
        (&team.bob, team.bob_session, "bob/2-refunds"),
    ] {
        team.world
            .post(
                who,
                "/v1/heartbeat",
                json!({"session_id": session, "repo": "api", "branch": branch}),
            )
            .await;
    }
    let first = team
        .footprint(
            team.alices,
            json!({"repo": "acme/api", "branch": "alice/1-rates", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "ABCD1234",
                   "paths": ["config/app.yaml", "src/billing/rates.rs"]}),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body, json!({"stored": true, "changed": true}));
    let body = json!({"repo": "api", "branch": "bob/2-refunds", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd1234",
                      "paths": ["config/app.yaml"], "truncated": true});
    assert_eq!(
        team.footprint(team.bobs, body.clone()).await.body["changed"],
        true
    );
    let again = team.footprint(team.bobs, body).await;
    assert_eq!(again.body, json!({"stored": true, "changed": false}));
    let subject = "Overlap: gh:acme/api#1 and gh:acme/api#2 change the same files";
    for who in [&team.alice, &team.bob] {
        let subjects = team.subjects(who).await;
        assert_eq!(
            subjects.iter().filter(|s| *s == subject).count(),
            1,
            "{subjects:?}"
        );
    }
    let status = team.world.get(&team.bob, "/v1/status").await;
    assert_eq!(
        status.body["overlaps"],
        json!([{
            "kind": "same_file",
            "items": [
                {"id": team.alices, "ref": "gh:acme/api#1", "title": "Rates",
                 "link": "https://github.com/acme/api/issues/1", "holder": "alice"},
                {"id": team.bobs, "ref": "gh:acme/api#2", "title": "Refunds",
                 "link": "https://github.com/acme/api/issues/2", "holder": "bob"}
            ],
            "pr": null,
            "paths": ["api:config/app.yaml"]
        }]),
        "{}",
        status.body
    );
    let events = team.world.get(&team.worker, "/v1/events").await;
    let found: Vec<&Value> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "overlap_found")
        .collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["payload"]["paths"], json!(["api:config/app.yaml"]));

    let merged = team
        .world
        .post(
            &team.worker,
            "/v1/prs/merged",
            json!({"pr": "acme/api#40", "base": "main", "merged_at": "2099-01-01T00:00:00Z",
                   "files": ["src/billing/rates.rs", "config/app.yaml"]}),
        )
        .await;
    assert_eq!(merged.body, json!({"matched": false}));
    let status = team.world.get(&team.bob, "/v1/status").await;
    let kinds: Vec<&str> = status.body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|overlap| overlap["kind"].as_str())
        .collect();
    assert_eq!(
        kinds,
        ["same_file", "main_moved", "main_moved"],
        "a merge nobody recorded moves the base under both branches: {}",
        status.body
    );
}

#[tokio::test]
async fn a_footprint_is_not_stored_without_a_live_claim_on_its_branch_and_is_refused_when_malformed()
 {
    let team = Team::new().await;
    let ok = json!({"repo": "api", "branch": "bob/2", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd", "paths": ["a.rs"]});
    assert_eq!(
        team.footprint(team.bobs, ok.clone()).await.body,
        json!({"stored": false, "reason": "other_branch"})
    );
    let unclaimed = team.world.item(&team.alice, None, "Nobody's").await;
    assert_eq!(
        team.footprint(unclaimed, ok.clone()).await.body,
        json!({"stored": false, "reason": "no_live_claim"})
    );
    assert_eq!(
        team.footprint(999, ok.clone()).await.body,
        json!({"stored": false, "reason": "no_live_claim"})
    );
    let unlisted = team
        .footprint(
            team.bobs,
            json!({"repo": "acme/elsewhere", "branch": "bob/2", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd", "paths": []}),
        )
        .await;
    assert_error(&unlisted, StatusCode::NOT_FOUND, "not_found");
    let too_many: Vec<String> = (0..3001).map(|n| format!("f{n}.rs")).collect();
    for body in [
        json!({"branch": "bob/2", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd", "paths": []}),
        json!({"repo": "api", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd", "paths": []}),
        json!({"repo": "api", "branch": "bob/2", "paths": []}),
        json!({"repo": "api", "branch": "bob/2", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd"}),
        json!({"repo": "api", "branch": "bob/2", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "xyz!", "paths": []}),
        json!({"repo": "api", "branch": "bob/2", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd", "paths": ["/abs"]}),
        json!({"repo": "api", "branch": "bob/2", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd", "paths": too_many}),
        json!({"repo": "api", "branch": "bob/2", "base_sha": "abcd", "base": "main", "paths": []}),
        json!({"repo": "api", "branch": "bob/2", "base_sha": "abcd", "base_at": "2026-03-01T00:00:00Z",
               "paths": []}),
        json!({"repo": "api", "branch": "bob/2", "base_sha": "abcd", "base": "main",
               "base_at": "yesterday", "paths": []}),
        json!({"repo": "api", "branch": "bob/2", "base_sha": "abcd", "base": "a..b",
               "base_at": "2026-03-01T00:00:00Z", "paths": []}),
    ] {
        assert_error(
            &team.footprint(team.bobs, body).await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        );
    }
    assert_error(
        &team
            .world
            .put(&team.bob, &format!("/v1/items/{}/footprint", team.bobs), ok)
            .await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
}

#[tokio::test]
async fn a_contract_changed_in_one_repository_meets_its_consumer_in_the_other() {
    let team = Team::new().await;
    for (who, session, repo, branch) in [
        (&team.alice, team.alice_session, "api", "alice/1"),
        (&team.bob, team.bob_session, "web", "bob/2"),
    ] {
        team.world
            .post(
                who,
                "/v1/heartbeat",
                json!({"session_id": session, "repo": repo, "branch": branch}),
            )
            .await;
    }
    team.footprint(
        team.bobs,
        json!({"repo": "web", "branch": "bob/2", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd", "paths": ["app/checkout/pay.ts"]}),
    )
    .await;
    team.footprint(
        team.alices,
        json!({"repo": "api", "branch": "alice/1", "base": "main", "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd", "paths": ["schema/invoice.json"]}),
    )
    .await;
    let status = team.world.get(&team.alice, "/v1/status").await;
    assert_eq!(
        status.body["overlaps"][0]["kind"], "contract",
        "{}",
        status.body
    );
    assert_eq!(
        status.body["overlaps"][0]["paths"],
        json!(["api:schema/invoice.json"])
    );
    let subjects = team.subjects(&team.bob).await;
    assert!(
        subjects.contains(
            &"Overlap: gh:acme/api#1 and gh:acme/api#2 meet at a published contract".to_owned()
        ),
        "{subjects:?}"
    );
}

#[tokio::test]
async fn two_sessions_of_one_machine_keep_the_branch_of_their_own_claims() {
    let team = Team::new().await;
    let registered = team
        .world
        .post(
            &team.bob,
            "/v1/sessions",
            json!({"host": "desk", "label": "desk/second", "agent": "agent"}),
        )
        .await;
    let second = registered.body["session_id"].as_u64().unwrap();
    assert_ne!(
        second, team.bob_session,
        "a second session on the same machine"
    );
    let other_item = team
        .world
        .item(&team.bob, Some("gh:acme/api#3"), "Search")
        .await;
    let claimed = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{other_item}/claim"),
            json!({"session_id": second}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    for _ in 0..2 {
        for (session, branch) in [(team.bob_session, "bob/2-refunds"), (second, "main")] {
            let beat = team
                .world
                .post(
                    &team.bob,
                    "/v1/heartbeat",
                    json!({"session_id": session, "repo": "api", "branch": branch}),
                )
                .await;
            assert_eq!(beat.status, StatusCode::OK, "{}", beat.body);
        }
    }
    let listed = team.world.get(&team.worker, "/v1/claims").await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let claims = listed.body["claims"].as_array().unwrap();
    let of = |item: u64| {
        claims
            .iter()
            .find(|claim| claim["item"] == item)
            .map(|claim| claim["branches"].clone())
    };
    assert_eq!(of(team.bobs), Some(json!({"api": "bob/2-refunds"})));
    assert_eq!(of(other_item), Some(json!({"api": "main"})));
    let events = team.world.get(&team.worker, "/v1/events").await;
    let recorded = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "branch_recorded")
        .count();
    assert_eq!(recorded, 2, "each claim's branch once, never flipped");
}

#[tokio::test]
async fn every_role_lists_the_live_claims_with_their_state_and_branches_and_nothing_else() {
    let team = Team::new().await;
    team.world
        .post(
            &team.bob,
            "/v1/heartbeat",
            json!({"session_id": team.bob_session, "repo": "api", "branch": "bob/2"}),
        )
        .await;
    team.world
        .post(
            &team.bob,
            &format!("/v1/items/{}/review", team.bobs),
            json!({"pr": "acme/api#9"}),
        )
        .await;
    for who in [&team.worker, &team.bob, &team.alice] {
        let listed = team.world.get(who, "/v1/claims").await;
        assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
        assert_eq!(
            listed.body,
            json!({"claims": [
                {"item": team.alices, "holder": "alice", "state": "working", "branches": {}},
                {"item": team.bobs, "holder": "bob", "state": "in_review",
                 "branches": {"api": "bob/2"}}
            ]})
        );
    }
    team.world.fakes.clock.advance(time::Duration::hours(25));
    let later = team.world.get(&team.worker, "/v1/claims").await;
    assert_eq!(
        later.body,
        json!({"claims": [
            {"item": team.bobs, "holder": "bob", "state": "in_review", "branches": {"api": "bob/2"}}
        ]}),
        "a stale claim is left out; a claim in review never goes stale"
    );
}
