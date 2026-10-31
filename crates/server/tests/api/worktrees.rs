//! The worktree routes through the real router over a real SQLite file: taking, taking again,
//! answering busy, releasing by worktree and by holder, beats, the end of a session, the admin's
//! forced release, each refusal with its status and code, what other members see, and the sweep
//! that ends a silent agent's hold and keeps a session's.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;
use dibs_server::stream::Shutdown;
use dibs_server::sweep::run_sweeper;

use crate::support::{Answer, Person, World, assert_error};

/// The machine every take of the tests names.
const MACHINE: &str = "0123456789abcdef0123456789abcdef";

/// The pool every take of the tests names.
const POOL: &str = "c0ffee42";

/// A project with alice (admin) and bob; alice has two sessions on `laptop`, bob one on
/// `desktop`.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    /// Alice's sessions, then bob's.
    sessions: [u64; 3],
}

async fn team() -> Team {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let sessions = [
        session(&world, &alice, "laptop", "laptop/hook").await,
        session(&world, &alice, "laptop", "laptop/cli").await,
        session(&world, &bob, "desktop", "desktop/hook").await,
    ];
    Team {
        world,
        alice,
        bob,
        sessions,
    }
}

async fn session(world: &World, who: &Person, host: &str, label: &str) -> u64 {
    let answer = world
        .post(
            who,
            "/v1/sessions",
            json!({"host": host, "label": label, "agent": "agent"}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    answer.body["session_id"].as_u64().unwrap()
}

/// A take of `worktree1`, `worktree2` and `worktree3`, in that order, with `extra` fields.
async fn take(world: &World, who: &Person, session: u64, extra: Value) -> Answer {
    let mut body = json!({
        "session_id": session, "machine": MACHINE, "pool": POOL,
        "names": ["worktree1", "worktree2", "worktree3"], "purpose": "fix the invoice totals",
    });
    if let (Some(fields), Some(more)) = (body.as_object_mut(), extra.as_object()) {
        fields.extend(more.clone());
    }
    world.post(who, "/v1/worktrees/take", body).await
}

async fn release(world: &World, who: &Person, session: u64, extra: Value) -> Answer {
    let mut body = json!({"session_id": session, "machine": MACHINE, "pool": POOL});
    if let (Some(fields), Some(more)) = (body.as_object_mut(), extra.as_object()) {
        fields.extend(more.clone());
    }
    world.post(who, "/v1/worktrees/release", body).await
}

async fn beat(world: &World, who: &Person, session: u64, beats: Value) -> Answer {
    world
        .post(
            who,
            "/v1/worktrees/beat",
            json!({"session_id": session, "machine": MACHINE, "pool": POOL, "beats": beats}),
        )
        .await
}

fn took(answer: &Answer) -> (String, bool) {
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(answer.body["outcome"], "took", "{}", answer.body);
    (
        answer.body["worktree"].as_str().unwrap().to_owned(),
        answer.body["again"].as_bool().unwrap(),
    )
}

/// The worktrees an answer's list names, in order.
fn worktrees(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|hold| hold["worktree"].as_str().unwrap())
        .collect()
}

/// The kinds of the project's events, read from the store as the operator would.
async fn event_kinds(world: &World, who: &Person) -> Vec<&'static str> {
    let mut tx = dibs_app::ports::Store::begin(&*world.store).await.unwrap();
    tx.events_after(who.actor.project, None, 10_000)
        .await
        .unwrap()
        .iter()
        .map(|event| event.kind.name())
        .collect()
}

