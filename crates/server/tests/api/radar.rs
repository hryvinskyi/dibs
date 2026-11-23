//! The radar through the router: a member's checkout says which branch their claim works on when
//! nothing else works there, an integration lists the branches and reports what each changes, the
//! overlap that makes is told to each holder in their own claim's conversation and shown in the
//! status, a merge moves the base under the other branch, told at most once a day, and every
//! refusal.

use axum::http::StatusCode;
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error, checkout_of};

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

    /// Every message `who` can read that opens with `Overlap:`, `Base moved:` or `The order
    /// changed:`, with the subject of its conversation, by conversation and then oldest first.
    async fn told(&self, who: &Person) -> Vec<(String, String)> {
        let threads = self.world.get(who, "/v1/threads").await;
        let mut ids: Vec<u64> = threads.body["threads"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|thread| thread["thread"]["id"].as_u64())
            .collect();
        ids.sort_unstable();
        let mut told = Vec::new();
        for id in ids {
            let thread = self.world.get(who, &format!("/v1/threads/{id}")).await;
            let subject = thread.body["thread"]["subject"]
                .as_str()
                .unwrap()
                .to_owned();
            for message in thread.body["messages"].as_array().unwrap() {
                let body = message["body"].as_str().unwrap();
                if body.starts_with("Overlap: ")
                    || body.starts_with("Base moved: ")
                    || body.starts_with("The order changed: ")
                {
                    told.push((subject.clone(), body.to_owned()));
                }
            }
        }
        told
    }
}

