//! The event stream over a real TCP connection to the served router: it resumes from
//! `Last-Event-ID` or `?after=` with no gap and no duplicate, delivers what is appended while it is
//! open, never carries a conversation the reader may not read nor another project's events, and
//! ends cleanly when the server stops.

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use dibs_domain::member::Role;

use crate::support::{Person, World};

/// How long a test waits for something it expects.
const WAIT: Duration = Duration::from_secs(5);

/// How long a test waits to be sure something does not arrive.
const QUIET: Duration = Duration::from_millis(300);

/// One event as received: its id and data.
#[derive(Debug, Clone, PartialEq)]
struct Received {
    id: u64,
    data: Value,
}

/// A client of the stream speaking HTTP/1.1 over a socket, decoding the chunked body and the
/// server-sent events in it.
struct Client {
    socket: TcpStream,
    raw: Vec<u8>,
    text: String,
    ended: bool,
}

impl Client {
    /// Connects as `who` to the stream at `path`, sending `Last-Event-ID` when given, and reads the
    /// answer's head.
    async fn connect(
        address: SocketAddr,
        who: &Person,
        path: &str,
        last_event_id: Option<u64>,
    ) -> (String, Self) {
        let mut socket = TcpStream::connect(address).await.unwrap();
        let mut head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nAccept: text/event-stream\r\n",
            who.token
        );
        if let Some(id) = last_event_id {
            head.push_str(&format!("Last-Event-ID: {id}\r\n"));
        }
        head.push_str("\r\n");
        socket.write_all(head.as_bytes()).await.unwrap();
        let mut client = Self {
            socket,
            raw: Vec::new(),
            text: String::new(),
            ended: false,
        };
        let head = loop {
            if let Some(end) = find(&client.raw, b"\r\n\r\n") {
                let head = String::from_utf8(client.raw[..end].to_vec()).unwrap();
                client.raw.drain(..end + 4);
                break head;
            }
            assert!(
                client.fill(WAIT).await,
                "the connection closed before the head"
            );
        };
        (head, client)
    }

    /// Reads more bytes from the socket; `false` when it closed or nothing came within `wait`.
    async fn fill(&mut self, wait: Duration) -> bool {
        let mut buffer = [0u8; 4096];
        match tokio::time::timeout(wait, self.socket.read(&mut buffer)).await {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => false,
            Ok(Ok(read)) => {
                self.raw.extend_from_slice(&buffer[..read]);
                true
            }
        }
    }

    /// Moves every whole chunk read so far into the text; notes the last chunk.
    fn decode(&mut self) {
        loop {
            let Some(line_end) = find(&self.raw, b"\r\n") else {
                return;
            };
            let size_text = String::from_utf8(self.raw[..line_end].to_vec()).unwrap();
            let size = usize::from_str_radix(size_text.trim(), 16).unwrap();
            if self.raw.len() < line_end + 2 + size + 2 {
                return;
            }
            let chunk = self.raw[line_end + 2..line_end + 2 + size].to_vec();
            self.raw.drain(..line_end + 2 + size + 2);
            if size == 0 {
                self.ended = true;
                return;
            }
            self.text.push_str(&String::from_utf8(chunk).unwrap());
        }
    }

    /// The next block the stream sent — an event or a comment — or `None` when nothing came
    /// within `wait` or the stream ended.
    async fn block(&mut self, wait: Duration) -> Option<String> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            self.decode();
            if let Some(end) = self.text.find("\n\n") {
                let block = self.text[..end].to_owned();
                self.text.drain(..end + 2);
                return Some(block);
            }
            if self.ended {
                return None;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() || !self.fill(left).await {
                return None;
            }
        }
    }

    /// The next event, skipping comments, within `wait`.
    async fn event(&mut self, wait: Duration) -> Option<Received> {
        loop {
            let block = self.block(wait).await?;
            if block.starts_with(':') {
                continue;
            }
            let mut id = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("id: ") {
                    id = Some(value.parse().unwrap());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data.push_str(value);
                }
            }
            return Some(Received {
                id: id.unwrap(),
                data: serde_json::from_str(&data).unwrap(),
            });
        }
    }

    /// The next `count` events; fails the test when they do not all come in time.
    async fn events(&mut self, count: usize) -> Vec<Received> {
        let mut events = Vec::new();
        while events.len() < count {
            let event = self.event(WAIT).await;
            events.push(event.unwrap_or_else(|| panic!("only {} events came", events.len())));
        }
        events
    }

    /// Whether the server ended the stream within `wait`.
    async fn ends(&mut self, wait: Duration) -> bool {
        while self.block(wait).await.is_some() {}
        self.ended
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The ids of the project's log as the feed shows them.
async fn log_ids(world: &World, who: &Person) -> Vec<u64> {
    world.get(who, "/v1/events").await.body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_u64().unwrap())
        .collect()
}

fn ids(events: &[Received]) -> Vec<u64> {
    events.iter().map(|e| e.id).collect()
}

