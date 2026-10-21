//! Review through the router: a holder records pull requests on a claim, an integration reports
//! merges and closes and reads the claims in review and the pull-request status, the verdicts and
//! the status meanwhile, and every refusal.

use axum::http::StatusCode;
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error};

/// Two repositories, a protected main branch and a slot that freezes it; the status of `api`'s
/// pull requests is on and does not name the holder.
fn settings() -> Value {
    json!({
        "stale_after": "1h",
        "repos": [
            {
                "id": "api",
                "remote": "https://github.com/acme/api.git",
                "areas": [{"name": "billing", "paths": ["src/billing/**"]}],
                "protected_refs": [{"ref": "refs/heads/main", "pushers": ["alice"]}],
                "pr_status": {"enabled": true}
            },
            {
                "id": "web",
                "remote": "git@github.com:acme/web.git",
                "areas": [{"name": "checkout", "paths": ["app/checkout/**"]}]
            }
        ],
        "slots": [{"name": "db:migrate", "max_hold": "1h", "freezes": ["api:refs/heads/main"]}]
    })
}

/// alice (admin), bob (member) and an integration; bob holds `api:billing` on an item.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    worker: Person,
    alice_session: u64,
    item: u64,
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
        let item = world
            .item(&bob, Some("gh:acme/api#42"), "Rounding in invoices")
            .await;
        let claimed = world
            .post(
                &bob,
                &format!("/v1/items/{item}/claim"),
                json!({"session_id": bob_session, "areas": ["api:billing"]}),
            )
            .await;
        assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
        Self {
            world,
            alice,
            bob,
            worker,
            alice_session,
            item,
        }
    }

    async fn review(&self, who: &Person, pr: &str, more: bool) -> crate::support::Answer {
        self.world
            .post(
                who,
                &format!("/v1/items/{}/review", self.item),
                json!({"pr": pr, "more": more}),
            )
            .await
    }

    async fn merged(&self, pr: &str) -> crate::support::Answer {
        self.world
            .post(
                &self.worker,
                "/v1/prs/merged",
                json!({"pr": pr, "base": "main", "merged_at": "2026-03-02T10:00:00Z",
                       "files": ["src/billing/round.rs"]}),
            )
            .await
    }
}

#[tokio::test]
async fn the_holder_moves_their_claim_to_review_and_the_area_answers_free_when_it_merges() {
    let team = Team::new().await;
    let reviewed = team.review(&team.bob, "Acme/API#7", false).await;
    assert_eq!(reviewed.status, StatusCode::OK, "{}", reviewed.body);
    assert_eq!(reviewed.body["claim"]["state"], "in_review");
    assert_eq!(
        reviewed.body["claim"]["review"]["prs"],
        json!(["acme/api#7"])
    );
    assert_eq!(reviewed.body["claim"]["areas"], json!(["api:billing"]));
    assert_eq!(reviewed.body["item"]["ref"], "gh:acme/api#42");

    let check = team
        .world
        .post(
            &team.alice,
            "/v1/edit-check",
            json!({"session_id": team.alice_session, "repo": "api",
                   "paths": ["src/billing/round.rs"]}),
        )
        .await;
    assert_eq!(check.status, StatusCode::OK, "{}", check.body);
    let verdict = &check.body["verdicts"][0];
    assert_eq!(verdict["verdict"], "held_by");
    assert_eq!(verdict["holder"], "bob");
    assert_eq!(verdict["reason"], "in_review");
    assert_eq!(verdict["prs"], json!(["acme/api#7"]));

    let status = team.world.get(&team.alice, "/v1/status").await;
    assert_eq!(status.body["live"][0]["claim"]["state"], "in_review");
    assert_eq!(status.body["integration_silent_since"], Value::Null);
}

#[tokio::test]
async fn the_last_recorded_pull_request_merged_completes_the_claim_once() {
    let team = Team::new().await;
    team.review(&team.bob, "acme/api#7", false).await;
    let merged = team.merged("acme/api#7").await;
    assert_eq!(merged.status, StatusCode::OK, "{}", merged.body);
    assert_eq!(
        merged.body,
        json!({"matched": true, "item": team.item, "claim": "completed"})
    );
    let status = team.world.get(&team.alice, "/v1/status").await;
    assert_eq!(status.body["live"], json!([]));
    assert_eq!(status.body["holds"], json!([]), "the area is free");
    let threads = team.world.get(&team.bob, "/v1/threads").await;
    let subjects: Vec<&str> = threads.body["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|thread| thread["thread"]["subject"].as_str())
        .collect();
    assert_eq!(subjects, ["Done by merge: Rounding in invoices"]);
    let again = team.merged("acme/api#7").await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body, json!({"matched": false}));
    let events = team.world.get(&team.alice, "/v1/events").await;
    let kinds: Vec<&str> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"review_started"), "{kinds:?}");
    assert!(kinds.contains(&"pr_merged"), "{kinds:?}");
    assert!(kinds.contains(&"completed_by_merge"), "{kinds:?}");
}