/// The fields of a JSON object, in order.
fn keys(object: &Value) -> Vec<&str> {
    object
        .as_object()
        .map(|fields| fields.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn an_edit_and_a_heartbeat_record_the_branch_and_the_integration_reads_it() {
    let team = Team::new().await;
    let edit = team
        .world
        .post(
            &team.bob,
            "/v1/edit-check",
            json!({"session_id": team.bob_session, "checkout": checkout_of(team.bob_session),
                   "repo": "api", "paths": ["src/billing/a.rs"], "reserve": true,
                   "branch": "bob/2-refunds"}),
        )
        .await;
    assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
    let beat = team
        .world
        .post(
            &team.alice,
            "/v1/heartbeat",
            json!({"session_id": team.alice_session, "checkout": checkout_of(team.alice_session),
                   "repo": "web", "branch": "alice/1-rates"}),
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
    let sources: Vec<Value> = status.body["live"]
        .as_array()
        .unwrap()
        .iter()
        .map(|held| held["claim"]["branch_sources"].clone())
        .collect();
    assert_eq!(
        sources,
        [json!({"web": "checkout"}), json!({"api": "checkout"})],
        "a claim says where each of its branches was learned from"
    );
    let listed = team.world.get(&team.worker, "/v1/claims").await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let claims = listed.body["claims"].as_array().unwrap();
    assert_eq!(claims.len(), 2, "{}", listed.body);
    for (claim, branches) in claims.iter().zip([
        json!({"web": "alice/1-rates"}),
        json!({"api": "bob/2-refunds"}),
    ]) {
        assert_eq!(claim["branches"], branches, "{}", listed.body);
        assert_eq!(
            keys(claim),
            ["branches", "holder", "item", "state"],
            "the integration's list keeps its shape: {}",
            listed.body
        );
    }
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
    assert_eq!(
        keys(&watched.body["claims"][0]),
        ["branches", "holder", "item", "more", "prs", "since"],
        "the claims in review keep their shape: {}",
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
        (
            json!({"session_id": team.bob_session, "checkout": "ABCDEF0123456789ABCDEF0123456789"}),
            "checkout",
        ),
        (
            json!({"session_id": team.bob_session, "checkout": "c0ffee"}),
            "checkout",
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
    let refused = team
        .world
        .post(
            &team.bob,
            "/v1/edit-check",
            json!({"session_id": team.bob_session, "repo": "api", "paths": ["a"],
                   "reserve": true, "checkout": "not a checkout"}),
        )
        .await;
    assert_error(
        &refused,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    assert!(
        refused.body.to_string().contains("checkout"),
        "{}",
        refused.body
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
                json!({"session_id": session, "checkout": checkout_of(session),
                       "repo": "api", "branch": branch}),
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
    for (who, subject, other) in [
        (
            &team.alice,
            "Your claim on gh:acme/api#1 \"Rates\"",
            "work item gh:acme/api#2 \"Refunds\" (bob, on bob/2-refunds)",
        ),
        (
            &team.bob,
            "Your claim on gh:acme/api#2 \"Refunds\"",
            "work item gh:acme/api#1 \"Rates\" (alice, on alice/1-rates)",
        ),
    ] {
        let told = team.told(who).await;
        assert_eq!(
            told.len(),
            1,
            "told once, in their own claim's conversation: {told:?}"
        );
        assert_eq!(told[0].0, subject);
        assert!(
            told[0].1.contains(other) && told[0].1.contains("change the same files"),
            "{told:?}"
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
            "paths": ["api:config/app.yaml"],
            "order": {"tied": true}
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

/// The branches of alice's and bob's claims, with what each checkout says.
const BRANCHES: [&str; 2] = ["alice/1-rates", "bob/2-refunds"];

/// Each checkout says its claim works on its own branch.
async fn on_branches(team: &Team) {
    for (who, session, branch) in [
        (&team.alice, team.alice_session, BRANCHES[0]),
        (&team.bob, team.bob_session, BRANCHES[1]),
    ] {
        team.world
            .post(
                who,
                "/v1/heartbeat",
                json!({"session_id": session, "checkout": checkout_of(session),
                       "repo": "api", "branch": branch}),
            )
            .await;
    }
}

/// Both claims' branches change `config/app.yaml`.
async fn share_a_file(team: &Team) {
    for (item, branch) in [(team.alices, BRANCHES[0]), (team.bobs, BRANCHES[1])] {
        let reported = team
            .footprint(
                item,
                json!({"repo": "api", "branch": branch, "base": "main",
                       "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd1234",
                       "paths": ["config/app.yaml"]}),
            )
            .await;
        assert_eq!(reported.status, StatusCode::OK, "{}", reported.body);
    }
}

/// The first line of the last message `who` was told about an overlap or its order.
async fn last_told(team: &Team, who: &Person) -> String {
    let told = team.told(who).await;
    told.last()
        .and_then(|(_, body)| body.lines().next())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn two_shared_footprints_tell_each_holder_which_claim_lands_first_and_the_status_shows_it() {
    let team = Team::new().await;
    on_branches(&team).await;
    let review = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{}/review", team.bobs),
            json!({"pr": "acme/api#9"}),
        )
        .await;
    assert_eq!(review.status, StatusCode::OK, "{}", review.body);
    share_a_file(&team).await;

    let bobs = last_told(&team, &team.bob).await;
    assert!(
        bobs.ends_with(
            "Yours lands first, because it is in review. Work item gh:acme/api#1 rebases onto it once it merges; go on with your work."
        ),
        "{bobs}"
    );
    let alices = last_told(&team, &team.alice).await;
    assert!(
        alices.ends_with(
            "Work item gh:acme/api#2 lands first, because it is in review. Once it merges, rebase onto the base and resolve these files, then say in this conversation that you rebased."
        ),
        "{alices}"
    );
    let status = team.world.get(&team.alice, "/v1/status").await;
    assert_eq!(
        status.body["overlaps"][0]["order"],
        json!({"first": team.bobs, "because": "in_review"}),
        "{}",
        status.body
    );
}

#[tokio::test]
async fn the_status_shows_an_escalation_dibs_raised_for_a_tie_as_raised_by_dibs() {
    let team = Team::new().await;
    let carol = team.world.member(&team.alice, "carol", Role::Member).await;
    let mut arbitrated = settings();
    arbitrated["arbiters"] = json!(["carol"]);
    team.world.settings(&team.alice, arbitrated).await;
    on_branches(&team).await;
    share_a_file(&team).await;

    let status = team.world.get(&carol, "/v1/status").await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.body);
    let escalation = &status.body["escalations"][0];
    assert_eq!(escalation["raised_by_dibs"], true, "{}", status.body);
    assert_eq!(
        escalation["by"], "alice",
        "the member it is recorded for stays, for older clients that require one"
    );
    assert_eq!(escalation["subject"], json!({"item": team.alices}));
    let events = team.world.get(&team.alice, "/v1/events").await;
    let escalated: Vec<&Value> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "escalated")
        .collect();
    assert_eq!(escalated.len(), 1, "{}", events.body);
    assert_eq!(
        escalated[0]["payload"],
        json!({"subject": {"item": team.alices}, "by": team.alice.actor.member.get(),
               "by_dibs": true})
    );
}

#[tokio::test]
async fn the_sweep_moves_the_order_when_a_claim_enters_review_and_tells_both_holders() {
    let team = Team::new().await;
    on_branches(&team).await;
    share_a_file(&team).await;
    let alices = last_told(&team, &team.alice).await;
    assert!(
        alices.ends_with(
            "The order rule cannot decide which lands first: both are at work, their work items have the same priority and their claims were made at the same time. No arbiter can be asked, so write to bob (dibs send bob) and agree on the order."
        ),
        "{alices}"
    );
    let status = team.world.get(&team.bob, "/v1/status").await;
    assert_eq!(status.body["overlaps"][0]["order"], json!({"tied": true}));

    let review = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{}/review", team.bobs),
            json!({"pr": "acme/api#9"}),
        )
        .await;
    assert_eq!(review.status, StatusCode::OK, "{}", review.body);
    let swept = team.world.app.tick_overlaps.execute().await.unwrap();
    assert_eq!(swept.reordered, 1);

    let status = team.world.get(&team.bob, "/v1/status").await;
    assert_eq!(
        status.body["overlaps"][0]["order"],
        json!({"first": team.bobs, "because": "in_review"}),
        "{}",
        status.body
    );
    assert_eq!(
        last_told(&team, &team.alice).await,
        "The order changed: work item gh:acme/api#2 now lands first, because it is in review. Once it merges, rebase onto the base and resolve the shared files (api:config/app.yaml), then say in this conversation that you rebased."
    );
    assert_eq!(
        last_told(&team, &team.bob).await,
        "The order changed: yours now lands first, because it is in review. Work item gh:acme/api#1 rebases onto it once it merges; go on with your work."
    );
    let again = team.world.app.tick_overlaps.execute().await.unwrap();
    assert_eq!(again.reordered, 0, "an order that holds is left");
}

#[tokio::test]
async fn the_log_records_a_collisions_first_order_and_each_change_of_who_lands_first() {
    let team = Team::new().await;
    on_branches(&team).await;
    share_a_file(&team).await;
    let review = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{}/review", team.bobs),
            json!({"pr": "acme/api#9"}),
        )
        .await;
    assert_eq!(review.status, StatusCode::OK, "{}", review.body);
    let swept = team.world.app.tick_overlaps.execute().await.unwrap();
    assert_eq!(swept.reordered, 1);

    let events = team.world.get(&team.alice, "/v1/events").await;
    assert_eq!(events.status, StatusCode::OK, "{}", events.body);
    let ordered: Vec<&Value> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "overlap_ordered")
        .map(|event| &event["payload"])
        .collect();
    assert_eq!(
        ordered,
        [
            &json!({"kind": "same_file", "items": [team.alices, team.bobs], "first": null,
                    "rule": "tied", "changed": false}),
            &json!({"kind": "same_file", "items": [team.alices, team.bobs],
                    "first": team.bobs, "rule": "in_review", "changed": true}),
        ],
        "{}",
        events.body
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
                json!({"session_id": session, "checkout": checkout_of(session),
                       "repo": repo, "branch": branch}),
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
    let told = team.told(&team.bob).await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].0, "Your claim on gh:acme/api#2 \"Refunds\"");
    assert!(
        told[0].1.starts_with("Overlap: ") && told[0].1.contains("meet at a published contract"),
        "{told:?}"
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
                    json!({"session_id": session, "checkout": checkout_of(session),
                       "repo": "api", "branch": branch}),
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
            json!({"session_id": team.bob_session, "checkout": checkout_of(team.bob_session),
                   "repo": "api", "branch": "bob/2"}),
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

#[tokio::test]
async fn two_sessions_of_a_member_in_one_checkout_record_no_branch_and_announce_nothing() {
    let team = Team::new().await;
    let second = team.world.session(&team.bob, "desk-two").await;
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
    let shared = checkout_of(team.bob_session);
    let detached = team
        .world
        .post(
            &team.bob,
            "/v1/heartbeat",
            json!({"session_id": second, "checkout": shared, "repo": "api"}),
        )
        .await;
    assert_eq!(detached.status, StatusCode::OK, "{}", detached.body);
    for _ in 0..2 {
        for (session, branch) in [
            (team.bob_session, "bob/2-refunds"),
            (second, "bob/3-search"),
        ] {
            let edit = team
                .world
                .post(
                    &team.bob,
                    "/v1/edit-check",
                    json!({"session_id": session, "checkout": shared, "repo": "api",
                           "paths": ["src/billing/a.rs"], "reserve": true, "branch": branch}),
                )
                .await;
            assert_eq!(edit.status, StatusCode::OK, "{}", edit.body);
            let beat = team
                .world
                .post(
                    &team.bob,
                    "/v1/heartbeat",
                    json!({"session_id": session, "checkout": shared, "repo": "api",
                           "branch": branch}),
                )
                .await;
            assert_eq!(beat.status, StatusCode::OK, "{}", beat.body);
        }
    }
    let listed = team.world.get(&team.worker, "/v1/claims").await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    for claim in listed.body["claims"].as_array().unwrap() {
        assert_eq!(claim["branches"], json!({}), "{}", listed.body);
    }
    let events = team.world.get(&team.worker, "/v1/events").await;
    let recorded = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "branch_recorded")
        .count();
    assert_eq!(recorded, 0, "{}", events.body);

    let ended = team
        .world
        .post(&team.bob, &format!("/v1/sessions/{second}/end"), json!({}))
        .await;
    assert_eq!(ended.status, StatusCode::OK, "{}", ended.body);
    let beat = team
        .world
        .post(
            &team.bob,
            "/v1/heartbeat",
            json!({"session_id": team.bob_session, "checkout": shared, "repo": "api",
                   "branch": "bob/2-refunds"}),
        )
        .await;
    assert_eq!(beat.status, StatusCode::OK, "{}", beat.body);
    let status = team.world.get(&team.bob, "/v1/status").await;
    let mine = status.body["live"]
        .as_array()
        .unwrap()
        .iter()
        .find(|held| held["claim"]["item"] == team.bobs)
        .map(|held| held["claim"].clone())
        .unwrap();
    assert_eq!(
        mine["branches"],
        json!({"api": "bob/2-refunds"}),
        "{}",
        status.body
    );
    assert_eq!(
        mine["branch_sources"],
        json!({"api": "checkout"}),
        "{}",
        status.body
    );
}

#[tokio::test]
async fn a_heartbeat_and_an_edit_that_name_no_checkout_record_no_branch() {
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
            &team.bob,
            "/v1/heartbeat",
            json!({"session_id": team.bob_session, "repo": "api", "branch": "bob/2-refunds"}),
        )
        .await;
    assert_eq!(beat.status, StatusCode::OK, "{}", beat.body);
    let listed = team.world.get(&team.worker, "/v1/claims").await;
    for claim in listed.body["claims"].as_array().unwrap() {
        assert_eq!(claim["branches"], json!({}), "{}", listed.body);
    }
}

#[tokio::test]
async fn two_merges_an_hour_apart_that_move_a_claims_base_are_told_once_and_both_shown() {
    let team = Team::new().await;
    team.world
        .post(
            &team.bob,
            "/v1/heartbeat",
            json!({"session_id": team.bob_session, "checkout": checkout_of(team.bob_session),
                   "repo": "api", "branch": "bob/2-refunds"}),
        )
        .await;
    let stored = team
        .footprint(
            team.bobs,
            json!({"repo": "api", "branch": "bob/2-refunds", "base": "main",
                   "base_at": "2026-03-01T00:00:00Z", "base_sha": "abcd1234",
                   "paths": ["config/app.yaml", "src/billing/rates.rs"]}),
        )
        .await;
    assert_eq!(stored.body["changed"], true, "{}", stored.body);
    for (pr, merged_at, file) in [
        ("acme/api#40", "2099-01-01T00:00:00Z", "config/app.yaml"),
        (
            "acme/api#41",
            "2099-01-01T01:00:00Z",
            "src/billing/rates.rs",
        ),
    ] {
        let merged = team
            .world
            .post(
                &team.worker,
                "/v1/prs/merged",
                json!({"pr": pr, "base": "main", "merged_at": merged_at, "files": [file]}),
            )
            .await;
        assert_eq!(merged.status, StatusCode::OK, "{}", merged.body);
        team.world.fakes.clock.advance(time::Duration::hours(1));
    }

    let told = team.told(&team.bob).await;
    assert_eq!(told.len(), 1, "one message within the day: {told:?}");
    assert_eq!(told[0].0, "Your claim on gh:acme/api#2 \"Refunds\"");
    assert!(
        told[0].1.starts_with(
            "Base moved: pull request acme/api#40 was merged into the base after your branch bob/2-refunds took its base"
        ),
        "{told:?}"
    );
    let status = team.world.get(&team.bob, "/v1/status").await;
    let moved: Vec<(&str, &str)> = status.body["overlaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|overlap| overlap["kind"] == "main_moved")
        .filter_map(|overlap| {
            Some((
                overlap["pr"].as_str()?,
                overlap["items"][0]["holder"].as_str()?,
            ))
        })
        .collect();
    assert_eq!(
        moved,
        [("acme/api#40", "bob"), ("acme/api#41", "bob")],
        "{}",
        status.body
    );
}