#[tokio::test]
async fn a_reconnect_with_last_event_id_gets_exactly_the_missed_events() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let (address, _served) = world.serve().await;
    for n in 1..=4 {
        world.item(&alice, None, &format!("Before {n}")).await;
    }
    let log = log_ids(&world, &alice).await;

    let (head, mut client) =
        Client::connect(address, &alice, "/v1/events/stream", Some(log[1])).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    let head = head.to_ascii_lowercase();
    assert!(head.contains("content-type: text/event-stream"), "{head}");
    assert!(
        head.contains("cache-control: no-cache, no-transform"),
        "{head}"
    );
    let replayed = client.events(2).await;
    assert_eq!(ids(&replayed), log[2..]);
    assert_eq!(replayed[0].data["id"], log[2]);

    let started = tokio::time::Instant::now();
    world.item(&alice, None, "While connected").await;
    let live = client.events(1).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(live[0].data["kind"], "item_created");
    assert_eq!(client.event(QUIET).await, None, "no duplicate follows");
    let last_seen = live[0].id;
    drop(client);

    for n in 1..=3 {
        world.item(&alice, None, &format!("While away {n}")).await;
    }
    let log = log_ids(&world, &alice).await;
    let missed: Vec<u64> = log.iter().copied().filter(|id| *id > last_seen).collect();
    assert_eq!(missed.len(), 3);
    let (_, mut again) =
        Client::connect(address, &alice, "/v1/events/stream", Some(last_seen)).await;
    assert_eq!(ids(&again.events(3).await), missed);
    assert_eq!(again.event(QUIET).await, None);
}

#[tokio::test]
async fn a_reconnect_with_after_gets_exactly_the_missed_events_and_the_header_wins() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let (address, _served) = world.serve().await;
    for n in 1..=3 {
        world.item(&alice, None, &format!("Item {n}")).await;
    }
    let log = log_ids(&world, &alice).await;
    let path = format!("/v1/events/stream?after={}", log[0]);
    let (_, mut by_query) = Client::connect(address, &alice, &path, None).await;
    assert_eq!(ids(&by_query.events(2).await), log[1..]);
    assert_eq!(by_query.event(QUIET).await, None);

    let (_, mut both) = Client::connect(address, &alice, &path, Some(log[1])).await;
    assert_eq!(ids(&both.events(1).await), log[2..]);
    assert_eq!(both.event(QUIET).await, None);

    let (_, mut fresh) = Client::connect(address, &alice, "/v1/events/stream", None).await;
    assert_eq!(
        fresh.event(QUIET).await,
        None,
        "a fresh stream replays nothing"
    );
    world.item(&alice, None, "After connecting").await;
    let next = fresh.events(1).await;
    assert!(next[0].id > log[2]);
}

#[tokio::test]
async fn a_conversation_reaches_only_the_streams_of_its_readers() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let bob = world.member(&alice, "bob", Role::Member).await;
    let carol = world.member(&alice, "carol", Role::Member).await;
    let (address, _served) = world.serve().await;
    let (_, mut bobs) = Client::connect(address, &bob, "/v1/events/stream", None).await;
    let (_, mut carols) = Client::connect(address, &carol, "/v1/events/stream", None).await;

    let thread = world
        .thread(&alice, json!(["bob"]), "Private subject", "Private body")
        .await;
    let sentinel = world.item(&alice, None, "Everyone sees this").await;

    let seen_by_bob = bobs.events(3).await;
    let kinds: Vec<&str> = seen_by_bob
        .iter()
        .map(|e| e.data["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["thread_started", "message_posted", "item_created"]);
    let posted = &seen_by_bob[1].data;
    assert_eq!(posted["payload"]["thread"], thread);
    assert_eq!(posted["message"]["body"], "Private body");
    assert_eq!(posted["message"]["author"], "alice");
    assert_eq!(posted["message"]["label"], "laptop/mcp");
    assert_eq!(posted["message"]["created_at"], "2026-03-02T09:00:00Z");
    assert!(seen_by_bob[0].data.get("message").is_none());

    let seen_by_carol = carols.events(1).await;
    assert_eq!(seen_by_carol[0].data["payload"]["item"], sentinel);
    assert_eq!(seen_by_carol[0].id, seen_by_bob[2].id);
    assert_eq!(carols.event(QUIET).await, None);

    let (_, mut replay) = Client::connect(address, &carol, "/v1/events/stream?after=0", None).await;
    let everything = replay.events(1).await;
    assert_eq!(everything[0].data["payload"]["item"], sentinel);
    assert_eq!(replay.event(QUIET).await, None);
    assert!(!format!("{everything:?}").contains("Private"));
}

#[tokio::test]
async fn another_projects_events_never_arrive() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let dave = world.project("globex", "dave").await;
    let (address, _served) = world.serve().await;
    let (_, mut daves) = Client::connect(address, &dave, "/v1/events/stream?after=0", None).await;
    for n in 1..=3 {
        world.item(&alice, None, &format!("Acme {n}")).await;
    }
    world
        .thread(&alice, json!("project"), "Acme only", "Acme only")
        .await;
    let own = world.item(&dave, None, "Globex").await;
    let received = daves.events(1).await;
    assert_eq!(received[0].data["payload"]["item"], own);
    assert_eq!(received[0].data["members"].as_object().unwrap().len(), 1);
    assert_eq!(daves.event(QUIET).await, None);
}

#[tokio::test]
async fn stopping_the_server_ends_every_stream_and_the_server_with_them() {
    let world = World::new().await;
    let alice = world.project("acme", "alice").await;
    let (address, served) = world.serve().await;
    let (_, mut first) = Client::connect(address, &alice, "/v1/events/stream", None).await;
    let (_, mut second) = Client::connect(address, &alice, "/v1/events/stream", None).await;
    world.shutdown.stop();
    assert!(first.ends(WAIT).await);
    assert!(second.ends(WAIT).await);
    tokio::time::timeout(WAIT, served).await.unwrap().unwrap();
}
