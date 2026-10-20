//! Slots: taking, waiting, the offer in the inbox, a run's heartbeat and release with its exit, a
//! takeover of a stalled hold and an admin's forced release, each answer parsed into its type.

use serde_json::json;
use time::Duration;

use dibs_client::ClientError;
use dibs_client::api::{
    ForceReleaseSlotRequest, HoldMode, InboxRequest, ReleaseSlotRequest, TakeOutcome,
    TakeOverSlotRequest, TakeSlotRequest,
};
use dibs_domain::ids::SessionId;
use dibs_domain::slot::{Liveness, Purpose, RunExit, SlotName};
use dibs_test_server::two_repositories;

use crate::support::{World, run, session};

fn world() -> World {
    let world = World::new();
    let mut settings = two_repositories();
    settings["slots"] = json!([
        {"name": "deploy:prod", "max_hold": "30m", "freezes": ["api:refs/heads/main"]},
        {"name": "migrate", "max_hold": "1h", "run_only": true}
    ]);
    world.server.settings(&world.alice, settings).unwrap();
    world
}

fn take(session: SessionId, mode: HoldMode) -> TakeSlotRequest {
    TakeSlotRequest {
        session_id: session,
        purpose: Purpose::parse("ship 2.4").unwrap(),
        item: None,
        mode,
        workflow: None,
        sha: None,
    }
}

fn slot(name: &str) -> SlotName {
    SlotName::parse(name).unwrap()
}

#[test]
fn a_slot_is_taken_queued_offered_and_taken_by_the_first_waiter() {
    let world = world();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let a = session(&alice, "laptop/cli").await.session_id;
        let b = session(&bob, "laptop/cli").await.session_id;
        let prod = slot("deploy:prod");
        let taken = alice
            .take_slot(&prod, &take(a, HoldMode::Session))
            .await
            .unwrap();
        assert_eq!(taken.outcome, TakeOutcome::Taken);
        let hold = taken.slot.hold.unwrap();
        assert_eq!(hold.holder.as_str(), "alice");
        assert_eq!(hold.liveness, Liveness::Session);
        assert_eq!(taken.slot.freezes[0].to_string(), "api:refs/heads/main");
        let queued = bob
            .take_slot(&prod, &take(b, HoldMode::Session))
            .await
            .unwrap();
        assert_eq!(
            (queued.outcome, queued.position),
            (TakeOutcome::Queued, Some(1))
        );
        assert_eq!(queued.slot.position_of(&"bob".parse().unwrap()), Some(1));
        let released = alice
            .release_slot(&prod, &ReleaseSlotRequest::default())
            .await
            .unwrap();
        assert_eq!(released.slot.offer.unwrap().member.as_str(), "bob");
        let inbox = bob.inbox(InboxRequest::default()).await.unwrap();
        assert_eq!(inbox.offers.len(), 1);
        assert_eq!(inbox.offers[0].slot, prod);
        let mine = bob
            .take_slot(&prod, &take(b, HoldMode::Session))
            .await
            .unwrap();
        assert_eq!(mine.outcome, TakeOutcome::Taken);
        let slots = alice.slots().await.unwrap();
        assert_eq!(slots.slots.len(), 2);
        assert!(slots.slots[1].run_only);
        let status = alice.status().await.unwrap();
        assert_eq!(
            status.slots[0].hold.as_ref().unwrap().holder.as_str(),
            "bob"
        );
        let refused = alice.leave_slot(&prod).await.unwrap_err();
        assert_eq!(refused.code(), "not_waiting");
        let unknown = alice.slot(&slot("ghost")).await.unwrap_err();
        assert_eq!(unknown.code(), "unknown_slot");
    });
}

#[test]
fn a_run_beats_releases_with_its_exit_and_learns_it_was_taken_over() {
    let world = world();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let a = session(&alice, "laptop/cli").await.session_id;
        let b = session(&bob, "laptop/cli").await.session_id;
        let migrate = slot("migrate");
        let took = alice
            .take_slot(&migrate, &take(a, HoldMode::Run))
            .await
            .unwrap();
        let run = took.run_id.unwrap();
        let beat = alice.slot_heartbeat(&migrate, run).await.unwrap();
        assert!(!beat.slot.hold.unwrap().stalled);
        world.server.advance(Duration::minutes(3));
        let stalled = bob.slot(&migrate).await.unwrap();
        assert!(stalled.slot.hold.unwrap().stalled);
        let taken = bob
            .take_over_slot(
                &migrate,
                &TakeOverSlotRequest {
                    session_id: b,
                    purpose: Purpose::parse("finish it").unwrap(),
                    reason: "the migration log stopped".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(taken.previous.holder.as_str(), "alice");
        let gone = alice.slot_heartbeat(&migrate, run).await.unwrap_err();
        assert_eq!(gone.code(), "taken_over");
        assert!(matches!(gone, ClientError::Conflict { .. }), "{gone:?}");
        assert_eq!(
            gone.details()
                .and_then(|d| d.holder.clone())
                .map(|h| h.to_string()),
            Some("bob".to_owned())
        );
        let forced = alice
            .force_release_slot(
                &migrate,
                &ForceReleaseSlotRequest {
                    session_id: a,
                    reason: "done for today".to_owned(),
                },
            )
            .await
            .unwrap();
        assert_eq!(forced.holder.as_str(), "bob");
        assert!(forced.thread_id.is_some());
        let prod = slot("deploy:prod");
        let run = alice
            .take_slot(&prod, &take(a, HoldMode::Run))
            .await
            .unwrap()
            .run_id
            .unwrap();
        let released = alice
            .release_slot(
                &prod,
                &ReleaseSlotRequest {
                    run_id: Some(run),
                    exit: Some(RunExit::Code(3)),
                },
            )
            .await
            .unwrap();
        assert_eq!(released.released.liveness, Liveness::Run { run });
    });
}