#[tokio::test]
async fn a_holder_takes_a_worktree_again_others_get_the_next_and_a_full_pool_is_busy() {
    let team = team().await;
    let world = &team.world;
    let [hook, cli, _] = team.sessions;
    let item = world.item(&team.alice, None, "Invoice totals").await;

    let first = take(
        world,
        &team.alice,
        hook,
        json!({"agent": "agent-1", "item": item}),
    )
    .await;
    assert_eq!(took(&first), ("worktree1".to_owned(), false));
    assert_eq!(first.body["held"], json!([]));
    let again = take(world, &team.alice, hook, json!({"agent": "agent-1"})).await;
    assert_eq!(
        took(&again),
        ("worktree1".to_owned(), true),
        "a retried take gets the same worktree"
    );
    let own = take(world, &team.alice, hook, json!({})).await;
    assert_eq!(
        took(&own),
        ("worktree2".to_owned(), false),
        "the session and its agent are two holders"
    );
    let other = take(world, &team.alice, cli, json!({"agent": "agent-1"})).await;
    assert_eq!(
        took(&other),
        ("worktree3".to_owned(), false),
        "an agent key of another session is another holder"
    );

    let busy = take(world, &team.alice, hook, json!({"agent": "agent-2"})).await;
    assert_eq!(busy.status, StatusCode::OK, "{}", busy.body);
    assert_eq!(busy.body["outcome"], "busy");
    assert_eq!(busy.body["worktree"], Value::Null);
    assert_eq!(busy.body["again"], false);
    let held = &busy.body["held"];
    assert_eq!(worktrees(held), ["worktree1", "worktree2", "worktree3"]);
    assert_eq!(
        held[0],
        json!({
            "worktree": "worktree1", "holder": "agent", "session_id": hook,
            "session": "laptop/hook", "agent": "agent-1", "host": "laptop",
            "purpose": "fix the invoice totals", "item": item,
            "taken_at": "2026-03-02T09:00:00Z", "last_seen": "2026-03-02T09:00:00Z",
        })
    );
    assert_eq!(
        (held[1]["holder"].clone(), held[1]["agent"].clone()),
        (json!("session"), Value::Null)
    );
    assert_eq!(held[2]["session"], "laptop/cli");
}

#[tokio::test]
async fn the_member_reads_their_pools_whole_and_everyone_else_the_holds_alone() {
    let team = team().await;
    let world = &team.world;
    let [hook, _, _] = team.sessions;
    take(world, &team.alice, hook, json!({"agent": "agent-1"})).await;
    take(world, &team.alice, hook, json!({})).await;

    let mine = world.get(&team.alice, "/v1/worktrees").await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.body);
    let pools = mine.body["pools"].as_array().unwrap();
    assert_eq!(pools.len(), 1, "{}", mine.body);
    assert_eq!(
        (pools[0]["machine"].clone(), pools[0]["pool"].clone()),
        (json!(MACHINE), json!(POOL))
    );
    assert_eq!(worktrees(&pools[0]["holds"]), ["worktree1", "worktree2"]);
    assert_eq!(pools[0]["holds"][0]["agent"], "agent-1");
    assert_eq!(
        mine.body["board"],
        json!([
            {"member": "alice", "host": "laptop", "worktree": "worktree1", "holder": "agent",
             "purpose": "fix the invoice totals", "item": null, "since": "2026-03-02T09:00:00Z"},
            {"member": "alice", "host": "laptop", "worktree": "worktree2", "holder": "session",
             "purpose": "fix the invoice totals", "item": null, "since": "2026-03-02T09:00:00Z"},
        ])
    );
    let filtered = world
        .get(&team.alice, &format!("/v1/worktrees?machine={MACHINE}"))
        .await;
    assert_eq!(filtered.body["pools"], mine.body["pools"]);
    let elsewhere = world
        .get(
            &team.alice,
            "/v1/worktrees?machine=ffffffffffffffffffffffffffffffff",
        )
        .await;
    assert_eq!(elsewhere.body["pools"], json!([]));
    assert_eq!(elsewhere.body["board"], mine.body["board"]);
    let status = world.get(&team.alice, "/v1/status").await;
    assert_eq!(status.body["worktrees"], mine.body["pools"]);

    let theirs = world.get(&team.bob, "/v1/worktrees").await;
    assert_eq!(theirs.body["pools"], json!([]));
    assert_eq!(theirs.body["board"], mine.body["board"]);
    assert_eq!(
        world.get(&team.bob, "/v1/status").await.body["worktrees"],
        json!([])
    );
    let importer = world.member(&team.alice, "board", Role::Importer).await;
    let read = world.get(&importer, "/v1/worktrees").await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    assert_eq!(read.body["board"], mine.body["board"]);
    for answer in [&theirs, &read] {
        let text = answer.body.to_string();
        assert!(
            !text.contains(MACHINE) && !text.contains(POOL) && !text.contains("agent-1"),
            "{text}"
        );
    }

    for reader in [&team.bob, &team.alice] {
        let log = world.get(reader, "/v1/events").await;
        let taken: Vec<&Value> = log.body["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "worktree_taken")
            .collect();
        assert_eq!(
            taken.len(),
            2,
            "every member's board shows the holds, so every member reads their events: {}",
            log.body
        );
        for event in taken {
            let fields: Vec<&str> = event["payload"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                fields,
                ["holder", "host", "member", "purpose", "worktree"],
                "no machine, pool, session or agent: {event}"
            );
        }
        let text = log.body.to_string();
        assert!(
            !text.contains(MACHINE) && !text.contains(POOL) && !text.contains("agent-1"),
            "{text}"
        );
    }
}

