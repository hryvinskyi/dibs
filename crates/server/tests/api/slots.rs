//! The slot routes through the real router over a real SQLite file: taking, waiting, offers,
//! releases with an exit, a run's heartbeat and a takeover of its stalled hold, the worker's report,
//! the admin's forced release, each refusal with its status and code, and the sweep.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;
use dibs_server::stream::Shutdown;
use dibs_server::sweep::run_sweeper;

use crate::support::{Person, World, assert_error};

/// A project with alice (admin), bob and carol, each with a session, and these slots: `deploy:prod`
/// alone, `db:staging` and `deploy:staging` in the group `staging`.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    carol: Person,
    sessions: [u64; 3],
}

async fn team() -> Team {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let carol = world.member(&alice, "carol", Role::Member).await;
    world
        .settings(
            &alice,
            json!({"slots": [
                {"name": "deploy:prod", "max_hold": "30m", "commands": [{"matches": "^deploy\\.sh prod"}],
                 "freezes": ["refs/heads/main"], "note": "say it in the channel first"},
                {"name": "db:staging", "max_hold": "1h", "group": "staging"},
                {"name": "deploy:staging", "max_hold": "20m", "group": "staging"}
            ]}),
        )
        .await;
    let sessions = [
        world.session(&alice, "laptop").await,
        world.session(&bob, "desktop").await,
        world.session(&carol, "tablet").await,
    ];
    Team {
        world,
        alice,
        bob,
        carol,
        sessions,
    }
}

impl Team {
    async fn take(
        &self,
        who: &Person,
        session: u64,
        slot: &str,
        extra: Value,
    ) -> crate::support::Answer {
        let mut body =
            json!({"session_id": session, "purpose": format!("work of {}", who.actor.member)});
        if let (Some(fields), Some(more)) = (body.as_object_mut(), extra.as_object()) {
            fields.extend(more.clone());
        }
        self.world
            .post(who, &format!("/v1/slots/{slot}/take"), body)
            .await
    }
}

