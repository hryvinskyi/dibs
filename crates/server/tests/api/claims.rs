//! Claiming, taking over, releasing and completing work items, and the status that shows them.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use time::Duration;
use time::format_description::well_known::Rfc3339;

use dibs_domain::member::Role;
use dibs_server::stream::Shutdown;
use dibs_server::sweep::run_sweeper;

use crate::support::{Person, START, World, assert_error, two_repositories};

/// A project with two repositories, alice (admin) and bob, each with a session.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    alice_session: u64,
    bob_session: u64,
}

impl Team {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        world.settings(&alice, two_repositories()).await;
        let alice_session = world.session(&alice, "laptop").await;
        let bob_session = world.session(&bob, "desk").await;
        Self {
            world,
            alice,
            bob,
            alice_session,
            bob_session,
        }
    }

    async fn claim(
        &self,
        who: &Person,
        session: u64,
        item: u64,
        areas: Value,
    ) -> crate::support::Answer {
        self.world
            .post(
                who,
                &format!("/v1/items/{item}/claim"),
                json!({"session_id": session, "areas": areas}),
            )
            .await
    }
}

#[tokio::test]
async fn a_claim_is_new_then_renewed() {
    let team = Team::new().await;
    let item = team.world.item(&team.alice, None, "Billing fix").await;
    let first = team
        .claim(
            &team.alice,
            team.alice_session,
            item,
            json!(["api:billing"]),
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["outcome"], "new");
    assert_eq!(first.body["claim"]["holder"], "alice");
    assert_eq!(first.body["claim"]["areas"], json!(["api:billing"]));
    assert_eq!(first.body["dropped_areas"], json!([]));
    let renewed = team
        .claim(&team.alice, team.alice_session, item, json!(["search"]))
        .await;
    assert_eq!(renewed.body["outcome"], "renewed");
    assert_eq!(
        renewed.body["claim"]["areas"],
        json!(["api:billing", "api:search"])
    );
}

#[tokio::test]
async fn every_answer_about_a_claim_names_its_item_by_its_reference() {
    let team = Team::new().await;
    let world = &team.world;
    let item = world
        .item(&team.alice, Some("gh:Acme/Api#12"), "Rounding")
        .await;
    let named = json!({
        "id": item, "ref": "gh:acme/api#12", "title": "Rounding",
        "link": "https://github.com/acme/api/issues/12",
    });
    let claimed = team
        .claim(&team.alice, team.alice_session, item, json!([]))
        .await;
    assert_eq!(claimed.body["item"], named, "{}", claimed.body);
    world.fakes.clock.advance(Duration::hours(2));
    let taken = world
        .post(
            &team.bob,
            &format!("/v1/items/{item}/takeover"),
            json!({"session_id": team.bob_session, "reason": "You have been away."}),
        )
        .await;
    assert_eq!(taken.body["item"], named, "{}", taken.body);
    let released = world
        .post(&team.bob, &format!("/v1/items/{item}/release"), json!({}))
        .await;
    assert_eq!(released.body["item"], named, "{}", released.body);
    team.claim(&team.bob, team.bob_session, item, json!([]))
        .await;
    let forced = world
        .post(
            &team.alice,
            &format!("/v1/admin/items/{item}/force-release"),
            json!({}),
        )
        .await;
    assert_eq!(forced.body["item"], named, "{}", forced.body);
    team.claim(&team.alice, team.alice_session, item, json!([]))
        .await;
    let done = world
        .post(&team.alice, &format!("/v1/items/{item}/done"), json!({}))
        .await;
    assert_eq!(done.body["item"], named, "{}", done.body);
}

#[tokio::test]
async fn a_claim_on_a_held_item_names_the_holder() {
    let team = Team::new().await;
    let item = team.world.item(&team.alice, None, "Billing fix").await;
    team.claim(&team.alice, team.alice_session, item, json!([]))
        .await;
    let refused = team
        .claim(&team.bob, team.bob_session, item, json!([]))
        .await;
    assert_error(&refused, StatusCode::CONFLICT, "held");
    assert_eq!(
        refused.body["error"]["details"],
        json!({
            "holder": "alice",
            "since": "2026-03-02T09:00:00Z",
            "last_seen": "2026-03-02T09:00:00Z"
        })
    );
}

#[tokio::test]
async fn a_claim_over_a_held_area_names_the_area_holder_and_item() {
    let team = Team::new().await;
    let first = team.world.item(&team.alice, None, "Billing fix").await;
    let second = team.world.item(&team.bob, None, "Billing feature").await;
    team.claim(
        &team.alice,
        team.alice_session,
        first,
        json!(["api:billing"]),
    )
    .await;
    let refused = team
        .claim(&team.bob, team.bob_session, second, json!(["api:billing"]))
        .await;
    assert_error(&refused, StatusCode::CONFLICT, "area_held");
    assert_eq!(
        refused.body["error"]["details"],
        json!({"area": "api:billing", "holder": "alice", "item": first})
    );
    let other_repo = team
        .claim(&team.bob, team.bob_session, second, json!(["web:billing"]))
        .await;
    assert_eq!(
        other_repo.status,
        StatusCode::OK,
        "an area name reused in another repository is another area"
    );
}

