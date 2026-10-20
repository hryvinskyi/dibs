//! The pilot's figures: day reports on the heartbeat, feedback, pull requests an integration reports
//! as conflicting, and `GET /v1/stats`, with who may see each.

use axum::http::StatusCode;
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{World, assert_error};

/// The id of bob's machine.
const MACHINE: &str = "0123456789abcdef0123456789abcdef";

fn report(day: &str, counts: Value) -> Value {
    json!({
        "day": day,
        "counts": counts,
        "durations": [
            {"agent": "neutral", "event": "before_tool", "buckets": [40, 10, 0, 0, 0, 0, 0]}
        ]
    })
}

#[tokio::test]
async fn a_heartbeat_keeps_its_reports_and_a_resend_replaces_and_an_unchanged_one_appends_nothing()
{
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let session = world.session(&bob, "laptop").await;
    let beat = |reports: Value| {
        let world = &world;
        let bob = &bob;
        async move {
            world
                .post(
                    bob,
                    "/v1/heartbeat",
                    json!({"session_id": session, "machine": MACHINE, "reports": reports}),
                )
                .await
        }
    };
    let first = beat(json!([report(
        "2026-03-02",
        json!({"held_by": 2, "a_kind_of_tomorrow": 5})
    )]))
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["reported"], json!(["2026-03-02"]));
    let again = beat(json!([report("2026-03-02", json!({"held_by": 2}))])).await;
    assert_eq!(
        again.body["reported"],
        json!([]),
        "an unknown kind was left out, so this is the same report"
    );
    let resent = beat(json!([report("2026-03-02", json!({"held_by": 3}))])).await;
    assert_eq!(resent.body["reported"], json!(["2026-03-02"]));
    let plain = world
        .post(&bob, "/v1/heartbeat", json!({"session_id": session}))
        .await;
    assert_eq!(plain.body["reported"], json!([]));

    let events = world.get(&alice, "/v1/events").await;
    let reported: Vec<&Value> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "hook_reported")
        .collect();
    assert_eq!(reported.len(), 2);
    assert_eq!(
        reported[1]["payload"],
        json!({
            "member": bob.actor.member.get(),
            "host": "laptop",
            "day": "2026-03-02",
            "counts": {"held_by": 3},
            "durations": [
                {"agent": "neutral", "event": "before_tool", "buckets": [40, 10, 0, 0, 0, 0, 0]}
            ]
        })
    );
}

#[tokio::test]
async fn a_report_the_server_cannot_read_is_left_out_and_the_heartbeat_still_keeps_the_session_alive()
 {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let session = world.session(&bob, "laptop").await;
    let item = world.item(&bob, Some("gh:acme/widgets#1"), "Work").await;
    let claimed = world
        .post(
            &bob,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": session}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    let beat = |body: Value| {
        let world = &world;
        let bob = &bob;
        async move { world.post(bob, "/v1/heartbeat", body).await }
    };
    let mixed = beat(json!({
        "session_id": session,
        "machine": MACHINE,
        "reports": [
            {"counts": {"held_by": 1}},
            report("2026-3-2", json!({"held_by": 1})),
            {"day": "2026-03-01", "counts": ["held_by"]},
            {"day": "2026-03-02", "counts": {"held_by": 2, "held_by_in_tomorrows_client": 4, "frozen": "many"},
             "durations": [
                {"agent": "neutral", "event": "before_tool", "buckets": [1, 2]},
                {"agent": "neutral", "event": "Before Tool", "buckets": [1, 0, 0, 0, 0, 0, 0]},
                {"agent": "neutral", "event": "after_tool", "buckets": [3, 0, 0, 0, 0, 0, 0], "p99": 4}
             ]},
            report("2026-03-02", json!({"held_by": 9})),
            {"day": "2026-02-28", "counts": {}, "durations": {"not": "a list"}},
            "a string"
        ]
    }))
    .await;
    assert_eq!(mixed.status, StatusCode::OK, "{}", mixed.body);
    assert_eq!(
        mixed.body["refreshed"],
        json!([item]),
        "the session was kept alive"
    );
    assert_eq!(mixed.body["reports_accepted"], json!(["2026-03-02"]));
    let events = world.get(&alice, "/v1/events").await;
    let kept: Vec<&Value> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "hook_reported")
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["payload"]["counts"], json!({"held_by": 2}));
    assert_eq!(
        kept[0]["payload"]["durations"],
        json!([{"agent": "neutral", "event": "after_tool", "buckets": [3, 0, 0, 0, 0, 0, 0]}]),
        "what of the report could be read is kept"
    );

    for body in [
        json!({"session_id": session, "reports": [report("2026-03-02", json!({"held_by": 7}))]}),
        json!({"session_id": session, "machine": "laptop", "reports": [report("2026-03-02", json!({}))]}),
        json!({"session_id": session, "machine": MACHINE, "reports": {"2026-03-02": {}}}),
    ] {
        let answer = beat(body).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        assert_eq!(answer.body["refreshed"], json!([item]));
        assert_eq!(answer.body["reports_accepted"], json!([]));
    }
}

