//! Conversations, the event log, and the event stream as it grows.

use std::time::Duration;

use futures_util::StreamExt;

use dibs_client::api::{PostMessage, Recipients, StartThread};
use dibs_domain::event::EventKind;

use crate::support::{World, run};

fn start(to: Recipients, body: &str) -> StartThread {
    StartThread {
        subject: "Billing rates".parse().unwrap(),
        to,
        item: None,
        label: "laptop/cli".parse().unwrap(),
        body: body.parse().unwrap(),
    }
}

#[test]
fn a_thread_is_started_answered_read_and_marked() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let started = alice
            .start_thread(&start(
                Recipients::Members(vec!["bob".parse().unwrap()]),
                "Can you look?\nLine two — ünïcode.",
            ))
            .await
            .unwrap();
        assert_eq!(started.thread.to.len(), 1);
        assert!(!started.thread.to_project);
        let inbox = bob.inbox(true).await.unwrap();
        assert_eq!(inbox.threads.len(), 1);
        assert_eq!(inbox.threads[0].unread, 1);
        let posted = bob
            .post_message(
                started.thread.id,
                &PostMessage {
                    label: "desktop/cli".parse().unwrap(),
                    body: "On it.".parse().unwrap(),
                },
            )
            .await
            .unwrap();
        assert_eq!(posted.message.author.as_str(), "bob");
        let detail = bob.thread(started.thread.id).await.unwrap();
        assert_eq!(detail.messages.len(), 2);
        assert_eq!(
            detail.messages[0].body.as_str(),
            "Can you look?\nLine two — ünïcode."
        );
        assert_eq!(
            bob.mark_read(started.thread.id).await.unwrap().newly_read,
            1
        );
        assert!(bob.inbox(true).await.unwrap().threads.is_empty());
        assert_eq!(bob.threads(None).await.unwrap().threads.len(), 1);

        let project = alice
            .start_thread(&start(Recipients::Project, "Everyone: deploy at noon."))
            .await
            .unwrap();
        assert!(project.thread.to_project);

        let empty = alice
            .start_thread(&start(Recipients::Members(Vec::new()), "nobody"))
            .await
            .unwrap_err();
        assert_eq!(empty.code(), "no_recipients");
    });
}

#[test]
fn the_log_is_read_page_by_page() {
    let world = World::new();
    let alice = world.client(&world.alice);
    run(async {
        let first = alice.events(None, Some(1)).await.unwrap();
        assert_eq!(first.events.len(), 1);
        let rest = alice.events(first.next, None).await.unwrap();
        assert!(
            rest.events
                .iter()
                .all(|event| event.id > first.events[0].id)
        );
    });
}

#[test]
fn the_stream_delivers_a_message_as_it_is_posted() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let latest = alice.events(None, None).await.unwrap().next;
        let mut stream = Box::pin(bob.events_stream(latest));
        let started = alice
            .start_thread(&start(
                Recipients::Members(vec!["bob".parse().unwrap()]),
                "Ping",
            ))
            .await
            .unwrap();
        let mut seen = Vec::new();
        while !seen.iter().any(|event: &dibs_client::api::Event| {
            matches!(event.kind, EventKind::MessagePosted(_))
        }) {
            let event = tokio::time::timeout(Duration::from_secs(10), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            seen.push(event);
        }
        let posted = seen
            .iter()
            .find(|event| matches!(event.kind, EventKind::MessagePosted(_)))
            .unwrap();
        let message = posted.message.as_ref().unwrap();
        assert_eq!(message.thread, started.thread.id);
        assert_eq!(message.body.as_str(), "Ping");
        assert_eq!(posted.members.values().next().unwrap().as_str(), "alice");
    });
}

#[test]
fn a_stream_refused_its_token_ends_with_the_refusal() {
    let world = World::new();
    let stranger = crate::support::client(&world.server.url(), "dibs_not_a_member_token_at_all_x");
    run(async {
        let mut stream = Box::pin(stranger.events_stream(None));
        let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.unwrap_err().code(), "unauthorized");
        assert!(stream.next().await.is_none());
    });
}

#[test]
fn a_stream_resumed_after_an_event_replays_what_followed_it() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let before = alice.events(None, None).await.unwrap().next;
        alice
            .start_thread(&start(
                Recipients::Members(vec!["bob".parse().unwrap()]),
                "Sent while bob was away",
            ))
            .await
            .unwrap();
        let mut stream = Box::pin(bob.events_stream(before));
        let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(first.id > before.unwrap());
        assert!(
            matches!(first.kind, EventKind::ThreadStarted(_)),
            "{first:?}"
        );
    });
}