#[tokio::test]
async fn areas_that_do_not_resolve_are_refused() {
    let team = Team::new().await;
    let item = team.world.item(&team.alice, None, "x").await;
    let cases = [
        (json!(["billing"]), "ambiguous_area"),
        (json!(["api:nothing"]), "unknown_area"),
        (json!(["Not An Area"]), "invalid_area"),
    ];
    for (areas, code) in cases {
        let answer = team
            .claim(&team.alice, team.alice_session, item, areas)
            .await;
        assert_error(&answer, StatusCode::UNPROCESSABLE_ENTITY, code);
    }
    let ambiguous = team
        .claim(&team.alice, team.alice_session, item, json!(["billing"]))
        .await;
    assert_eq!(
        ambiguous.body["error"]["details"]["candidates"],
        json!(["api:billing", "web:billing"])
    );
}

#[tokio::test]
async fn a_stale_claim_is_taken_over_only_explicitly() {
    let team = Team::new().await;
    let item = team.world.item(&team.alice, None, "Billing fix").await;
    team.claim(
        &team.alice,
        team.alice_session,
        item,
        json!(["api:billing"]),
    )
    .await;
    let path = format!("/v1/items/{item}/takeover");
    let live = team
        .world
        .post(
            &team.bob,
            &path,
            json!({"session_id": team.bob_session, "reason": "away?"}),
        )
        .await;
    assert_error(&live, StatusCode::CONFLICT, "not_stale");
    assert_eq!(live.body["error"]["details"]["holder"], "alice");
    team.world.fakes.clock.advance(Duration::hours(2));
    let stale = team
        .claim(&team.bob, team.bob_session, item, json!([]))
        .await;
    assert_error(&stale, StatusCode::CONFLICT, "stale");
    assert_eq!(
        stale.body["error"]["details"],
        json!({"holder": "alice", "last_seen": "2026-03-02T09:00:00Z"})
    );
    let no_reason = team
        .world
        .post(&team.bob, &path, json!({"session_id": team.bob_session}))
        .await;
    assert_error(
        &no_reason,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    let taken = team
        .world
        .post(
            &team.bob,
            &path,
            json!({"session_id": team.bob_session, "reason": "Picking this up; you have been away."}),
        )
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    assert_eq!(taken.body["previous_holder"], "alice");
    assert_eq!(taken.body["claim"]["holder"], "bob");
    assert_eq!(taken.body["claim"]["areas"], json!(["api:billing"]));
    assert!(taken.body["thread_id"].as_u64().is_some());
    let own = team
        .world
        .post(
            &team.bob,
            &path,
            json!({"session_id": team.bob_session, "reason": "again"}),
        )
        .await;
    assert_error(&own, StatusCode::CONFLICT, "own_claim");
    let free = team.world.item(&team.alice, None, "Free").await;
    let nothing = team
        .world
        .post(
            &team.bob,
            &format!("/v1/items/{free}/takeover"),
            json!({"session_id": team.bob_session, "reason": "x"}),
        )
        .await;
    assert_error(&nothing, StatusCode::CONFLICT, "nothing_to_take");
}

#[tokio::test]
async fn only_the_holder_releases_or_completes() {
    let team = Team::new().await;
    let item = team.world.item(&team.alice, None, "Billing fix").await;
    let release = format!("/v1/items/{item}/release");
    let done = format!("/v1/items/{item}/done");
    let nobody = team
        .world
        .call(Method::POST, &release, Some(&team.alice.token), None)
        .await;
    assert_error(&nobody, StatusCode::CONFLICT, "no_claim");
    team.claim(&team.alice, team.alice_session, item, json!([]))
        .await;
    for path in [&release, &done] {
        let refused = team
            .world
            .call(Method::POST, path, Some(&team.bob.token), None)
            .await;
        assert_error(&refused, StatusCode::FORBIDDEN, "not_holder");
        assert_eq!(refused.body["error"]["details"], json!({"holder": "alice"}));
    }
    let released = team
        .world
        .call(Method::POST, &release, Some(&team.alice.token), None)
        .await;
    assert_eq!(released.status, StatusCode::OK);
    assert_eq!(released.body["claim"]["holder"], "alice");
    let shown = team
        .world
        .get(&team.alice, &format!("/v1/items/{item}"))
        .await;
    assert_eq!(shown.body["item"]["status"], "open");
    team.claim(&team.bob, team.bob_session, item, json!([]))
        .await;
    let completed = team
        .world
        .call(Method::POST, &done, Some(&team.bob.token), None)
        .await;
    assert_eq!(completed.status, StatusCode::OK);
    let closed = team
        .claim(&team.bob, team.bob_session, item, json!([]))
        .await;
    assert_error(&closed, StatusCode::CONFLICT, "item_closed");
    assert_eq!(closed.body["error"]["details"], json!({"status": "done"}));
}

#[tokio::test]
async fn an_integration_holds_no_claim() {
    let team = Team::new().await;
    let carol = team
        .world
        .member(&team.alice, "carol", Role::Importer)
        .await;
    let item = team.world.item(&team.alice, None, "x").await;
    let refused = team.claim(&carol, 1, item, json!([])).await;
    assert_error(&refused, StatusCode::FORBIDDEN, "forbidden");
}

#[tokio::test]
async fn a_claim_needs_a_session_of_the_caller() {
    let team = Team::new().await;
    let item = team.world.item(&team.alice, None, "x").await;
    let others = team
        .claim(&team.alice, team.bob_session, item, json!([]))
        .await;
    assert_error(&others, StatusCode::NOT_FOUND, "not_found");
    let missing = team
        .world
        .post(&team.alice, &format!("/v1/items/{item}/claim"), json!({}))
        .await;
    assert_error(
        &missing,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    let no_item = team
        .claim(&team.alice, team.alice_session, 4242, json!([]))
        .await;
    assert_error(&no_item, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn status_shows_live_and_stale_claims_and_holds() {
    let team = Team::new().await;
    let first = team.world.item(&team.alice, None, "First").await;
    let second = team.world.item(&team.bob, None, "Second").await;
    team.claim(
        &team.alice,
        team.alice_session,
        first,
        json!(["api:billing"]),
    )
    .await;
    team.world.fakes.clock.advance(Duration::hours(2));
    team.claim(
        &team.bob,
        team.bob_session,
        second,
        json!(["web:checkout", "api:search"]),
    )
    .await;
    let status = team.world.get(&team.alice, "/v1/status").await;
    assert_eq!(status.status, StatusCode::OK);
    let live = status.body["live"].as_array().unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0]["item"]["id"], second);
    assert_eq!(live[0]["claim"]["holder"], "bob");
    let stale = status.body["stale"].as_array().unwrap();
    assert_eq!(stale[0]["claim"]["holder"], "alice");
    assert_eq!(
        status.body["holds"],
        json!([
            {"area": "api:search", "holder": "bob", "item": second},
            {"area": "web:checkout", "holder": "bob", "item": second}
        ])
    );
}

#[tokio::test]
async fn a_claim_naming_its_branches_answers_them_with_their_sources_and_the_list_names_them() {
    let team = Team::new().await;
    let worker = team
        .world
        .member(&team.alice, "worker", Role::Importer)
        .await;
    let item = team
        .world
        .item(&team.bob, Some("gh:acme/api#42"), "Rounding")
        .await;
    let path = format!("/v1/items/{item}/claim");
    let claimed = team
        .world
        .post(
            &team.bob,
            &path,
            json!({
                "session_id": team.bob_session,
                "branches": {"api": "bob/42-refunds", "acme/web": "bob/42-refunds-ui"}
            }),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    assert_eq!(
        claimed.body["claim"]["branches"],
        json!({"api": "bob/42-refunds", "web": "bob/42-refunds-ui"})
    );
    assert_eq!(
        claimed.body["claim"]["branch_sources"],
        json!({"api": "named", "web": "named"})
    );
    let listed = team.world.get(&worker, "/v1/claims").await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(
        listed.body["claims"][0]["branches"],
        json!({"api": "bob/42-refunds", "web": "bob/42-refunds-ui"}),
        "the integration's list names the branches and nothing more: {}",
        listed.body
    );
    assert_eq!(
        listed.body["claims"][0]
            .as_object()
            .map(|claim| claim.contains_key("branch_sources")),
        Some(false)
    );
}

#[tokio::test]
async fn malformed_branches_or_an_unlisted_repository_refuse_the_claim_and_name_why() {
    let team = Team::new().await;
    let item = team
        .world
        .item(&team.bob, Some("gh:acme/api#42"), "Rounding")
        .await;
    let path = format!("/v1/items/{item}/claim");
    for branches in [
        json!({"api": "bob/42..refunds"}),
        json!({"api": 42}),
        json!(["bob/42-refunds"]),
        json!({"api": "bob/42-refunds", "acme/api": "bob/42-rounding"}),
    ] {
        let refused = team
            .world
            .post(
                &team.bob,
                &path,
                json!({"session_id": team.bob_session, "branches": branches}),
            )
            .await;
        assert_error(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        );
        assert_eq!(refused.body["error"]["details"]["field"], "branches");
    }
    let unlisted = team
        .world
        .post(
            &team.bob,
            &path,
            json!({"session_id": team.bob_session, "branches": {"acme/elsewhere": "bob/42"}}),
        )
        .await;
    assert_error(
        &unlisted,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unlisted_repository",
    );
    assert_eq!(unlisted.body["error"]["details"]["repo"], "acme/elsewhere");
    let status = team.world.get(&team.bob, "/v1/status").await;
    assert_eq!(
        status.body["live"],
        json!([]),
        "no claim was made: {}",
        status.body
    );
}

#[tokio::test]
async fn the_sweep_releases_a_claim_unseen_past_the_release_delay_and_the_log_shows_it() {
    let team = Team::new().await;
    let item = team
        .world
        .item(&team.bob, Some("gh:acme/api#42"), "Billing fix")
        .await;
    let claimed = team
        .claim(&team.bob, team.bob_session, item, json!(["api:billing"]))
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    team.world.fakes.clock.advance(Duration::days(7));
    let early = team.world.app.tick_claims.execute().await.unwrap();
    assert_eq!(early.released, 0, "seven days are not more than seven days");
    team.world.fakes.clock.advance(Duration::minutes(1));

    let shutdown = Shutdown::new();
    let sweeper = tokio::spawn(run_sweeper(
        Arc::clone(&team.world.app),
        StdDuration::from_millis(20),
        shutdown.signal(),
    ));
    let mut released = None;
    for _ in 0..200 {
        let events = team.world.get(&team.alice, "/v1/events").await;
        assert_eq!(events.status, StatusCode::OK, "{}", events.body);
        released = events.body["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["kind"] == "released_stale")
            .cloned();
        if released.is_some() {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    shutdown.stop();
    tokio::time::timeout(StdDuration::from_secs(5), sweeper)
        .await
        .unwrap()
        .unwrap();

    let released = released.expect("the sweep released the claim");
    let bob = team.bob.actor.member.get();
    assert_eq!(
        released["payload"],
        json!({"item": item, "member": bob, "last_seen": START.format(&Rfc3339).unwrap()})
    );
    assert_eq!(released["members"][bob.to_string()], "bob");
    let read = team
        .world
        .get(&team.bob, &format!("/v1/items/{item}"))
        .await;
    assert_eq!(read.body["claim"], Value::Null, "{}", read.body);
    assert_eq!(read.body["item"]["status"], "open");
    let taken = team
        .claim(
            &team.alice,
            team.alice_session,
            item,
            json!(["api:billing"]),
        )
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    assert_eq!(taken.body["outcome"], "new");
}

#[tokio::test]
async fn the_sweep_gives_back_an_area_no_work_showed_in_for_a_week_and_the_log_shows_it() {
    let team = Team::new().await;
    let item = team
        .world
        .item(&team.bob, Some("gh:acme/api#42"), "Billing fix")
        .await;
    let claimed = team
        .claim(
            &team.bob,
            team.bob_session,
            item,
            json!(["api:billing", "api:search"]),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    team.world
        .fakes
        .clock
        .advance(Duration::days(7) + Duration::minutes(1));
    let renewed = team
        .claim(&team.bob, team.bob_session, item, json!(["api:search"]))
        .await;
    assert_eq!(renewed.status, StatusCode::OK, "{}", renewed.body);

    let shutdown = Shutdown::new();
    let sweeper = tokio::spawn(run_sweeper(
        Arc::clone(&team.world.app),
        StdDuration::from_millis(20),
        shutdown.signal(),
    ));
    let mut returned = None;
    for _ in 0..200 {
        let events = team.world.get(&team.alice, "/v1/events").await;
        assert_eq!(events.status, StatusCode::OK, "{}", events.body);
        returned = events.body["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["kind"] == "areas_returned")
            .cloned();
        if returned.is_some() {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    shutdown.stop();
    tokio::time::timeout(StdDuration::from_secs(5), sweeper)
        .await
        .unwrap()
        .unwrap();

    let returned = returned.expect("the sweep gave the area back");
    let bob = team.bob.actor.member.get();
    assert_eq!(
        returned["payload"],
        json!({"item": item, "member": bob, "areas": ["api:billing"]})
    );
    assert_eq!(returned["members"][bob.to_string()], "bob");
    let read = team
        .world
        .get(&team.bob, &format!("/v1/items/{item}"))
        .await;
    assert_eq!(
        read.body["claim"]["areas"],
        json!(["api:search"]),
        "{}",
        read.body
    );
    let other = team
        .world
        .item(&team.alice, Some("gh:acme/api#43"), "Rates")
        .await;
    let taken = team
        .claim(
            &team.alice,
            team.alice_session,
            other,
            json!(["api:billing"]),
        )
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
}