#[tokio::test]
async fn a_renamed_machine_keeps_one_report_a_day_and_two_machines_of_one_name_keep_two() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let first = world.session(&bob, "laptop").await;
    let renamed = world.session(&bob, "laptop-2").await;
    let twin = world
        .post(
            &bob,
            "/v1/sessions",
            json!({"host": "laptop", "label": "laptop/other-machine", "agent": "agent"}),
        )
        .await
        .body["session_id"]
        .as_u64()
        .unwrap();
    for (session, machine, held) in [
        (first, MACHINE, 1),
        (renamed, MACHINE, 2),
        (first, MACHINE, 2),
        (twin, "fedcba9876543210fedcba9876543210", 4),
    ] {
        let answer = world
            .post(
                &bob,
                "/v1/heartbeat",
                json!({"session_id": session, "machine": machine,
                       "reports": [report("2026-03-02", json!({"held_by": held}))]}),
            )
            .await;
        assert_eq!(answer.body["reports_accepted"], json!(["2026-03-02"]));
    }
    let stats = world.get(&alice, "/v1/stats").await;
    assert_eq!(
        stats.body["reported"]["held_by"], 6,
        "the renamed machine's day replaced its first report, the other machine of the first one's name kept its own: {}",
        stats.body
    );
}

#[tokio::test]
async fn reports_and_feedback_are_seen_by_their_member_and_admins_only() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let carol = world.member(&alice, "carol", Role::Member).await;
    let board = world.member(&alice, "board", Role::Importer).await;
    let session = world.session(&bob, "laptop").await;
    world
        .post(
            &bob,
            "/v1/heartbeat",
            json!({"session_id": session, "machine": MACHINE, "reports": [report("2026-03-02", json!({"frozen": 1}))]}),
        )
        .await;
    let given = world
        .post(
            &bob,
            "/v1/feedback",
            json!({"kind": "false_refusal", "refusal": "r-7f3a", "tally": "held_by", "reason": "the area was free"}),
        )
        .await;
    assert_eq!(given.status, StatusCode::CREATED, "{}", given.body);
    let private = |who: &crate::support::Person| {
        let world = &world;
        let who = who.clone();
        async move {
            let events = world.get(&who, "/v1/events").await;
            events.body["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| {
                    event["kind"] == "hook_reported" || event["kind"] == "feedback_given"
                })
                .count()
        }
    };
    assert_eq!(private(&alice).await, 2);
    assert_eq!(private(&bob).await, 2);
    assert_eq!(private(&carol).await, 0);
    assert_eq!(private(&board).await, 0);
}

#[tokio::test]
async fn feedback_names_a_refusal_or_a_conflicts_area_and_never_keeps_the_path() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    world
        .settings(
            &alice,
            json!({"areas": [{"name": "billing", "paths": ["packages/billing/**"]}]}),
        )
        .await;
    let conflict = world
        .post(
            &alice,
            "/v1/feedback",
            json!({"kind": "conflict", "repo": "default", "path": "packages/billing/private-pricing.rs", "reason": "rebased over bob's rounding"}),
        )
        .await;
    assert_eq!(conflict.status, StatusCode::CREATED, "{}", conflict.body);
    assert_eq!(
        conflict.body["feedback"],
        json!({"kind": "conflict", "repo": "default", "area": "default:billing"})
    );
    let refusal = world
        .post(
            &alice,
            "/v1/feedback",
            json!({"kind": "false_refusal", "refusal": "r-00ff", "tally": "a_kind_of_tomorrow", "reason": "free"}),
        )
        .await;
    assert_eq!(
        refusal.body["feedback"],
        json!({"kind": "false_refusal", "refusal": "r-00ff", "tally": null})
    );
    let events = world.get(&alice, "/v1/events").await;
    assert!(
        !events.body.to_string().contains("private-pricing"),
        "{}",
        events.body
    );
    for (body, code, field) in [
        (
            json!({"refusal": "r-00ff", "reason": "x"}),
            "invalid_request",
            Some("kind"),
        ),
        (
            json!({"kind": "praise", "reason": "x"}),
            "invalid_request",
            Some("kind"),
        ),
        (
            json!({"kind": "false_refusal", "refusal": "7f3a", "reason": "x"}),
            "invalid_request",
            Some("refusal"),
        ),
        (
            json!({"kind": "false_refusal", "refusal": "r-7f3a", "reason": " "}),
            "invalid_request",
            Some("reason"),
        ),
        (
            json!({"kind": "conflict", "repo": "elsewhere", "path": "a.rs", "reason": "x"}),
            "unknown_repo",
            None,
        ),
    ] {
        let refused = world.post(&alice, "/v1/feedback", body).await;
        assert_eq!(refused.body["error"]["code"], code, "{}", refused.body);
        if let Some(field) = field {
            assert_eq!(refused.body["error"]["details"]["field"], field);
        }
    }
}

