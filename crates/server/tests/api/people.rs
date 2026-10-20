//! Escalations, members leaving, deleted conversations and the retention through the router: what
//! dibs writes is from dibs and counts toward no limit, an escalation stands until an arbiter
//! acknowledges it, a removal releases everything at once, and a cursor behind what the retention
//! removed is refused.

use axum::http::StatusCode;
use serde_json::{Value, json};
use time::Duration;

use dibs_domain::member::Role;

use crate::support::{Person, World, assert_error};

/// alice (admin) holds `billing` on an item; bob and carol are members; carol is the arbiter; a
/// high or urgent message may be sent once an hour, and one slot is declared.
struct Team {
    world: World,
    alice: Person,
    bob: Person,
    carol: Person,
    item: u64,
    alice_session: u64,
    bob_session: u64,
}

impl Team {
    async fn new() -> Self {
        let world = World::new().await;
        let alice = world.project("acme", "alice").await;
        let bob = world.member(&alice, "bob", Role::Member).await;
        let carol = world.member(&alice, "carol", Role::Member).await;
        world
            .settings(
                &alice,
                json!({
                    "raised_per_hour": 1,
                    "arbiters": ["carol"],
                    "areas": [{"name": "billing", "paths": ["src/billing/**"]}],
                    "slots": [{"name": "deploy:prod", "max_hold": "30m"}]
                }),
            )
            .await;
        let alice_session = world.session(&alice, "laptop").await;
        let bob_session = world.session(&bob, "desk").await;
        let item = world.item(&alice, Some("gh:acme/api#7"), "Invoices").await;
        let held = world
            .post(
                &alice,
                &format!("/v1/items/{item}/claim"),
                json!({"session_id": alice_session, "areas": ["billing"]}),
            )
            .await;
        assert_eq!(held.status, StatusCode::OK, "{}", held.body);
        Self {
            world,
            alice,
            bob,
            carol,
            item,
            alice_session,
            bob_session,
        }
    }

    async fn escalations(&self, who: &Person) -> Vec<Value> {
        let status = self.world.get(who, "/v1/status").await;
        assert_eq!(status.status, StatusCode::OK, "{}", status.body);
        status.body["escalations"].as_array().unwrap().clone()
    }
}

#[tokio::test]
async fn an_escalation_reaches_the_arbiter_and_the_holder_and_stands_until_the_arbiter_acks() {
    let team = Team::new().await;
    let world = &team.world;
    let sent = world
        .post(
            &team.bob,
            "/v1/escalations",
            json!({"item": team.item, "reason": "alice and I both need billing today"}),
        )
        .await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.body);
    let escalation = &sent.body["escalation"];
    assert_eq!(escalation["arbiters"], json!(["carol"]));
    assert_eq!(escalation["reached"], json!(["alice", "carol"]));
    let thread = escalation["thread_id"].as_u64().unwrap();
    let message = escalation["message_id"].as_u64().unwrap();

    let read = world
        .get(&team.carol, &format!("/v1/threads/{thread}"))
        .await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    let first = &read.body["messages"][0];
    assert_eq!(first["author"], "dibs");
    assert_eq!(first["system"], true);
    assert_eq!(first["label"], "dibs");
    assert_eq!(first["importance"], "urgent");
    assert_eq!(first["ack_required"], true);
    assert_eq!(read.body["thread"]["author"], "bob");
    assert_eq!(read.body["thread"]["system"], false);
    assert!(
        first["body"]
            .as_str()
            .unwrap()
            .ends_with("Why: alice and I both need billing today")
    );

    for who in [&team.carol, &team.alice, &team.bob] {
        let shown = team.escalations(who).await;
        assert_eq!(shown.len(), 1, "{}", who.actor.member);
        assert_eq!(shown[0]["by"], "bob");
        assert_eq!(shown[0]["subject"], json!({"item": team.item}));
        assert_eq!(shown[0]["message_id"], message);
    }
    let held = world
        .post(
            &team.alice,
            &format!("/v1/messages/{message}/ack"),
            json!({}),
        )
        .await;
    assert_eq!(held.status, StatusCode::CREATED, "{}", held.body);
    assert_eq!(
        team.escalations(&team.carol).await.len(),
        1,
        "the holder settles nothing"
    );
    let settled = world
        .post(
            &team.carol,
            &format!("/v1/messages/{message}/ack"),
            json!({}),
        )
        .await;
    assert_eq!(settled.status, StatusCode::CREATED, "{}", settled.body);
    assert!(team.escalations(&team.carol).await.is_empty());
    assert!(team.escalations(&team.bob).await.is_empty());

    let events = world.get(&team.alice, "/v1/events").await;
    let escalated: Vec<&Value> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "escalated")
        .collect();
    assert_eq!(escalated.len(), 1);
    assert_eq!(
        escalated[0]["payload"],
        json!({"subject": {"item": team.item}, "by": team.bob.actor.member.get()})
    );
    let posted = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "message_posted" && event["payload"]["message"] == message)
        .unwrap();
    assert_eq!(
        posted["payload"]["author"],
        Value::Null,
        "dibs is no member"
    );
}

