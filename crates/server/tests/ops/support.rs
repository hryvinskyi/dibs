//! The `dibs-server` binary run as an operator runs it: its commands against a database file in a
//! temporary directory, and the server spawned on an ephemeral port, its JSON log read line by line.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

/// The binary under test.
pub const BINARY: &str = env!("CARGO_BIN_EXE_dibs-server");

/// A temporary directory with a database file and a snapshot directory in it.
pub struct Place {
    dir: TempDir,
}

impl Place {
    /// A new, empty directory; the snapshot directory exists, the database does not.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("snapshots")).unwrap();
        Self { dir }
    }

    /// The database file.
    pub fn database(&self) -> PathBuf {
        self.dir.path().join("dibs.db")
    }

    /// The snapshot directory.
    pub fn snapshots(&self) -> PathBuf {
        self.dir.path().join("snapshots")
    }

    /// A path inside the directory.
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The snapshot files in the snapshot directory, by name.
    pub fn snapshot_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.snapshots())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

/// What a command printed and how it exited.
#[derive(Debug)]
pub struct Ran {
    /// The exit status.
    pub code: Option<i32>,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// Runs the binary with `args` against the database at `database`, with nothing else in its
/// environment but `env`.
pub fn run(database: &Path, args: &[&str], env: &[(&str, &Path)]) -> Ran {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("DIBS_DATABASE", database)
        .args(args);
    for (var, value) in env {
        command.env(var, value);
    }
    let output = command.output().unwrap();
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// Creates the project `widgets` with its admin `alice` and answers the token it printed.
pub fn init(database: &Path) -> String {
    let ran = run(
        database,
        &[
            "admin",
            "init",
            "--project",
            "widgets",
            "--admin",
            "alice",
            "--name",
            "Alice",
        ],
        &[],
    );
    assert_eq!(ran.code, Some(0), "{ran:?}");
    token_of(&ran)
}

/// The token a command printed: standard output holds it and nothing else.
pub fn token_of(ran: &Ran) -> String {
    let token = ran.stdout.trim_end_matches('\n');
    assert!(token.starts_with("dibs_"), "{ran:?}");
    assert!(!token.contains('\n'), "{ran:?}");
    assert!(!ran.stderr.contains(token), "the token is printed once");
    token.to_owned()
}

/// A spawned server, killed when dropped unless it already exited.
pub struct Server {
    child: Child,
    /// The address it listens on.
    pub address: SocketAddr,
    logs: Receiver<Value>,
}

impl Server {
    /// Spawns the server on the database at `database` with `env` added.
    pub fn spawn(database: &Path, env: &[(&str, &Path)]) -> Self {
        let mut command = Command::new(BINARY);
        command
            .env_clear()
            .env("DIBS_DATABASE", database)
            .env("DIBS_LISTEN", "127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (var, value) in env {
            command.env(var, value);
        }
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, logs) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(entry) = serde_json::from_str::<Value>(&line)
                    && sender.send(entry).is_err()
                {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            address: SocketAddr::from(([127, 0, 0, 1], 0)),
            logs,
        };
        let listening = server.log("listening", Duration::from_secs(30));
        server.address = listening["fields"]["address"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        server
    }

    /// The next log entry with this message, waiting at most `wait`.
    pub fn log(&mut self, message: &str, wait: Duration) -> Value {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let entry = self
                .logs
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no '{message}' in the log within {wait:?}"));
            if entry["fields"]["message"] == message {
                return entry;
            }
        }
    }

    /// Sends `SIGTERM`.
    #[cfg(unix)]
    pub fn terminate(&self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Waits at most `wait` for the process to exit.
    pub fn exit(&mut self, wait: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One request on a connection of its own, read to the end: the status and the whole answer.
pub fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let body = body.unwrap_or("");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    (status_of(&answer), answer)
}

/// The JSON body of an HTTP/1.1 answer read to its end.
pub fn body_of(answer: &str) -> Value {
    let (_, body) = answer
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("no body: {answer}"));
    serde_json::from_str(body).unwrap_or_else(|error| panic!("{error}: {answer}"))
}

/// The status code of an HTTP/1.1 answer.
pub fn status_of(answer: &str) -> u16 {
    answer
        .strip_prefix("HTTP/1.1 ")
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("not an HTTP answer: {answer}"))
}

/// The write-ahead log of the database at `database`.
pub fn wal(database: &Path) -> PathBuf {
    let mut name = database.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}
