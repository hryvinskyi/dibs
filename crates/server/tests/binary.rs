//! The `dibs-server` binary: it starts on an ephemeral port against a fresh database file and
//! answers `/health`, and it refuses a configuration naming the variable at fault.

#[cfg(test)]
mod binary {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    const BINARY: &str = env!("CARGO_BIN_EXE_dibs-server");

    /// Kills the server when the test ends, however it ends.
    struct Running(Child);

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// The address the server logged that it listens on.
    fn listening_address(child: &mut Child) -> SocketAddr {
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if entry["fields"]["message"] == "listening" {
                    let address = entry["fields"]["address"].as_str().unwrap().to_owned();
                    let _ = sender.send(address);
                }
            }
        });
        receiver
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .parse()
            .unwrap()
    }

    fn get(address: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    }

    #[test]
    fn the_binary_serves_health_on_an_ephemeral_port() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("dibs.db");
        let child = Command::new(BINARY)
            .env_clear()
            .env("DIBS_DATABASE", &database)
            .env("DIBS_LISTEN", "127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut running = Running(child);
        let address = listening_address(&mut running.0);
        let health = get(address, "/health");
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        assert!(
            health
                .to_ascii_lowercase()
                .contains("cache-control: no-store")
        );
        assert!(health.ends_with(r#"{"ok":true}"#), "{health}");
        let me = get(address, "/v1/me");
        assert!(me.starts_with("HTTP/1.1 401"), "{me}");
        assert!(database.exists());
    }

    #[test]
    fn a_refused_configuration_exits_2_naming_the_variable() {
        let missing = Command::new(BINARY).env_clear().output().unwrap();
        assert_eq!(missing.status.code(), Some(2));
        let stderr = String::from_utf8(missing.stderr).unwrap();
        assert!(stderr.contains("DIBS_DATABASE is not set"), "{stderr}");
        let invalid = Command::new(BINARY)
            .env_clear()
            .env("DIBS_DATABASE", "dibs.db")
            .env("DIBS_LISTEN", "nowhere")
            .output()
            .unwrap();
        assert_eq!(invalid.status.code(), Some(2));
        let stderr = String::from_utf8(invalid.stderr).unwrap();
        assert!(stderr.contains("DIBS_LISTEN is not valid"), "{stderr}");
    }
}
