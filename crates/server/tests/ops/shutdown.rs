use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dibs_app::use_cases::ItemQuery;
use dibs_server::adapters::BroadcastNotifier;
use dibs_server::serve::use_cases;
use dibs_store_sqlite::SqliteStore;

use crate::support::{Place, Server, init, status_of, wal};

/// Reads until `needle` arrives, failing after `wait`.
fn read_until(stream: &mut TcpStream, needle: &str, wait: Duration) -> String {
    let deadline = Instant::now() + wait;
    let mut seen = Vec::new();
    let mut buffer = [0u8; 4096];
    while !String::from_utf8_lossy(&seen).contains(needle) {
        assert!(Instant::now() < deadline, "no '{needle}' in {seen:?}");
        match stream.read(&mut buffer) {
            Ok(0) => panic!("the connection closed before '{needle}'"),
            Ok(n) => seen.extend_from_slice(&buffer[..n]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == ErrorKind::TimedOut => {}
            Err(error) => panic!("{error}"),
        }
    }
    String::from_utf8_lossy(&seen).into_owned()
}

/// Reads until the peer closes the connection, failing after `wait`.
fn read_to_close(stream: &mut TcpStream, wait: Duration) -> String {
    let deadline = Instant::now() + wait;
    let mut seen = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        assert!(
            Instant::now() < deadline,
            "still open after {wait:?}: {}",
            String::from_utf8_lossy(&seen)
        );
        match stream.read(&mut buffer) {
            Ok(0) => return String::from_utf8_lossy(&seen).into_owned(),
            Ok(n) => seen.extend_from_slice(&buffer[..n]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == ErrorKind::TimedOut => {}
            Err(error) if error.kind() == ErrorKind::ConnectionReset => {
                return String::from_utf8_lossy(&seen).into_owned();
            }
            Err(error) => panic!("{error}"),
        }
    }
}

fn connect(server: &Server) -> TcpStream {
    let stream = TcpStream::connect(server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    stream
}

#[test]
fn sigterm_ends_the_streams_lets_the_request_in_flight_finish_and_checkpoints_the_log() {
    let place = Place::new();
    let token = init(&place.database());
    let mut server = Server::spawn(&place.database(), &[]);

    let mut stream = connect(&server);
    write!(
        stream,
        "GET /v1/events/stream HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\r\n"
    )
    .unwrap();
    let head = read_until(&mut stream, "\r\n\r\n", Duration::from_secs(10));
    assert_eq!(status_of(&head), 200, "{head}");
    assert!(head.contains("text/event-stream"), "{head}");

    let body = r#"{"title": "Invoice totals are off"}"#;
    let (first, rest) = body.split_at(10);
    let mut in_flight = connect(&server);
    write!(
        in_flight,
        "POST /v1/items HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{first}",
        body.len()
    )
    .unwrap();
    in_flight.flush().unwrap();
    std::thread::sleep(Duration::from_millis(500));

    server.terminate();
    server.log(
        "stopping: no new connections, the event streams end, requests in flight may finish",
        Duration::from_secs(10),
    );

    let ended = read_to_close(&mut stream, Duration::from_secs(5));
    assert!(
        ended.ends_with("0\r\n\r\n"),
        "the stream ends its body: {ended:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(server.address).is_ok() {
        assert!(
            Instant::now() < deadline,
            "the server still accepts connections"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        server.exit(Duration::from_millis(300)),
        None,
        "it waits for the request in flight"
    );
    in_flight.write_all(rest.as_bytes()).unwrap();
    let answer = read_to_close(&mut in_flight, Duration::from_secs(10));
    assert_eq!(status_of(&answer), 201, "{answer}");
    assert!(answer.contains("Invoice totals are off"), "{answer}");

    let status = server
        .exit(Duration::from_secs(15))
        .expect("the server exits");
    assert_eq!(status.code(), Some(0));
    server.log(
        "the write-ahead log is checkpointed",
        Duration::from_secs(5),
    );
    let log = wal(&place.database());
    assert!(
        std::fs::metadata(&log).map_or(true, |meta| meta.len() == 0),
        "the log is empty or gone"
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let titles = runtime.block_on(async {
        let store = Arc::new(SqliteStore::open(&place.database()).await.unwrap());
        let app = use_cases(store.clone(), Arc::new(BroadcastNotifier::new(1)));
        let actor = app.authenticate.execute(&token).await.unwrap();
        let items = app
            .list_items
            .execute(
                &actor,
                ItemQuery {
                    status: None,
                    holder: None,
                    external_ref: None,
                },
            )
            .await
            .unwrap();
        store.close().await;
        items
            .into_iter()
            .map(|item| item.item.title.to_string())
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, ["Invoice totals are off"]);
}
