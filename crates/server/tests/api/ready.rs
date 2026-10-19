//! The work graph through the router: an importer's batch (`PUT /v1/items`), the ready list
//! (`GET /v1/ready`), a person's dependencies (`POST`/`DELETE /v1/items/{id}/dependencies`), and a
//! claim on a blocked item.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error};

struct Board {
    world: World,
    alice: Person,
    bob: Person,
    bot: Person,
}

impl Board {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        world
            .settings(
                &alice,
                json!({"areas": [
                    {"name": "billing", "paths": ["packages/billing/**"]},
                    {"name": "search", "paths": ["packages/search/**"]}
                ]}),
            )
            .await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let bot = world.member(&alice, "board", Role::Importer).await;
        Self {
            world,
            alice,
            bob,
            bot,
        }
    }

    async fn import(&self, body: Value) -> Value {
        let answer = self.world.put(&self.bot, "/v1/items", body).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        answer.body
    }

    async fn events(&self) -> usize {
        self.world.get(&self.alice, "/v1/events").await.body["events"]
            .as_array()
            .unwrap()
            .len()
    }

    async fn id(&self, reference: &str) -> u64 {
        let answer = self
            .world
            .get(&self.alice, &format!("/v1/items?ref={}", encode(reference)))
            .await;
        answer.body["items"][0]["id"].as_u64().unwrap()
    }
}

fn encode(text: &str) -> String {
    text.replace('#', "%23")
        .replace('/', "%2F")
        .replace(':', "%3A")
}

fn two_items() -> Value {
    json!({
        "source": "board",
        "items": [
            {"ref": "gh:Acme/Web#1", "title": "Invoices round wrong", "priority": 1,
             "areas_hint": ["billing", "payroll"], "source_status": "Todo",
             "blocked_by": ["gh:acme/api#7"]},
            {"ref": "gh:acme/web#2", "title": "Search is slow"},
            {"ref": "gh:acme/api#7", "title": "Rate table", "blocker": true}
        ]
    })
}

#[tokio::test]
async fn a_batch_answers_per_item_and_a_repeat_changes_nothing() {
    let board = Board::new().await;
    let first = board.import(two_items()).await;
    let outcomes: Vec<&str> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["created", "created", "created"]);
    assert_eq!(first["items"][0]["ref"], "gh:acme/web#1");
    assert_eq!(
        first["items"][0]["dropped_hints"],
        json!([{"area": "payroll", "reason": "undeclared"}])
    );
    assert_eq!(first["edges_added"], 1);
    assert_eq!(first["refused_edges"], json!([]));
    let item = board
        .world
        .get(
            &board.alice,
            &format!("/v1/items/{}", board.id("gh:acme/web#1").await),
        )
        .await;
    assert_eq!(item.body["item"]["priority"], 1);
    assert_eq!(item.body["item"]["areas_hint"], json!(["default:billing"]));
    assert_eq!(item.body["item"]["source_status"], "Todo");
    assert_eq!(item.body["item"]["source"], "board");
    assert_eq!(item.body["item"]["closed"], false);

    let events = board.events().await;
    let again = board.import(two_items()).await;
    assert!(
        again["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["outcome"] == "unchanged"),
        "{again}"
    );
    assert_eq!(
        board.events().await,
        events,
        "a repeated batch appends no event"
    );
}

#[tokio::test]
async fn a_batch_reports_edges_and_items_it_did_not_record() {
    let board = Board::new().await;
    let answer = board
        .import(json!({
            "source": "board",
            "items": [
                {"ref": "gh:acme/web#1", "title": "One", "blocked_by": ["gh:acme/api#404", "gh:acme/web#1"]},
                {"ref": "gh:acme/web#9", "title": "Long gone", "closed": true}
            ]
        }))
        .await;
    assert_eq!(answer["items"][1]["outcome"], "skipped_closed");
    assert_eq!(answer["items"][1]["id"], Value::Null);
    let reasons: Vec<&str> = answer["refused_edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|edge| edge["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, ["unknown_blocker", "self_dependency"]);
    assert_eq!(answer["refused_edges"][0]["blocked_by"], "gh:acme/api#404");
    assert!(
        answer["refused_edges"][0]["message"]
            .as_str()
            .unwrap()
            .ends_with('.')
    );
}

#[tokio::test]
async fn a_complete_batch_closes_what_it_left_out() {
    let board = Board::new().await;
    board.import(two_items()).await;
    let answer = board
        .import(json!({"source": "board", "complete": true, "items": [
            {"ref": "gh:acme/web#1", "title": "Invoices round wrong", "priority": 1,
             "areas_hint": ["billing", "payroll"], "source_status": "Todo",
             "blocked_by": ["gh:acme/api#7"]}
        ]}))
        .await;
    assert_eq!(answer["closed"], json!([board.id("gh:acme/web#2").await]));
}