#[tokio::test]
async fn a_slot_is_taken_queued_for_released_and_offered_on() {
    let team = team().await;
    let [a, b, c] = team.sessions;
    let taken = team.take(&team.alice, a, "deploy:prod", json!({})).await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    assert_eq!(taken.body["outcome"], "taken");
    assert_eq!(taken.body["position"], Value::Null);
    let slot = &taken.body["slot"];
    assert_eq!(slot["name"], "deploy:prod");
    assert_eq!(slot["max_hold"], "30m");
    assert_eq!(slot["offer_for"], "5m");
    assert_eq!(slot["freezes"], json!(["default:refs/heads/main"]));
    assert_eq!(slot["note"], "say it in the channel first");
    assert_eq!(slot["hold"]["holder"], "alice");
    assert_eq!(slot["hold"]["liveness"], json!({"mode": "session"}));
    assert_eq!(slot["hold"]["expires_at"], "2026-03-02T09:30:00Z");
    assert_eq!(slot["hold"]["stalled"], false);

    let bob = team.take(&team.bob, b, "deploy:prod", json!({})).await;
    assert_eq!(
        (bob.body["outcome"].clone(), bob.body["position"].clone()),
        (json!("queued"), json!(1))
    );
    let carol = team.take(&team.carol, c, "deploy:prod", json!({})).await;
    assert_eq!(carol.body["position"], 2);
    let queue: Vec<&str> = carol.body["slot"]["queue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["member"].as_str().unwrap())
        .collect();
    assert_eq!(queue, ["bob", "carol"]);

    let released = team
        .world
        .post(&team.alice, "/v1/slots/deploy:prod/release", json!({}))
        .await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.body);
    assert_eq!(released.body["released"]["holder"], "alice");
    assert_eq!(released.body["slot"]["hold"], Value::Null);
    assert_eq!(released.body["slot"]["offer"]["member"], "bob");
    assert_eq!(
        released.body["slot"]["offer"]["until"],
        "2026-03-02T09:05:00Z"
    );

    let inbox = team.world.get(&team.bob, "/v1/inbox").await;
    assert_eq!(
        inbox.body["offers"],
        json!([{"slot": "deploy:prod", "until": "2026-03-02T09:05:00Z"}])
    );
    let still = team.take(&team.carol, c, "deploy:prod", json!({})).await;
    assert_eq!(
        still.body["position"], 1,
        "only the offered member takes it"
    );
    let bobs = team.take(&team.bob, b, "deploy:prod", json!({})).await;
    assert_eq!(bobs.body["outcome"], "taken");
    assert_eq!(
        team.world.get(&team.bob, "/v1/inbox").await.body["offers"],
        json!([])
    );

    let left = team
        .world
        .post(&team.carol, "/v1/slots/deploy:prod/leave", json!({}))
        .await;
    assert_eq!(left.status, StatusCode::OK);
    assert_eq!(left.body["slot"]["queue"], json!([]));
    let one = team.world.get(&team.alice, "/v1/slots/deploy:prod").await;
    assert_eq!(one.body["slot"]["hold"]["holder"], "bob");
    let all = team.world.get(&team.alice, "/v1/slots").await;
    let names: Vec<&str> = all.body["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["deploy:prod", "db:staging", "deploy:staging"]);
    let status = team.world.get(&team.alice, "/v1/status").await;
    assert_eq!(status.body["slots"][0]["hold"]["holder"], "bob");
}

#[tokio::test]
async fn a_hold_taken_from_one_session_is_the_members_in_every_other() {
    let team = team().await;
    let mcp = team.world.session(&team.alice, "laptop-mcp").await;
    let hook = team.sessions[0];
    team.take(&team.alice, mcp, "deploy:prod", json!({})).await;
    let renewed = team.take(&team.alice, hook, "deploy:prod", json!({})).await;
    assert_eq!(renewed.body["outcome"], "renewed");
    assert_eq!(renewed.body["slot"]["hold"]["session_id"], hook);
}

#[tokio::test]
async fn slots_of_a_group_exclude_each_other() {
    let team = team().await;
    let [a, b, _] = team.sessions;
    team.take(&team.alice, a, "db:staging", json!({})).await;
    let bob = team.take(&team.bob, b, "deploy:staging", json!({})).await;
    assert_eq!(bob.body["outcome"], "queued");
    assert_eq!(bob.body["slot"]["hold"]["slot"], "db:staging");
    assert_eq!(bob.body["slot"]["group"], "staging");
    let other = team.take(&team.alice, a, "deploy:staging", json!({})).await;
    assert_error(&other, StatusCode::CONFLICT, "holds_other_slot");
    assert_eq!(other.body["error"]["details"]["held"], "db:staging");
}

#[tokio::test]
async fn every_refusal_has_its_status_and_code() {
    let team = team().await;
    let [a, b, _] = team.sessions;
    let world = &team.world;
    let cases: Vec<(crate::support::Answer, StatusCode, &str)> = vec![
        (
            team.take(&team.alice, a, "ghost", json!({})).await,
            StatusCode::NOT_FOUND,
            "unknown_slot",
        ),
        (
            team.take(&team.alice, a, "Not%20A%20Slot", json!({})).await,
            StatusCode::NOT_FOUND,
            "unknown_slot",
        ),
        (
            world.get(&team.alice, "/v1/slots/ghost").await,
            StatusCode::NOT_FOUND,
            "unknown_slot",
        ),
        (
            world
                .post(
                    &team.alice,
                    "/v1/slots/deploy:prod/take",
                    json!({"session_id": a}),
                )
                .await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            team.take(&team.alice, a, "deploy:prod", json!({"mode": "forever"}))
                .await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            team.take(
                &team.alice,
                a,
                "deploy:prod",
                json!({"mode": "until_run", "workflow": "deploy.yml"}),
            )
            .await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            team.take(&team.alice, b, "deploy:prod", json!({})).await,
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            team.take(&team.alice, a, "deploy:prod", json!({"item": 999}))
                .await,
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            world
                .post(&team.alice, "/v1/slots/deploy:prod/release", json!({}))
                .await,
            StatusCode::CONFLICT,
            "slot_not_held",
        ),
        (
            world
                .post(&team.alice, "/v1/slots/deploy:prod/leave", json!({}))
                .await,
            StatusCode::CONFLICT,
            "not_waiting",
        ),
        (
            world
                .post(&team.alice, "/v1/slots/deploy:prod/heartbeat", json!({}))
                .await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            world
                .post(
                    &team.alice,
                    "/v1/slots/deploy:prod/heartbeat",
                    json!({"run_id": 5}),
                )
                .await,
            StatusCode::CONFLICT,
            "run_ended",
        ),
        (
            world
                .post(
                    &team.alice,
                    "/v1/slots/deploy:prod/release",
                    json!({"exit": {"conclusion": "success"}}),
                )
                .await,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        ),
        (
            world
                .post(
                    &team.bob,
                    "/v1/slots/deploy:prod/takeover",
                    json!({"session_id": b, "purpose": "x", "reason": "checked"}),
                )
                .await,
            StatusCode::CONFLICT,
            "slot_not_held",
        ),
        (
            world
                .post(
                    &team.bob,
                    "/v1/admin/slots/deploy:prod/force-release",
                    json!({"session_id": b, "reason": "x"}),
                )
                .await,
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
        (
            world
                .post(
                    &team.alice,
                    "/v1/admin/slots/deploy:prod/force-release",
                    json!({"session_id": a, "reason": "x"}),
                )
                .await,
            StatusCode::CONFLICT,
            "slot_not_held",
        ),
        (
            world
                .post(
                    &team.bob,
                    "/v1/slots/deploy:prod/run-finished",
                    json!({"workflow": "deploy.yml", "sha": "abc1234", "conclusion": "success"}),
                )
                .await,
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
    ];
    for (n, (answer, status, code)) in cases.iter().enumerate() {
        assert_eq!(answer.status, *status, "case {n}: {}", answer.body);
        assert_error(answer, *status, code);
    }
    team.take(&team.alice, a, "deploy:prod", json!({})).await;
    let not_holder = world
        .post(&team.bob, "/v1/slots/deploy:prod/release", json!({}))
        .await;
    assert_error(&not_holder, StatusCode::FORBIDDEN, "not_holder");
    assert_eq!(not_holder.body["error"]["details"]["holder"], "alice");
    let not_stalled = world
        .post(
            &team.bob,
            "/v1/slots/deploy:prod/takeover",
            json!({"session_id": b, "purpose": "x", "reason": "checked"}),
        )
        .await;
    assert_error(&not_stalled, StatusCode::CONFLICT, "not_stalled");
    assert_eq!(not_stalled.body["error"]["details"]["holder"], "alice");
    let own = world
        .post(
            &team.alice,
            "/v1/slots/deploy:prod/takeover",
            json!({"session_id": a, "purpose": "x", "reason": "checked"}),
        )
        .await;
    assert_error(&own, StatusCode::CONFLICT, "own_hold");
}

#[tokio::test]
async fn a_run_beats_releases_with_its_exit_and_a_stalled_hold_is_taken_over_with_a_reason() {
    let team = team().await;
    let [a, b, _] = team.sessions;
    let world = &team.world;
    let run = team
        .take(&team.alice, a, "deploy:prod", json!({"mode": "run"}))
        .await;
    let run_id = run.body["run_id"].as_u64().unwrap();
    assert_eq!(
        run.body["slot"]["hold"]["liveness"],
        json!({"mode": "run", "run": run_id})
    );
    world.fakes.clock.advance(Duration::minutes(3));
    let stalled = world.get(&team.bob, "/v1/slots/deploy:prod").await;
    assert_eq!(stalled.body["slot"]["hold"]["stalled"], true);
    let beat = world
        .post(
            &team.alice,
            "/v1/slots/deploy:prod/heartbeat",
            json!({"run_id": run_id}),
        )
        .await;
    assert_eq!(beat.status, StatusCode::OK, "{}", beat.body);
    assert_eq!(beat.body["slot"]["hold"]["stalled"], false);
    world.fakes.clock.advance(Duration::minutes(3));
    let taken = world
        .post(
            &team.bob,
            "/v1/slots/deploy:prod/takeover",
            json!({"session_id": b, "purpose": "finish the deploy", "reason": "the deploy log stopped; nothing runs"}),
        )
        .await;
    assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
    assert_eq!(taken.body["previous"]["holder"], "alice");
    assert_eq!(taken.body["slot"]["hold"]["holder"], "bob");
    let thread = taken.body["thread_id"].as_u64().unwrap();
    let told = world
        .get(&team.alice, &format!("/v1/threads/{thread}"))
        .await;
    assert_eq!(told.body["messages"][0]["author"], "dibs");
    assert_eq!(told.body["messages"][0]["system"], true);
    assert_eq!(
        told.body["thread"]["author"], "bob",
        "dibs writes it for bob"
    );
    assert!(
        told.body["messages"][0]["body"]
            .as_str()
            .unwrap()
            .ends_with("Their reason: the deploy log stopped; nothing runs"),
        "{}",
        told.body
    );
    let gone = world
        .post(
            &team.alice,
            "/v1/slots/deploy:prod/heartbeat",
            json!({"run_id": run_id}),
        )
        .await;
    assert_error(&gone, StatusCode::CONFLICT, "taken_over");
    assert_eq!(gone.body["error"]["details"]["holder"], "bob");

    let mine = team
        .take(&team.alice, a, "deploy:staging", json!({"mode": "run"}))
        .await;
    let mine_id = mine.body["run_id"].as_u64().unwrap();
    let ended = world
        .post(
            &team.alice,
            "/v1/slots/deploy:staging/release",
            json!({"run_id": mine_id, "exit": {"code": 3}}),
        )
        .await;
    assert_eq!(ended.status, StatusCode::OK, "{}", ended.body);
    let events = world.get(&team.alice, "/v1/events").await;
    let released = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["kind"] == "slot_released")
        .unwrap()
        .clone();
    assert_eq!(released["payload"]["exit"], json!({"code": 3}));
    assert_eq!(released["payload"]["slot"], "deploy:staging");
}

#[tokio::test]
async fn the_worker_ends_a_hold_tied_to_its_run_and_a_repeat_answers_the_same() {
    let team = team().await;
    let worker = team
        .world
        .member(&team.alice, "board", Role::Importer)
        .await;
    team.take(
        &team.alice,
        team.sessions[0],
        "deploy:prod",
        json!({"mode": "until_run", "workflow": "deploy.yml", "sha": "abc1234"}),
    )
    .await;
    let report = |sha: &str| json!({"workflow": "deploy.yml", "sha": sha, "conclusion": "success"});
    let other = team
        .world
        .post(
            &worker,
            "/v1/slots/deploy:prod/run-finished",
            report("def5678"),
        )
        .await;
    assert_eq!(other.body, json!({"matched": false}));
    let first = team
        .world
        .post(
            &worker,
            "/v1/slots/deploy:prod/run-finished",
            report("abc1234"),
        )
        .await;
    assert_eq!(
        (first.status, first.body.clone()),
        (StatusCode::OK, json!({"matched": true}))
    );
    let again = team
        .world
        .post(
            &worker,
            "/v1/slots/deploy:prod/run-finished",
            report("abc1234"),
        )
        .await;
    assert_eq!(
        (again.status, again.body),
        (StatusCode::OK, json!({"matched": false}))
    );
    let read = team.world.get(&worker, "/v1/slots/deploy:prod").await;
    assert_eq!(read.body["slot"]["hold"], Value::Null);
}

#[tokio::test]
async fn an_admin_forces_a_release_and_the_holder_reads_why() {
    let team = team().await;
    team.take(&team.bob, team.sessions[1], "deploy:prod", json!({}))
        .await;
    let forced = team
        .world
        .post(
            &team.alice,
            "/v1/admin/slots/deploy:prod/force-release",
            json!({"session_id": team.sessions[0], "reason": "the release is cancelled"}),
        )
        .await;
    assert_eq!(forced.status, StatusCode::OK, "{}", forced.body);
    assert_eq!(forced.body["holder"], "bob");
    let thread = forced.body["thread_id"].as_u64().unwrap();
    let told = team
        .world
        .get(&team.bob, &format!("/v1/threads/{thread}"))
        .await;
    assert!(
        told.body["messages"][0]["body"]
            .as_str()
            .unwrap()
            .ends_with("Their reason: the release is cancelled"),
        "{}",
        told.body
    );
    assert_eq!(told.body["messages"][0]["system"], true);
    assert_eq!(told.body["messages"][0]["importance"], "high");
}

#[tokio::test]
async fn the_sweep_offers_a_freed_slot_with_nobody_touching_it_and_stops_on_its_signal() {
    let team = team().await;
    let [a, b, _] = team.sessions;
    team.take(&team.alice, a, "deploy:prod", json!({})).await;
    team.take(&team.bob, b, "deploy:prod", json!({})).await;
    team.world.fakes.clock.advance(Duration::minutes(31));
    let shutdown = Shutdown::new();
    let sweeper = tokio::spawn(run_sweeper(
        Arc::clone(&team.world.app),
        StdDuration::from_millis(20),
        shutdown.signal(),
    ));
    let mut kinds = Vec::new();
    for _ in 0..200 {
        let mut tx = dibs_app::ports::Store::begin(&*team.world.store)
            .await
            .unwrap();
        kinds = tx
            .events_after(team.alice.actor.project, None, 10_000)
            .await
            .unwrap()
            .iter()
            .map(|event| event.kind.name())
            .collect();
        drop(tx);
        if kinds.contains(&"slot_offered") {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    assert!(
        kinds.ends_with(&[
            "slot_expired",
            "slot_offered",
            "thread_started",
            "message_posted"
        ]),
        "the sweep tells the holder whose hold expired: {kinds:?}"
    );
    shutdown.stop();
    tokio::time::timeout(StdDuration::from_secs(5), sweeper)
        .await
        .unwrap()
        .unwrap();
}