#[tokio::test]
async fn what_dibs_writes_counts_toward_nobodys_limit() {
    let team = Team::new().await;
    let world = &team.world;
    let urgent = |subject: &str| {
        json!({"subject": subject, "to": ["alice"], "label": "desk/mcp", "body": "Now, please.",
               "importance": "urgent"})
    };
    let first = world
        .post(&team.bob, "/v1/threads", urgent("Billing first"))
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    let second = world
        .post(&team.bob, "/v1/threads", urgent("Billing again"))
        .await;
    assert_error(&second, StatusCode::TOO_MANY_REQUESTS, "importance_limited");
    let slot = json!({"slot": "deploy:prod", "reason": "who deploys first?"});
    let escalated = world.post(&team.bob, "/v1/escalations", slot.clone()).await;
    assert_eq!(escalated.status, StatusCode::CREATED, "{}", escalated.body);
    let item = world
        .post(
            &team.bob,
            "/v1/escalations",
            json!({"item": team.item, "reason": "who goes first?"}),
        )
        .await;
    assert_eq!(item.status, StatusCode::CREATED, "{}", item.body);
    let again = world.post(&team.bob, "/v1/escalations", slot).await;
    assert_error(&again, StatusCode::CONFLICT, "already_escalated");
    assert_eq!(
        again.body["error"]["details"]["thread_id"], escalated.body["escalation"]["thread_id"],
        "it names the escalation that stands"
    );
}

#[tokio::test]
async fn an_escalation_names_one_subject_a_reason_and_needs_another_arbiter() {
    let team = Team::new().await;
    let world = &team.world;
    for body in [
        json!({"reason": "?"}),
        json!({"item": team.item, "slot": "deploy:prod", "reason": "?"}),
        json!({"item": team.item}),
        json!({"item": team.item, "reason": " "}),
    ] {
        let refused = world.post(&team.bob, "/v1/escalations", body).await;
        assert_error(
            &refused,
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
        );
    }
    let lonely = world
        .post(
            &team.carol,
            "/v1/escalations",
            json!({"item": team.item, "reason": "I am the only arbiter"}),
        )
        .await;
    assert_error(&lonely, StatusCode::CONFLICT, "no_arbiters");
    let ghost = world
        .post(
            &team.bob,
            "/v1/escalations",
            json!({"slot": "ghost", "reason": "?"}),
        )
        .await;
    assert_error(&ghost, StatusCode::NOT_FOUND, "unknown_slot");
    let bot = world.member(&team.alice, "board", Role::Importer).await;
    let refused = world
        .post(
            &bot,
            "/v1/escalations",
            json!({"item": team.item, "reason": "?"}),
        )
        .await;
    assert_error(&refused, StatusCode::FORBIDDEN, "forbidden");
}