#[tokio::test]
async fn an_integration_reports_a_conflicting_pull_request_once_and_its_end() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let board = world.member(&alice, "board", Role::Importer).await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let state = |mergeable: bool| json!({"pr": "acme/widgets#7", "mergeable": mergeable});
    let mut recorded = Vec::new();
    for mergeable in [true, false, false, true] {
        let answer = world.post(&board, "/v1/prs/state", state(mergeable)).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        recorded.push(answer.body["recorded"].as_bool().unwrap());
    }
    assert_eq!(recorded, [false, true, false, true]);
    let refused = world.post(&bob, "/v1/prs/state", state(false)).await;
    assert_error(&refused, StatusCode::FORBIDDEN, "forbidden");
    let missing = world
        .post(&board, "/v1/prs/state", json!({"pr": "acme/widgets#7"}))
        .await;
    assert_error(
        &missing,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
}

#[tokio::test]
async fn an_admin_reads_the_pilots_figures_and_nobody_else_does() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let board = world.member(&alice, "board", Role::Importer).await;
    let session = world.session(&bob, "laptop").await;
    let item = world.item(&alice, Some("gh:acme/widgets#1"), "Work").await;
    let claimed = world
        .post(
            &bob,
            &format!("/v1/items/{item}/claim"),
            json!({"session_id": session}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    world
        .post(
            &bob,
            "/v1/heartbeat",
            json!({"session_id": session, "machine": MACHINE, "reports": [
                report("2026-03-02", json!({"held_by": 2, "unreachable": 1, "switched_off": 1})),
                report("2026-02-27", json!({"needs_slot": 1})),
            ]}),
        )
        .await;
    world
        .post(
            &bob,
            "/v1/feedback",
            json!({"kind": "false_refusal", "refusal": "r-7f3a", "tally": "held_by", "reason": "it was free"}),
        )
        .await;
    world
        .post(
            &board,
            "/v1/prs/state",
            json!({"pr": "acme/widgets#7", "mergeable": false}),
        )
        .await;

    let stats = world.get(&alice, "/v1/stats").await;
    assert_eq!(stats.status, StatusCode::OK, "{}", stats.body);
    let body = &stats.body;
    assert_eq!(body["from"], "2026-02-24");
    assert_eq!(body["to"], "2026-03-02");
    assert_eq!(body["claims"], 1);
    assert_eq!(body["truncated"], false);
    assert_eq!(body["reported"]["held_by"], 2);
    assert_eq!(body["reported"]["needs_slot"], 1);
    assert_eq!(body["reported"]["frozen"], 0, "every kind is named");
    assert_eq!(body["reported"]["switched_off"], 1);
    assert_eq!(
        body["reported_days"],
        json!([
            {"member": "bob", "day": "2026-02-27", "counts": {"needs_slot": 1}},
            {"member": "bob", "day": "2026-03-02", "counts": {"held_by": 2, "unreachable": 1, "switched_off": 1}}
        ])
    );
    assert_eq!(
        body["hook_durations"],
        json!([{"agent": "neutral", "event": "before_tool", "runs": 100, "p95_ms": 30, "buckets": [80, 20, 0, 0, 0, 0, 0]}])
    );
    assert_eq!(body["hook_durations_by_member"][0]["member"], "bob");
    assert_eq!(body["feedback"]["false_refusals"], 1);
    assert_eq!(body["feedback"]["entries"][0]["member"], "bob");
    assert_eq!(body["feedback"]["entries"][0]["kind"], "false_refusal");
    assert_eq!(body["feedback"]["entries"][0]["reason"], "it was free");
    assert_eq!(
        body["conflicting_prs"],
        json!({"reports": 1, "prs": ["acme/widgets#7"]})
    );
    assert_eq!(
        body["slot_waits"],
        json!({"count": 0, "median_seconds": null, "max_seconds": null})
    );

    let one_day = world
        .get(&alice, "/v1/stats?from=2026-02-27&to=2026-02-27")
        .await;
    assert_eq!(one_day.body["reported"]["needs_slot"], 1);
    assert_eq!(one_day.body["reported"]["held_by"], 0);
    assert_eq!(one_day.body["claims"], 0);

    for path in [
        "/v1/stats?from=2026-03-03&to=2026-03-02",
        "/v1/stats?from=2025-01-01&to=2026-03-02",
        "/v1/stats?to=yesterday",
    ] {
        let refused = world.get(&alice, path).await;
        assert_error(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        );
    }
    for who in [&bob, &board] {
        let refused = world.get(who, "/v1/stats").await;
        assert_error(&refused, StatusCode::FORBIDDEN, "forbidden");
    }
}