#[tokio::test]
async fn another_members_event_stream_follows_the_holds() {
    let team = team().await;
    let world = &team.world;
    let [hook, _, _] = team.sessions;
    let (status, mut bobs) = crate::events::open(
        world,
        crate::events::stream_request(&team.bob, "/v1/events/stream?after=0", None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    take(world, &team.alice, hook, json!({"agent": "agent-1"})).await;
    let released = release(world, &team.alice, hook, json!({"worktree": "worktree1"})).await;
    assert_eq!(released.status, StatusCode::OK, "{}", released.body);
    let kinds: Vec<String> = bobs
        .events(2)
        .await
        .into_iter()
        .map(|(_, event)| event["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds,
        ["worktree_taken", "worktree_released"],
        "bob's board refreshes on alice's holds"
    );
}

#[tokio::test]
async fn a_pool_is_always_the_callers_own() {
    let team = team().await;
    let world = &team.world;
    let [hook, _, desk] = team.sessions;
    take(world, &team.alice, hook, json!({})).await;
    let bobs = take(world, &team.bob, desk, json!({})).await;
    assert_eq!(
        took(&bobs),
        ("worktree1".to_owned(), false),
        "naming alice's machine and pool reaches bob's own"
    );
    let not_his = release(world, &team.bob, desk, json!({"holder_session": hook})).await;
    assert_error(&not_his, StatusCode::NOT_FOUND, "not_found");
    let alices = world.get(&team.alice, "/v1/worktrees").await;
    assert_eq!(worktrees(&alices.body["pools"][0]["holds"]), ["worktree1"]);
    assert_eq!(alices.body["pools"][0]["holds"][0]["session_id"], hook);
}

#[tokio::test]
async fn a_hold_is_released_by_its_worktree_or_by_its_holder() {
    let team = team().await;
    let world = &team.world;
    let [hook, cli, _] = team.sessions;
    take(world, &team.alice, hook, json!({"agent": "agent-1"})).await;
    take(world, &team.alice, hook, json!({})).await;

    let by_name = release(world, &team.alice, cli, json!({"worktree": "worktree1"})).await;
    assert_eq!(by_name.status, StatusCode::OK, "{}", by_name.body);
    assert_eq!(by_name.body["released"]["worktree"], "worktree1");
    assert_eq!(by_name.body["released"]["agent"], "agent-1");
    let by_holder = release(world, &team.alice, cli, json!({"holder_session": hook})).await;
    assert_eq!(by_holder.status, StatusCode::OK, "{}", by_holder.body);
    assert_eq!(by_holder.body["released"]["worktree"], "worktree2");
    assert_eq!(by_holder.body["released"]["holder"], "session");

    let none = release(world, &team.alice, cli, json!({"worktree": "worktree1"})).await;
    assert_error(&none, StatusCode::CONFLICT, "worktree_not_held");
    let agent = release(
        world,
        &team.alice,
        hook,
        json!({"holder_session": hook, "agent": "agent-1"}),
    )
    .await;
    assert_error(&agent, StatusCode::CONFLICT, "worktree_not_held");
    assert_eq!(
        world.get(&team.alice, "/v1/worktrees").await.body["pools"],
        json!([])
    );
    let kinds = event_kinds(world, &team.alice).await;
    assert!(
        kinds.ends_with(&[
            "worktree_taken",
            "worktree_taken",
            "worktree_released",
            "worktree_released"
        ]),
        "{kinds:?}"
    );
}

#[tokio::test]
async fn every_refusal_has_its_status_and_code() {
    let team = team().await;
    let world = &team.world;
    let [hook, _, desk] = team.sessions;
    let importer = world.member(&team.alice, "board", Role::Importer).await;
    take(world, &team.alice, hook, json!({})).await;

    let invalid = |answer: &Answer, field: &str| {
        assert_error(answer, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
        assert_eq!(answer.body["error"]["details"]["field"], field);
    };
    invalid(
        &take(world, &team.alice, hook, json!({"machine": "laptop"})).await,
        "machine",
    );
    invalid(
        &take(world, &team.alice, hook, json!({"pool": "Not/A/Pool"})).await,
        "pool",
    );
    invalid(
        &take(world, &team.alice, hook, json!({"names": ["../etc"]})).await,
        "names",
    );
    invalid(
        &take(world, &team.alice, hook, json!({"agent": "two words"})).await,
        "agent",
    );
    invalid(
        &take(world, &team.alice, hook, json!({"purpose": ""})).await,
        "purpose",
    );
    invalid(
        &world
            .post(
                &team.alice,
                "/v1/worktrees/take",
                json!({"session_id": hook, "machine": MACHINE, "pool": POOL, "purpose": "x"}),
            )
            .await,
        "names",
    );
    let empty = take(world, &team.alice, hook, json!({"names": []})).await;
    assert_error(
        &empty,
        StatusCode::UNPROCESSABLE_ENTITY,
        "worktree_names_invalid",
    );
    let twice = take(
        world,
        &team.alice,
        hook,
        json!({"names": ["worktree1", "worktree1"]}),
    )
    .await;
    assert_error(
        &twice,
        StatusCode::UNPROCESSABLE_ENTITY,
        "worktree_names_invalid",
    );
    assert_eq!(twice.body["error"]["details"]["worktree"], "worktree1");
    assert_error(
        &take(world, &team.alice, desk, json!({})).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_error(
        &take(world, &team.alice, hook, json!({"item": 999})).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_error(
        &take(world, &importer, hook, json!({})).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let anonymous = world
        .call(
            Method::POST,
            "/v1/worktrees/take",
            None,
            Some(json!({"session_id": hook})),
        )
        .await;
    assert_error(&anonymous, StatusCode::UNAUTHORIZED, "unauthorized");
    assert_error(
        &world.get(&team.alice, "/v1/worktrees?machine=zz").await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );

    invalid(
        &release(
            world,
            &team.alice,
            hook,
            json!({"worktree": "worktree1", "holder_session": hook}),
        )
        .await,
        "worktree",
    );
    invalid(
        &release(world, &team.alice, hook, json!({})).await,
        "worktree",
    );
    invalid(
        &release(
            world,
            &team.alice,
            hook,
            json!({"worktree": "worktree1", "agent": "agent-1"}),
        )
        .await,
        "agent",
    );
    assert_error(
        &release(world, &importer, hook, json!({"worktree": "worktree1"})).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );

    invalid(
        &beat(
            world,
            &team.alice,
            hook,
            json!([{"worktree": "worktree1", "holder_session": hook, "seen_at": "yesterday"}]),
        )
        .await,
        "beats[0].seen_at",
    );
    invalid(
        &world
            .post(
                &team.alice,
                "/v1/worktrees/beat",
                json!({"session_id": hook, "machine": MACHINE, "pool": POOL}),
            )
            .await,
        "beats",
    );
    assert_error(
        &beat(
            world,
            &team.alice,
            hook,
            json!([{"worktree": "worktree1", "holder_session": desk,
                    "seen_at": "2026-03-02T09:00:00Z"}]),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    for path in [
        format!("/v1/sessions/{desk}/end"),
        "/v1/sessions/me/end".to_owned(),
    ] {
        assert_error(
            &world.post(&team.alice, &path, json!({})).await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
    }
    assert_error(
        &world
            .post(&importer, &format!("/v1/sessions/{hook}/end"), json!({}))
            .await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );

    let force = |who: &Person, body: Value| {
        let who = who.clone();
        async move {
            world
                .post(&who, "/v1/admin/worktrees/force-release", body)
                .await
        }
    };
    let body = json!({"session_id": desk, "member": "alice", "host": "laptop",
                      "worktree": "worktree1", "reason": "the build is broken"});
    assert_error(
        &force(&team.bob, body).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_error(
        &force(
            &team.alice,
            json!({"session_id": hook, "member": "carol", "host": "laptop",
                   "worktree": "worktree1", "reason": "x"}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_error(
        &force(
            &team.alice,
            json!({"session_id": hook, "member": "alice", "host": "desktop",
                   "worktree": "worktree1", "reason": "x"}),
        )
        .await,
        StatusCode::CONFLICT,
        "worktree_not_held",
    );
    invalid(
        &force(
            &team.alice,
            json!({"session_id": hook, "member": "alice", "host": "laptop", "worktree": "worktree1"}),
        )
        .await,
        "reason",
    );
}

#[tokio::test]
async fn beats_keep_a_hold_and_name_the_ones_that_hold_nothing() {
    let team = team().await;
    let world = &team.world;
    let [hook, cli, _] = team.sessions;
    take(world, &team.alice, hook, json!({"agent": "agent-1"})).await;
    world.fakes.clock.advance(Duration::minutes(15));

    let beaten = beat(
        world,
        &team.alice,
        cli,
        json!([
            {"worktree": "worktree1", "holder_session": hook, "agent": "agent-1",
             "seen_at": "2026-03-02T09:14:00Z"},
            {"worktree": "worktree1", "holder_session": hook, "agent": "agent-1",
             "seen_at": "2026-03-02T09:15:00Z"},
            {"worktree": "worktree2", "holder_session": hook, "seen_at": "2026-03-02T09:15:00Z"},
            {"worktree": "worktree1", "holder_session": cli, "seen_at": "2026-03-02T09:15:00Z"},
        ]),
    )
    .await;
    assert_eq!(beaten.status, StatusCode::OK, "{}", beaten.body);
    assert_eq!(
        beaten.body,
        json!({
            "kept": [{"worktree": "worktree1", "holder_session": hook, "agent": "agent-1"}],
            "unknown": [
                {"worktree": "worktree2", "holder_session": hook, "agent": null},
                {"worktree": "worktree1", "holder_session": cli, "agent": null},
            ],
        })
    );
    let mine = world.get(&team.alice, "/v1/worktrees").await;
    assert_eq!(
        mine.body["pools"][0]["holds"][0]["last_seen"],
        "2026-03-02T09:15:00Z"
    );

    world.fakes.clock.advance(Duration::minutes(15));
    let future = beat(
        world,
        &team.alice,
        hook,
        json!([{"worktree": "worktree1", "holder_session": hook, "agent": "agent-1",
                "seen_at": "2027-01-01T00:00:00Z"}]),
    )
    .await;
    assert_eq!(future.body["kept"][0]["worktree"], "worktree1");
    let mine = world.get(&team.alice, "/v1/worktrees").await;
    assert_eq!(
        mine.body["pools"][0]["holds"][0]["last_seen"], "2026-03-02T09:30:00Z",
        "a time after now counts as now"
    );
    let none = beat(world, &team.alice, hook, json!([])).await;
    assert_eq!(none.body, json!({"kept": [], "unknown": []}));
}

#[tokio::test]
async fn the_sweep_ends_a_silent_agents_hold_and_keeps_a_waiting_sessions() {
    let team = team().await;
    let world = &team.world;
    let [hook, _, _] = team.sessions;
    take(world, &team.alice, hook, json!({"agent": "agent-1"})).await;
    take(world, &team.alice, hook, json!({})).await;
    world.fakes.clock.advance(Duration::minutes(21));

    let shutdown = Shutdown::new();
    let sweeper = tokio::spawn(run_sweeper(
        Arc::clone(&world.app),
        StdDuration::from_millis(20),
        shutdown.signal(),
    ));
    let mut kinds = Vec::new();
    for _ in 0..200 {
        kinds = event_kinds(world, &team.alice).await;
        if kinds.contains(&"worktree_released") {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    shutdown.stop();
    tokio::time::timeout(StdDuration::from_secs(5), sweeper)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "worktree_released")
            .count(),
        1,
        "only the agent's hold went silent: {kinds:?}"
    );
    let log = world.get(&team.alice, "/v1/events").await;
    let released = log.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "worktree_released")
        .unwrap()
        .clone();
    assert_eq!(
        released["payload"],
        json!({"member": team.alice.actor.member.0, "host": "laptop", "worktree": "worktree1",
               "holder": "agent", "purpose": "fix the invoice totals", "cause": "silent",
               "by": null})
    );

    let mine = world.get(&team.alice, "/v1/worktrees").await;
    assert_eq!(
        worktrees(&mine.body["pools"][0]["holds"]),
        ["worktree2"],
        "a session waits for its person for hours"
    );
    let late = beat(
        world,
        &team.alice,
        hook,
        json!([{"worktree": "worktree1", "holder_session": hook, "agent": "agent-1",
                "seen_at": "2026-03-02T09:21:00Z"}]),
    )
    .await;
    assert_eq!(
        late.body["unknown"],
        json!([{"worktree": "worktree1", "holder_session": hook, "agent": "agent-1"}]),
        "a beat for a freed hold holds nothing"
    );

    world.fakes.clock.advance(Duration::hours(12));
    let later = world.get(&team.alice, "/v1/worktrees").await;
    assert_eq!(later.body["pools"], json!([]));
    assert_eq!(later.body["board"], json!([]));
}

#[tokio::test]
async fn the_end_of_a_session_ends_its_holds_and_its_agents() {
    let team = team().await;
    let world = &team.world;
    let [hook, cli, _] = team.sessions;
    take(world, &team.alice, hook, json!({"agent": "agent-1"})).await;
    take(world, &team.alice, hook, json!({})).await;
    take(world, &team.alice, cli, json!({})).await;

    let ended = world
        .post(&team.alice, &format!("/v1/sessions/{hook}/end"), json!({}))
        .await;
    assert_eq!(ended.status, StatusCode::OK, "{}", ended.body);
    assert_eq!(
        worktrees(&ended.body["released"]),
        ["worktree1", "worktree2"]
    );
    assert_eq!(ended.body["released"][0]["agent"], "agent-1");
    let mine = world.get(&team.alice, "/v1/worktrees").await;
    assert_eq!(worktrees(&mine.body["pools"][0]["holds"]), ["worktree3"]);
    let log = world.get(&team.alice, "/v1/events").await;
    let causes: Vec<&str> = log.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "worktree_released")
        .map(|event| event["payload"]["cause"].as_str().unwrap())
        .collect();
    assert_eq!(causes, ["session_ended", "session_ended"]);

    let again = world
        .post(&team.alice, &format!("/v1/sessions/{hook}/end"), json!({}))
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    assert_eq!(again.body["released"], json!([]));
    let still = take(world, &team.alice, hook, json!({})).await;
    assert_eq!(
        took(&still),
        ("worktree1".to_owned(), false),
        "the session stays registered"
    );
}

#[tokio::test]
async fn an_admin_ends_a_members_hold_and_the_member_reads_why() {
    let team = team().await;
    let world = &team.world;
    let [hook, _, desk] = team.sessions;
    take(world, &team.bob, desk, json!({"agent": "agent-9"})).await;

    let forced = world
        .post(
            &team.alice,
            "/v1/admin/worktrees/force-release",
            json!({"session_id": hook, "member": "bob", "host": "desktop",
                   "worktree": "worktree1", "reason": "its agent is stuck in a loop"}),
        )
        .await;
    assert_eq!(forced.status, StatusCode::OK, "{}", forced.body);
    assert_eq!(
        forced.body["released"],
        json!([{"member": "bob", "host": "desktop", "worktree": "worktree1", "holder": "agent",
                "purpose": "fix the invoice totals", "item": null,
                "since": "2026-03-02T09:00:00Z"}])
    );
    let thread = forced.body["thread_id"].as_u64().unwrap();
    let told = world.get(&team.bob, &format!("/v1/threads/{thread}")).await;
    let text = told.body["messages"][0]["body"].as_str().unwrap();
    assert!(
        text.contains("worktree1 on desktop") && text.ends_with("its agent is stuck in a loop"),
        "{text}"
    );
    assert_eq!(
        world.get(&team.bob, "/v1/worktrees").await.body["pools"],
        json!([])
    );

    take(world, &team.alice, hook, json!({})).await;
    let own = world
        .post(
            &team.alice,
            "/v1/admin/worktrees/force-release",
            json!({"session_id": hook, "member": "alice", "host": "laptop",
                   "worktree": "worktree1", "reason": "cleaning up"}),
        )
        .await;
    assert_eq!(own.status, StatusCode::OK, "{}", own.body);
    assert_eq!(own.body["thread_id"], Value::Null);
}

#[tokio::test]
async fn removing_a_member_ends_their_worktree_holds() {
    let team = team().await;
    let world = &team.world;
    let desk = team.sessions[2];
    take(world, &team.bob, desk, json!({"agent": "agent-9"})).await;
    take(world, &team.bob, desk, json!({})).await;
    let removed = world.delete(&team.alice, "/v1/admin/members/bob").await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
    assert_eq!(removed.body["worktrees"], 2);
    let board = world.get(&team.alice, "/v1/worktrees").await;
    assert_eq!(board.body["board"], json!([]));
}