#[tokio::test]
async fn removing_a_member_releases_what_they_held_at_once_and_tells_the_project() {
    let team = Team::new().await;
    let world = &team.world;
    let took = world
        .post(
            &team.bob,
            "/v1/slots/deploy:prod/take",
            json!({"session_id": team.bob_session, "purpose": "ship 2.4"}),
        )
        .await;
    assert_eq!(took.status, StatusCode::OK, "{}", took.body);
    let queued = world
        .post(
            &team.alice,
            "/v1/slots/deploy:prod/take",
            json!({"session_id": team.alice_session, "purpose": "ship 2.5"}),
        )
        .await;
    assert_eq!(queued.body["outcome"], "queued", "{}", queued.body);
    let bobs = world.item(&team.bob, None, "Search ranking").await;
    let claimed = world
        .post(
            &team.bob,
            &format!("/v1/items/{bobs}/claim"),
            json!({"session_id": team.bob_session}),
        )
        .await;
    assert_eq!(claimed.status, StatusCode::OK, "{}", claimed.body);

    assert_error(
        &world.delete(&team.carol, "/v1/admin/members/bob").await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_error(
        &world.delete(&team.alice, "/v1/admin/members/alice").await,
        StatusCode::CONFLICT,
        "self_removal",
    );
    let removed = world.delete(&team.alice, "/v1/admin/members/bob").await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
    assert_eq!(removed.body["member"]["handle"], "bob");
    assert_eq!(removed.body["removed_at"], "2026-03-02T09:00:00Z");
    assert_eq!(removed.body["released"][0]["id"], bobs);
    assert_eq!(removed.body["slots"]["held"], json!(["deploy:prod"]));
    assert_eq!(removed.body["tokens_revoked"], 1);

    assert_error(
        &world.get(&team.bob, "/v1/me").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    let members = world.get(&team.alice, "/v1/members").await;
    let handles: Vec<&str> = members.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["handle"].as_str().unwrap())
        .collect();
    assert_eq!(handles, ["alice", "carol"]);
    let item = world.get(&team.alice, &format!("/v1/items/{bobs}")).await;
    assert_eq!(item.body["item"]["status"], "open");
    assert_eq!(item.body["claim"], Value::Null);
    let slot = world.get(&team.alice, "/v1/slots/deploy:prod").await;
    assert_eq!(slot.body["slot"]["hold"], Value::Null, "{}", slot.body);
    assert_eq!(slot.body["slot"]["offer"]["member"], "alice", "offered on");

    let inbox = world.get(&team.carol, "/v1/inbox").await;
    let told = inbox.body["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|summary| summary["thread"]["subject"] == "bob left the project")
        .unwrap();
    let thread = told["thread"]["id"].as_u64().unwrap();
    let read = world
        .get(&team.carol, &format!("/v1/threads/{thread}"))
        .await;
    assert_eq!(read.body["thread"]["to_project"], true);
    assert_eq!(read.body["messages"][0]["system"], true);
    let text = read.body["messages"][0]["body"].as_str().unwrap();
    assert!(
        text.starts_with("alice, an admin, removed bob from this project."),
        "{text}"
    );
    assert!(text.contains("deploy:prod"), "{text}");

    let events = world.get(&team.alice, "/v1/events").await;
    let removal = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "member_removed")
        .unwrap()
        .clone();
    assert_eq!(
        removal["members"][team.bob.actor.member.get().to_string()],
        "bob"
    );
    assert_eq!(
        removal["members"][team.alice.actor.member.get().to_string()],
        "alice"
    );
    assert_error(
        &world.delete(&team.alice, "/v1/admin/members/bob").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn an_admin_deletes_a_conversation_and_the_log_keeps_its_id_alone() {
    let team = Team::new().await;
    let world = &team.world;
    let thread = world
        .thread(
            &team.bob,
            json!(["carol"]),
            "Oops",
            "the password is hunter2",
        )
        .await;
    assert_error(
        &world
            .delete(&team.bob, &format!("/v1/admin/threads/{thread}"))
            .await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let deleted = world
        .delete(&team.alice, &format!("/v1/admin/threads/{thread}"))
        .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert_eq!(deleted.body, json!({"thread_id": thread, "messages": 1}));
    assert_error(
        &world
            .get(&team.carol, &format!("/v1/threads/{thread}"))
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let events = world.get(&team.carol, "/v1/events").await;
    let kinds: Vec<&str> = events.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert!(!kinds.contains(&"message_posted"), "{kinds:?}");
    assert_eq!(kinds.last(), Some(&"thread_deleted"));
    assert!(!events.body.to_string().contains("hunter2"));
    assert_error(
        &world
            .delete(&team.alice, &format!("/v1/admin/threads/{thread}"))
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn a_cursor_behind_what_the_retention_removed_is_refused_and_reading_again_works() {
    let team = Team::new().await;
    let world = &team.world;
    let first = world.get(&team.alice, "/v1/events?limit=1").await;
    let early = first.body["next"].as_u64().unwrap();
    world.fakes.clock.advance(Duration::days(91));
    let report = world.app.prune.execute().await.unwrap();
    assert_eq!(report.pruned.len(), 1);
    let behind = world
        .get(&team.alice, &format!("/v1/events?after={early}"))
        .await;
    assert_error(&behind, StatusCode::GONE, "cursor_expired");
    assert!(behind.body["error"]["details"]["pruned_through"].is_u64());
    let again = world.get(&team.alice, "/v1/events?after=0").await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.body);
    let kinds: Vec<&str> = again.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["pruned"]);
}
