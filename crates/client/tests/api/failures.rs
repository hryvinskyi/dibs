//! A server that is not there, is too slow, refuses the token, or answers something that is not
//! dibs.

use std::time::{Duration, Instant};

use dibs_client::server_url::ServerUrl;
use dibs_client::token::Token;
use dibs_client::{Client, ClientError, ClientOptions};

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
