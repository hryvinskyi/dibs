//! A server that is not there, is too slow, refuses the token, or answers something that is not
//! dibs.

use std::time::{Duration, Instant};

use dibs_client::api::NewItem;
use dibs_client::server_url::ServerUrl;
use dibs_client::token::Token;
use dibs_client::{Client, ClientError, ClientOptions};
use dibs_domain::item::ExternalRef;

use crate::support::{World, client, run};

#[test]
fn nothing_listening_is_a_network_failure() {
    let url = dibs_test_server::unreachable().unwrap();
    let error = run(client(&url, "dibs_token").me()).unwrap_err();
    // A closed port refuses at once on Unix; on Windows the connect retries until it times out.
    assert!(
        matches!(
            error,
            ClientError::Network { .. } | ClientError::Timeout { .. }
        ),
        "{error:?}"
    );
    assert!(error.is_unavailable());
    assert!(error.to_string().contains(&url), "{error}");
}

#[test]
fn a_server_that_never_answers_times_out_within_the_limit() {
    let url = dibs_test_server::silent().unwrap();
    let options = ClientOptions {
        timeout: Duration::from_millis(300),
        ..ClientOptions::default()
    };
    let slow = Client::new(
        ServerUrl::parse(&url).unwrap(),
        Token::parse("dibs_token").unwrap(),
        options,
    )
    .unwrap();
    let started = Instant::now();
    let error = run(slow.me()).unwrap_err();
    assert!(matches!(error, ClientError::Timeout { .. }), "{error:?}");
    assert!(error.is_unavailable());
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_challenge_page_is_unexpected_and_never_shown() {
    let url = dibs_test_server::fixed_answer(
        403,
        "text/html; charset=UTF-8",
        "<!DOCTYPE html><title>Attention Required</title>",
    )
    .unwrap();
    let error = run(client(&url, "dibs_token").me()).unwrap_err();
    assert!(
        matches!(error, ClientError::Unexpected { status: 403, .. }),
        "{error:?}"
    );
    assert!(error.is_unavailable());
    assert!(!error.to_string().contains("Attention"));
}

#[test]
fn a_proxy_error_in_json_of_another_shape_is_unexpected() {
    let url =
        dibs_test_server::fixed_answer(502, "application/json", r#"{"message":"bad gateway"}"#)
            .unwrap();
    let error = run(client(&url, "dibs_token").me()).unwrap_err();
    assert!(matches!(error, ClientError::Unexpected { status: 502, .. }));
}

#[test]
fn an_unknown_token_is_unauthorized_with_the_servers_sentence() {
    let world = World::new();
    let error =
        run(client(&world.server.url(), "dibs_unknown_token_of_nobody_here").me()).unwrap_err();
    let ClientError::Unauthorized { message } = &error else {
        panic!("{error:?}");
    };
    assert!(message.ends_with('.'), "{message}");
    assert!(!error.is_unavailable());
}

fn person_client(url: &str, token: &str) -> Client {
    Client::new(
        ServerUrl::parse(url).unwrap(),
        Token::parse(token).unwrap(),
        ClientOptions::for_person(),
    )
    .unwrap()
}

#[test]
fn a_persons_read_is_sent_once_more_when_its_connection_is_reset() {
    let world = World::new();
    let resetting = dibs_test_server::resetting_first_connection(&world.server.url()).unwrap();
    let me = run(person_client(&resetting, &world.bob.token).me()).unwrap();
    assert_eq!(me.member.handle.as_str(), "bob");
    assert_eq!(world.server.requests_to("/v1/me"), 1);
}

#[test]
fn without_a_persons_options_a_reset_connection_is_not_tried_again() {
    let world = World::new();
    let resetting = dibs_test_server::resetting_first_connection(&world.server.url()).unwrap();
    let error = run(client(&resetting, &world.bob.token).me()).unwrap_err();
    assert!(matches!(error, ClientError::Network { .. }), "{error:?}");
    assert!(error.is_unavailable());
    assert_eq!(world.server.requests_to("/v1/me"), 0);
}

#[test]
fn a_request_whose_repeat_would_do_it_twice_is_never_sent_again() {
    let world = World::new();
    let local = NewItem {
        external_ref: None,
        title: "Rounding".parse().unwrap(),
    };
    let resetting = dibs_test_server::resetting_first_connection(&world.server.url()).unwrap();
    let error = run(person_client(&resetting, &world.bob.token).create_item(&local)).unwrap_err();
    assert!(matches!(error, ClientError::Network { .. }), "{error:?}");
    assert_eq!(world.server.requests_to("/v1/items"), 0);

    let named = NewItem {
        external_ref: Some(ExternalRef::parse("gh:acme/api#4").unwrap()),
        title: "Rounding".parse().unwrap(),
    };
    let resetting = dibs_test_server::resetting_first_connection(&world.server.url()).unwrap();
    let error = run(person_client(&resetting, &world.bob.token).create_item(&named)).unwrap_err();
    assert!(
        matches!(error, ClientError::Network { .. }),
        "an item named by a reference is not sent again after a reset either, so it is never \
         answered as one that already existed: {error:?}"
    );
    assert_eq!(world.server.requests_to("/v1/items"), 0);
}

#[test]
fn a_person_waits_out_an_answer_slower_than_the_default_limit() {
    let world = World::new();
    world.server.delay_answers(Duration::from_secs(6));
    let started = Instant::now();
    let me = run(person_client(&world.server.url(), &world.bob.token).me()).unwrap();
    assert_eq!(me.member.handle.as_str(), "bob");
    assert!(started.elapsed() >= Duration::from_secs(6));

    let started = Instant::now();
    let error = run(world.client(&world.bob).me()).unwrap_err();
    assert!(matches!(error, ClientError::Timeout { .. }), "{error:?}");
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_secs(6),
        "the default limit of five seconds holds: {waited:?}"
    );
}

fn narrowed_areas(world: &World, client: &Client) -> Result<usize, ClientError> {
    use dibs_client::api::{ClaimRequest, NarrowRequest};

    let bob = world.client(&world.bob);
    run(async {
        let item = bob
            .create_item(&NewItem {
                external_ref: None,
                title: "Billing".parse().unwrap(),
            })
            .await
            .unwrap()
            .item
            .id;
        let session = crate::support::session(&bob, "desk/cli").await.session_id;
        bob.claim(
            item,
            &ClaimRequest {
                session_id: session,
                areas: vec!["billing".to_owned()],
                wide: None,
                kind: None,
            },
        )
        .await
        .unwrap();
        client
            .narrow(
                item,
                &NarrowRequest {
                    areas: vec!["billing".to_owned()],
                },
            )
            .await
            .map(|narrowed| narrowed.areas.len())
    })
}

#[test]
fn a_reset_under_a_change_is_reported_as_the_failure_it_is_and_never_sent_again() {
    let world = World::new();
    let resetting = dibs_test_server::resetting_first_connection(&world.server.url()).unwrap();
    let error = narrowed_areas(&world, &person_client(&resetting, &world.bob.token)).unwrap_err();
    assert!(
        matches!(error, ClientError::Network { .. }),
        "a lost answer is a failure, not an answer that nothing changed: {error:?}"
    );
    assert!(
        !world
            .server
            .request_paths()
            .iter()
            .any(|path| path.ends_with("/claim/narrow")),
        "{:?}",
        world.server.request_paths()
    );
}

// Only Linux keeps a connection unopened while the queue it waits in is full: it drops the
// connection's first packet, so the client waits out its connect limit. macOS and Windows refuse
// the connection at once, which is no connect timeout.
#[cfg(target_os = "linux")]
#[test]
fn a_change_whose_connection_never_opened_is_sent_once_more() {
    let world = World::new();
    let unopened =
        dibs_test_server::unopened_for(&world.server.url(), Duration::from_secs(3)).unwrap();
    let options = ClientOptions {
        connect_timeout: Duration::from_millis(2500),
        ..ClientOptions::for_person()
    };
    let client = Client::new(
        ServerUrl::parse(&unopened).unwrap(),
        Token::parse(&world.bob.token).unwrap(),
        options,
    )
    .unwrap();
    assert_eq!(narrowed_areas(&world, &client).unwrap(), 1);
    let narrows = world
        .server
        .request_paths()
        .iter()
        .filter(|path| path.ends_with("/claim/narrow"))
        .count();
    assert_eq!(narrows, 1, "sent once: the first sending never left");

    let unopened =
        dibs_test_server::unopened_for(&world.server.url(), Duration::from_secs(3)).unwrap();
    let without = Client::new(
        ServerUrl::parse(&unopened).unwrap(),
        Token::parse(&world.bob.token).unwrap(),
        ClientOptions {
            connect_timeout: Duration::from_millis(2500),
            ..ClientOptions::default()
        },
    )
    .unwrap();
    let error = run(without.me()).unwrap_err();
    assert!(matches!(error, ClientError::Timeout { .. }), "{error:?}");
}

#[test]
fn revoking_ones_own_token_answers_that_it_is_revoked() {
    let world = World::new();
    let prefix: String = world
        .bob
        .token
        .trim_start_matches("dibs_")
        .chars()
        .take(8)
        .collect();
    let mine = person_client(&world.server.url(), &world.alice.token);
    let alice_prefix: String = world
        .alice
        .token
        .trim_start_matches("dibs_")
        .chars()
        .take(8)
        .collect();
    let revoked = run(mine.revoke_token(&alice_prefix)).unwrap();
    assert_eq!(revoked.token.member.as_str(), "alice");
    assert!(revoked.token.revoked_at.is_some());
    let after = run(mine.revoke_token(&prefix)).unwrap_err();
    assert!(
        matches!(after, ClientError::Unauthorized { .. }),
        "the revoked token signs in no more: {after:?}"
    );
}