#[tokio::test]
async fn a_merge_with_more_to_follow_and_a_close_return_the_claim_to_work() {
    let team = Team::new().await;
    team.review(&team.bob, "acme/api#7", true).await;
    team.review(&team.bob, "acme/web#3", true).await;
    let first = team.merged("acme/web#3").await;
    assert_eq!(first.body["claim"], "in_review", "{}", first.body);
    let second = team.merged("acme/api#7").await;
    assert_eq!(second.body["claim"], "working", "{}", second.body);
    team.review(&team.bob, "acme/api#8", false).await;
    let closed = team
        .world
        .post(&team.worker, "/v1/prs/closed", json!({"pr": "acme/api#8"}))
        .await;
    assert_eq!(
        closed.body,
        json!({"matched": true, "item": team.item, "claim": "working"})
    );
    let status = team.world.get(&team.bob, "/v1/status").await;
    assert_eq!(status.body["live"][0]["claim"]["state"], "working");
    assert_eq!(status.body["live"][0]["claim"]["review"], Value::Null);
    let unrecorded = team
        .world
        .post(
            &team.worker,
            "/v1/prs/closed",
            json!({"pr": "acme/api#999"}),
        )
        .await;
    assert_eq!(unrecorded.body, json!({"matched": false}));
}

#[tokio::test]
async fn an_integration_lists_the_claims_in_review_and_reads_the_pr_status() {
    let team = Team::new().await;
    team.review(&team.bob, "acme/api#7", false).await;
    let listed = team.world.get(&team.worker, "/v1/review-claims").await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["claims"][0]["holder"], "bob");
    assert_eq!(listed.body["claims"][0]["prs"], json!(["acme/api#7"]));
    assert_eq!(listed.body["claims"][0]["item"]["id"], team.item);

    let read = |repo: &'static str, base: &'static str| {
        let world = &team.world;
        let worker = &team.worker;
        async move {
            world
                .get(worker, &format!("/v1/pr-status?repo={repo}&base={base}"))
                .await
        }
    };
    let free = read("acme/api", "main").await;
    assert_eq!(
        free.body,
        json!({"enabled": true, "context": "dibs", "state": "success",
               "description": "Nothing freezes the base branch; it may be merged."})
    );
    let taken = team
        .world
        .post(
            &team.alice,
            "/v1/slots/db:migrate/take",
            json!({"session_id": team.alice_session, "purpose": "rename the invoices table"}),
        )
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    let frozen = read("api", "refs/heads/main").await;
    assert_eq!(frozen.body["state"], "failure", "{}", frozen.body);
    let description = frozen.body["description"].as_str().unwrap();
    assert!(
        description.starts_with("A freezing action is in progress until "),
        "{description}"
    );
    assert!(!description.contains("alice") && !description.contains("invoices"));
    let web = read("web", "main").await;
    assert_eq!(web.body["enabled"], false);
    assert_eq!(web.body["state"], "success");
    let released = team
        .world
        .post(&team.alice, "/v1/slots/db:migrate/release", json!({}))
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.body);
    assert_eq!(read("api", "main").await.body["state"], "success");
    assert_error(
        &read("acme/elsewhere", "main").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let missing = team.world.get(&team.worker, "/v1/pr-status?repo=api").await;
    assert_error(
        &missing,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
}

#[tokio::test]
async fn each_refusal_says_what_is_wrong() {
    let team = Team::new().await;
    assert_error(
        &team.review(&team.alice, "acme/api#7", false).await,
        StatusCode::FORBIDDEN,
        "not_holder",
    );
    assert_error(
        &team.review(&team.bob, "acme/elsewhere#7", false).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unlisted_repository",
    );
    assert_error(
        &team.review(&team.bob, "acme/api", false).await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    let other = team.world.item(&team.alice, None, "Another").await;
    team.world
        .post(
            &team.alice,
            &format!("/v1/items/{other}/claim"),
            json!({"session_id": team.alice_session}),
        )
        .await;
    team.review(&team.bob, "acme/api#7", false).await;
    let twice = team
        .world
        .post(
            &team.alice,
            &format!("/v1/items/{other}/review"),
            json!({"pr": "acme/api#7"}),
        )
        .await;
    assert_error(&twice, StatusCode::CONFLICT, "pr_recorded");
    assert_eq!(twice.body["error"]["details"]["item"], team.item);
    let unmerged = team
        .world
        .post(
            &team.worker,
            "/v1/prs/merged",
            json!({"pr": "acme/api#7", "base": "main", "merged_at": "yesterday"}),
        )
        .await;
    assert_error(
        &unmerged,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    let too_many: Vec<String> = (0..3001).map(|n| format!("src/{n}.rs")).collect();
    let flood = team
        .world
        .post(
            &team.worker,
            "/v1/prs/merged",
            json!({"pr": "acme/api#7", "base": "main", "merged_at": "2026-03-02T10:00:00Z",
                   "files": too_many}),
        )
        .await;
    assert_error(&flood, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    for (path, body) in [
        ("/v1/prs/merged", json!({"pr": "acme/api#7"})),
        ("/v1/prs/closed", json!({"pr": "acme/api#7"})),
    ] {
        assert_error(
            &team.world.post(&team.bob, path, body).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    for path in ["/v1/review-claims", "/v1/pr-status?repo=api&base=main"] {
        assert_error(
            &team.world.get(&team.bob, path).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
}

#[tokio::test]
async fn the_status_says_since_when_the_integrations_are_silent_only_while_reviews_rely_on_them() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let worker = world.member(&alice, "worker", Role::Importer).await;
    world.get(&worker, "/v1/me").await;
    world.fakes.clock.advance(time::Duration::minutes(16));
    let unused = world.get(&alice, "/v1/status").await;
    assert_eq!(
        unused.body["integration_silent_since"],
        Value::Null,
        "nothing relies on the integration yet: {}",
        unused.body
    );
    world
        .settings(&alice, json!({"pr_status": {"enabled": true}}))
        .await;
    let silent = world.get(&alice, "/v1/status").await;
    assert_eq!(
        silent.body["integration_silent_since"], "2026-03-02T09:00:00Z",
        "{}",
        silent.body
    );
    world.get(&worker, "/v1/review-claims").await;
    let heard = world.get(&alice, "/v1/status").await;
    assert_eq!(heard.body["integration_silent_since"], Value::Null);
}

#[tokio::test]
async fn a_close_or_a_stacked_merge_beside_another_pull_request_sets_it_aside() {
    let team = Team::new().await;
    team.review(&team.bob, "acme/api#7", false).await;
    team.review(&team.bob, "acme/api#8", false).await;
    let stacked = team
        .world
        .post(
            &team.worker,
            "/v1/prs/merged",
            json!({"pr": "acme/api#8", "base": "bob/42-base", "merged_at": "2026-03-02T10:00:00Z"}),
        )
        .await;
    assert_eq!(
        stacked.body,
        json!({"matched": true, "item": team.item, "claim": "set_aside"})
    );
    let status = team.world.get(&team.bob, "/v1/status").await;
    let claim = &status.body["live"][0]["claim"];
    assert_eq!(claim["review"]["prs"], json!(["acme/api#7"]));
    assert_eq!(claim["review"]["more"], true);
    let merged = team.merged("acme/api#7").await;
    assert_eq!(
        merged.body["claim"], "working",
        "the remaining merge no longer completes the item: {}",
        merged.body
    );
    team.review(&team.bob, "acme/api#9", false).await;
    team.review(&team.bob, "acme/api#10", false).await;
    let closed = team
        .world
        .post(&team.worker, "/v1/prs/closed", json!({"pr": "acme/api#10"}))
        .await;
    assert_eq!(closed.body["claim"], "set_aside", "{}", closed.body);
    let threads = team.world.get(&team.bob, "/v1/threads").await;
    let told = threads.body["threads"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|thread| {
            thread["thread"]["subject"]
                .as_str()
                .is_some_and(|subject| subject.starts_with("Set aside in review: "))
        })
        .count();
    assert_eq!(told, 2, "the holder is told each time");
}

#[tokio::test]
async fn the_holder_drops_a_pull_request_and_a_refusal_names_the_claim_that_recorded_one() {
    let team = Team::new().await;
    team.review(&team.bob, "acme/api#7", false).await;
    let dropped = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{}/review/drop", team.item),
            json!({"pr": "acme/api#7"}),
        )
        .await;
    assert_eq!(dropped.status, StatusCode::OK, "{}", dropped.body);
    assert_eq!(dropped.body["claim"]["state"], "working");
    assert_eq!(dropped.body["claim"]["review"], Value::Null);
    let again = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{}/review/drop", team.item),
            json!({"pr": "acme/api#7"}),
        )
        .await;
    assert_error(&again, StatusCode::CONFLICT, "pr_not_recorded");
    assert_eq!(again.body["error"]["details"]["pr"], "acme/api#7");
    let theirs = team
        .world
        .post(
            &team.alice,
            &format!("/v1/items/{}/review/drop", team.item),
            json!({"pr": "acme/api#7"}),
        )
        .await;
    assert_error(&theirs, StatusCode::FORBIDDEN, "not_holder");

    team.review(&team.bob, "acme/api#7", false).await;
    let other = team
        .world
        .item(&team.alice, Some("gh:acme/api#43"), "Other")
        .await;
    team.world
        .post(
            &team.alice,
            &format!("/v1/items/{other}/claim"),
            json!({"session_id": team.alice_session}),
        )
        .await;
    let twice = team
        .world
        .post(
            &team.alice,
            &format!("/v1/items/{other}/review"),
            json!({"pr": "acme/api#7"}),
        )
        .await;
    assert_error(&twice, StatusCode::CONFLICT, "pr_recorded");
    let details = &twice.body["error"]["details"];
    assert_eq!(
        (&details["item"], &details["item_ref"], &details["holder"]),
        (&json!(team.item), &json!("gh:acme/api#42"), &json!("bob"))
    );
}

/// [`settings`] with `api`'s release branches protected beside `main` and a slot that freezes
/// every one of them.
fn release_train() -> Value {
    let mut train = settings();
    train["repos"][0]["protected_refs"] = json!([
        {"ref": "refs/heads/main", "pushers": ["alice"]},
        {"ref": "refs/heads/release/*", "pushers": ["alice"]}
    ]);
    train["slots"] = json!([
        {"name": "release:cut", "max_hold": "1h", "freezes": ["api:refs/heads/release/*"]}
    ]);
    train
}

#[tokio::test]
async fn a_merge_into_a_release_branch_a_pattern_protects_completes_the_claim() {
    let team = Team::new().await;
    team.world.settings(&team.alice, release_train()).await;
    team.review(&team.bob, "acme/api#7", false).await;
    let stacked = team
        .world
        .post(
            &team.worker,
            "/v1/prs/merged",
            json!({"pr": "acme/api#7", "base": "release", "merged_at": "2026-03-02T10:00:00Z"}),
        )
        .await;
    assert_eq!(stacked.status, StatusCode::OK, "{}", stacked.body);
    assert_eq!(
        stacked.body["claim"], "working",
        "a branch the pattern does not cover delivers nothing"
    );
    team.review(&team.bob, "acme/api#8", false).await;
    let merged = team
        .world
        .post(
            &team.worker,
            "/v1/prs/merged",
            json!({"pr": "acme/api#8", "base": "release/2026.10.3",
                   "merged_at": "2026-03-02T11:00:00Z"}),
        )
        .await;
    assert_eq!(merged.status, StatusCode::OK, "{}", merged.body);
    assert_eq!(
        merged.body,
        json!({"matched": true, "item": team.item, "claim": "completed"})
    );
}

#[tokio::test]
async fn the_pr_status_of_a_release_branch_fails_while_a_slot_that_freezes_its_pattern_is_held() {
    let team = Team::new().await;
    team.world.settings(&team.alice, release_train()).await;
    let read = |base: &'static str| {
        let world = &team.world;
        let worker = &team.worker;
        async move {
            world
                .get(worker, &format!("/v1/pr-status?repo=api&base={base}"))
                .await
        }
    };
    assert_eq!(read("release/2026.10.3").await.body["state"], "success");
    let taken = team
        .world
        .post(
            &team.alice,
            "/v1/slots/release:cut/take",
            json!({"session_id": team.alice_session, "purpose": "cut 2026.10.3"}),
        )
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    assert_eq!(
        taken.body["slot"]["freezes"],
        json!(["api:refs/heads/release/*"])
    );
    for base in ["release/2026.10.3", "refs/heads/release/2026.11.1"] {
        let frozen = read(base).await;
        assert_eq!(frozen.body["state"], "failure", "{base}: {}", frozen.body);
    }
    for base in ["main", "release", "bob/42-rounding"] {
        assert_eq!(read(base).await.body["state"], "success", "{base}");
    }
}