#[tokio::test]
async fn a_batch_is_refused_whole_for_its_shape_and_by_field_for_its_values() {
    let board = Board::new().await;
    let world = &board.world;
    let bad = world
        .put(
            &board.bot,
            "/v1/items",
            json!({"items": [{"ref": "gh:acme", "title": "x"}]}),
        )
        .await;
    assert_error(&bad, StatusCode::UNPROCESSABLE_ENTITY, "invalid_request");
    assert_eq!(bad.body["error"]["details"]["field"], "items[0].ref");
    let priority = world
        .put(
            &board.bot,
            "/v1/items",
            json!({"items": [{"ref": "gh:acme/web#1", "title": "x", "priority": 7}]}),
        )
        .await;
    assert_eq!(
        priority.body["error"]["details"]["field"],
        "items[0].priority"
    );
    let no_source = world
        .put(
            &board.bot,
            "/v1/items",
            json!({"complete": true, "items": []}),
        )
        .await;
    assert_error(
        &no_source,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_batch",
    );
    let twice = world
        .put(
            &board.bot,
            "/v1/items",
            json!({"items": [{"ref": "gh:acme/web#1", "title": "a"}, {"ref": "gh:Acme/web#1", "title": "b"}]}),
        )
        .await;
    assert_error(&twice, StatusCode::UNPROCESSABLE_ENTITY, "duplicate_ref");
    assert_eq!(twice.body["error"]["details"]["ref"], "gh:acme/web#1");
    let unknown_field = world
        .put(
            &board.bot,
            "/v1/items",
            json!({"items": [], "colour": "red"}),
        )
        .await;
    assert_error(&unknown_field, StatusCode::BAD_REQUEST, "invalid_json");
}

#[tokio::test]
async fn the_largest_batch_fits_and_one_more_item_is_refused() {
    let board = Board::new().await;
    let title = "t".repeat(200);
    let items = |count: usize| -> Value {
        (0..count)
            .map(|n| {
                json!({"ref": format!("gh:acme/widgets#{n}"), "title": title,
                            "areas_hint": ["billing"], "blocked_by": []})
            })
            .collect()
    };
    let full = board
        .world
        .put(
            &board.bot,
            "/v1/items",
            json!({"source": "board", "items": items(500)}),
        )
        .await;
    assert_eq!(
        full.status,
        StatusCode::OK,
        "a batch of 500 is above the default body limit"
    );
    let over = board
        .world
        .put(
            &board.bot,
            "/v1/items",
            json!({"source": "board", "items": items(501)}),
        )
        .await;
    assert_error(&over, StatusCode::UNPROCESSABLE_ENTITY, "batch_too_large");
    assert_eq!(over.body["error"]["details"]["max"], 500);
}

#[tokio::test]
async fn only_an_admin_and_the_importer_send_a_batch() {
    let board = Board::new().await;
    let refused = board.world.put(&board.bob, "/v1/items", json!({})).await;
    assert_error(&refused, StatusCode::FORBIDDEN, "forbidden");
    let admin = board
        .world
        .put(
            &board.alice,
            "/v1/items",
            json!({"items": [{"ref": "gh:acme/web#1", "title": "x"}]}),
        )
        .await;
    assert_eq!(admin.status, StatusCode::OK, "{}", admin.body);
}

#[tokio::test]
async fn the_ready_list_names_its_order_its_links_and_why_the_rest_waits() {
    let board = Board::new().await;
    board.import(two_items()).await;
    let ready = board.world.get(&board.bob, "/v1/ready").await;
    assert_eq!(ready.status, StatusCode::OK, "{}", ready.body);
    let refs: Vec<&str> = ready.body["ready"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["item"]["ref"].as_str().unwrap())
        .collect();
    assert_eq!(
        refs,
        ["gh:acme/web#2", "gh:acme/api#7"],
        "same priority and time: by id"
    );
    assert_eq!(
        ready.body["ready"][1]["link"],
        "https://github.com/acme/api/issues/7"
    );
    assert_eq!(ready.body["ready"][1]["stale_holder"], Value::Null);
    let excluded = &ready.body["excluded"][0];
    assert_eq!(excluded["item"]["ref"], "gh:acme/web#1");
    assert_eq!(excluded["reasons"][0]["reason"], "blocked_by");
    assert_eq!(excluded["reasons"][0]["items"][0]["ref"], "gh:acme/api#7");
    assert_eq!(excluded["reasons"][0]["items"][0]["title"], "Rate table");
    assert_eq!(
        excluded["reasons"][0]["items"][0]["link"],
        "https://github.com/acme/api/issues/7"
    );
    let bots = board.world.get(&board.bot, "/v1/ready").await;
    assert_eq!(bots.status, StatusCode::OK);
    let status = board.world.get(&board.bob, "/v1/status").await;
    assert_eq!(status.body["ready"], ready.body);
}

