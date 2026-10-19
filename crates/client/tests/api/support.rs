//! A server with one project, its people signed in through clients of their own.

use std::future::Future;

use dibs_client::api::{RegisterSession, Session};
use dibs_client::server_url::ServerUrl;
use dibs_client::token::Token;
use dibs_client::{Client, ClientOptions};
use dibs_domain::member::Role;
use dibs_test_server::{Person, TestServer, two_repositories};

/// The server, its project `acme` with the two repositories, the admin `alice` and the member
/// `bob`.
pub struct World {
    pub server: TestServer,
    pub alice: Person,
    pub bob: Person,
}

impl World {
    pub fn new() -> Self {
        let server = TestServer::start().unwrap();
        let alice = server.project("acme", "alice").unwrap();
        let bob = server.member(&alice, "bob", Role::Member).unwrap();
        server.settings(&alice, two_repositories()).unwrap();
        Self { server, alice, bob }
    }

    pub fn client(&self, who: &Person) -> Client {
        client(&self.server.url(), &who.token)
    }
}

pub fn client(url: &str, token: &str) -> Client {
    Client::new(
        ServerUrl::parse(url).unwrap(),
        Token::parse(token).unwrap(),
        ClientOptions::default(),
    )
    .unwrap()
}

/// Runs a future to its end on a runtime of the test's own.
pub fn run<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

pub async fn session(client: &Client, label: &str) -> Session {
    client
        .register_session(&RegisterSession {
            host: "laptop".parse().unwrap(),
            label: label.parse().unwrap(),
            agent: "cli".parse().unwrap(),
            push: false,
        })
        .await
        .unwrap()
}