#[tokio::test]
async fn a_held_hinted_area_is_a_reason_naming_the_holder_and_their_item() {
    let board = Board::new().await;
    board
        .import(json!({"source": "board", "items": [
            {"ref": "gh:acme/web#3", "title": "Billing export", "areas_hint": ["billing"]}
        ]}))
        .await;
    let held = board
        .world
        .item(&board.alice, Some("gh:acme/web#8"), "Held")
        .await;
    let session = board.world.session(&board.alice, "laptop").await;
    let claimed = board
        .world
        .post(
            &board.alice,
            &format!("/v1/items/{held}/claim"),
            json!({"session_id": session, "areas": ["billing"]}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);
    let ready = board.world.get(&board.bob, "/v1/ready").await;
    let reasons = &ready.body["excluded"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["item"]["ref"] == "gh:acme/web#3")
        .unwrap()["reasons"];
    assert_eq!(
        reasons,
        &json!([{"reason": "area_held", "area": "default:billing", "holder": "alice",
                 "item": {"id": held, "ref": "gh:acme/web#8", "title": "Held",
                          "link": "https://github.com/acme/web/issues/8"}}])
    );
}

#[tokio::test]
async fn a_person_records_and_removes_a_dependency_and_a_cycle_is_refused() {
    let board = Board::new().await;
    let world = &board.world;
    let a = world.item(&board.bob, Some("gh:acme/web#1"), "A").await;
    let b = world.item(&board.bob, Some("gh:acme/web#2"), "B").await;
    let path = |item: u64| format!("/v1/items/{item}/dependencies");
    let added = world
        .post(&board.bob, &path(a), json!({"blocked_by": b}))
        .await;
    assert_eq!(added.status, StatusCode::OK, "{}", added.body);
    assert_eq!(added.body["changed"], true);
    assert_eq!(added.body["blocked_by"]["ref"], "gh:acme/web#2");
    let again = world
        .post(&board.bob, &path(a), json!({"blocked_by": b}))
        .await;
    assert_eq!(again.body["changed"], false);
    let cycle = world
        .post(&board.bob, &path(b), json!({"blocked_by": a}))
        .await;
    assert_error(&cycle, StatusCode::CONFLICT, "dependency_cycle");
    let itself = world
        .post(&board.bob, &path(a), json!({"blocked_by": a}))
        .await;
    assert_error(&itself, StatusCode::UNPROCESSABLE_ENTITY, "self_dependency");
    let missing = world.post(&board.bob, &path(a), json!({})).await;
    assert_error(
        &missing,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
    let unknown = world
        .post(&board.bob, &path(a), json!({"blocked_by": 999}))
        .await;
    assert_error(&unknown, StatusCode::NOT_FOUND, "not_found");

    let session = world.session(&board.bob, "laptop").await;
    let claim = world
        .post(
            &board.bob,
            &format!("/v1/items/{a}/claim"),
            json!({"session_id": session}),
        )
        .await;
    assert_eq!(claim.status, StatusCode::OK, "{}", claim.body);
    assert_eq!(claim.body["blocked_by"][0]["ref"], "gh:acme/web#2");
    assert_eq!(
        claim.body["blocked_by"][0]["link"],
        "https://github.com/acme/web/issues/2"
    );

    let removed = world
        .call(
            Method::DELETE,
            &format!("{}?blocked_by={b}", path(a)),
            Some(&board.bob.token),
            None,
        )
        .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
    assert_eq!(removed.body["changed"], true);
    let gone = world
        .call(
            Method::DELETE,
            &format!("{}?blocked_by={b}", path(a)),
            Some(&board.bob.token),
            None,
        )
        .await;
    assert_eq!(gone.body["changed"], false);
    let no_query = world.delete(&board.bob, &path(a)).await;
    assert_error(
        &no_query,
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
    );
}

#[tokio::test]
async fn a_held_area_refusal_names_the_holders_item_by_its_reference() {
    let board = Board::new().await;
    let world = &board.world;
    let held = world
        .item(&board.alice, Some("gh:acme/web#12"), "Held")
        .await;
    let wanted = world.item(&board.bob, None, "Wanted").await;
    let alice = world.session(&board.alice, "laptop").await;
    let bob = world.session(&board.bob, "desk").await;
    world
        .post(
            &board.alice,
            &format!("/v1/items/{held}/claim"),
            json!({"session_id": alice, "areas": ["billing"]}),
        )
        .await;
    let refused = world
        .post(
            &board.bob,
            &format!("/v1/items/{wanted}/claim"),
            json!({"session_id": bob, "areas": ["billing"]}),
        )
        .await;
    assert_error(&refused, StatusCode::CONFLICT, "area_held");
    assert_eq!(refused.body["error"]["details"]["item"], held);
    assert_eq!(
        refused.body["error"]["details"]["item_ref"],
        "gh:acme/web#12"
    );
    assert_eq!(refused.body["error"]["details"]["holder"], "alice");
}
