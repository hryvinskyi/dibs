//! The harness: the built `dibs-server` and `dibs` binaries, run as separate processes against a
//! temporary database on an ephemeral port, with people who each have a home of their own.
//!
//! Every variable a child reads is set on the child, whose environment starts empty: nothing of
//! the developer's own configuration, credentials or git settings reaches a case.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

/// The paths of the two built binaries.
struct Binaries {
    dibs: PathBuf,
    server: PathBuf,
}

/// The binaries next to the running test executable, built first when they are missing or old.
fn binaries() -> &'static Binaries {
    static BINARIES: OnceLock<Binaries> = OnceLock::new();
    BINARIES.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().unwrap().parent().unwrap().to_owned();
        let target_dir = profile_dir.parent().unwrap();
        let profile = profile_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut build = Command::new(cargo);
        build
            .args([
                "build",
                "--quiet",
                "-p",
                "dibs",
                "-p",
                "dibs-server",
                "--target-dir",
            ])
            .arg(target_dir);
        match profile.as_str() {
            "debug" => {}
            "release" => {
                build.arg("--release");
            }
            other => {
                build.args(["--profile", other]);
            }
        }
        let status = build.status().unwrap();
        assert!(status.success(), "the binaries could not be built");
        let executable =
            |name: &str| profile_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        Binaries {
            dibs: executable("dibs"),
            server: executable("dibs-server"),
        }
    })
}

/// The text of a byte stream, lossily.
pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A command with an empty environment but for what a program needs to be found and to run.
fn clean(program: &Path, home: &Path) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    for name in ["PATH", "SystemRoot", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let no_file = if cfg!(windows) { "NUL" } else { "/dev/null" };
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("GIT_CONFIG_GLOBAL", no_file)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

/// A running `dibs-server` on a temporary database, killed when it is dropped.
pub struct Server {
    child: Child,
    url: String,
    project: String,
    database: PathBuf,
    scratch: TempDir,
}

impl Server {
    /// Creates the project `project` with its admin `admin` (`dibs-server admin init`), then
    /// serves it on an ephemeral port on the loopback address. Returns the server and the admin's
    /// token.
    pub fn start(project: &str, admin: &str) -> (Self, String) {
        let scratch = tempfile::tempdir().unwrap();
        let database = scratch.path().join("dibs.db");
        let init = Self::admin_command(scratch.path(), &database)
            .args([
                "admin",
                "init",
                "--project",
                project,
                "--admin",
                admin,
                "--name",
            ])
            .arg(format!("{admin} Example"))
            .output()
            .unwrap();
        assert!(init.status.success(), "admin init: {}", text(&init.stderr));
        let token = token_of(&init);

        let mut child = Self::admin_command(scratch.path(), &database)
            .env("DIBS_LISTEN", "127.0.0.1:0")
            .arg("serve")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        // The log is read until the server ends: an unread pipe would stall it once full.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(entry) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if entry["fields"]["message"] == "listening"
                    && let Some(address) = entry["fields"]["address"].as_str()
                {
                    let _ = sender.send(address.to_owned());
                }
            }
        });
        let address = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
        let server = Self {
            child,
            url: format!("http://{address}"),
            project: project.to_owned(),
            database,
            scratch,
        };
        (server, token)
    }

    /// `dibs-server` for the database, in the server's scratch directory.
    fn admin_command(scratch: &Path, database: &Path) -> Command {
        let mut command = clean(&binaries().server, scratch);
        command.env("DIBS_DATABASE", database).current_dir(scratch);
        command
    }

    /// The server's base URL, `http://127.0.0.1:<port>`.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The slug of the project.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Adds a member with `dibs-server admin member add` and returns their token.
    pub fn add_member(&self, handle: &str) -> String {
        self.add_member_as(handle, "member")
    }

    /// Adds a member of `role` (`admin`, `member` or `importer`) with `dibs-server admin member
    /// add --role` and returns their token.
    pub fn add_member_as(&self, handle: &str, role: &str) -> String {
        let output = Self::admin_command(self.scratch.path(), &self.database)
            .args([
                "admin",
                "member",
                "add",
                handle,
                "--project",
                &self.project,
                "--role",
                role,
                "--name",
            ])
            .arg(format!("{handle} Example"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "admin member add: {}",
            text(&output.stderr)
        );
        token_of(&output)
    }
}

/// One HTTP/1.1 request to the server with a bearer token and a JSON body, as an integration
/// worker sends it; answers the status and the JSON it got back.
pub fn call(server: &Server, method: &str, path: &str, token: &str, body: &Value) -> (u16, Value) {
    let address = server.url().trim_start_matches("http://");
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let payload = body.to_string();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let chunked = head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
    let json = if chunked {
        unchunk(body)
    } else {
        body.to_owned()
    };
    (status, serde_json::from_str(&json).unwrap_or(Value::Null))
}

/// The payload of a chunked HTTP/1.1 body.
fn unchunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].trim_start_matches("\r\n");
    }
    out
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The token a `dibs-server` admin command printed: standard output holds it alone.
fn token_of(output: &Output) -> String {
    let out = text(&output.stdout);
    let token = out.trim();
    assert!(
        token.starts_with("dibs_") && !token.contains(char::is_whitespace),
        "standard output is not a token alone: {out:?}"
    );
    token.to_owned()
}

/// A member of the project with a home of their own (credentials and state directories), signed in.
pub struct Person {
    pub handle: String,
    home: PathBuf,
}

impl Person {
    /// Signs `handle` in with `dibs login` from a directory outside every checkout. The person's
    /// directories are under `root/homes/<handle>`.
    pub fn sign_in(server: &Server, root: &Path, handle: &str, token: &str) -> Self {
        let person = Self {
            handle: handle.to_owned(),
            home: root.join("homes").join(handle),
        };
        fs::create_dir_all(&person.home).unwrap();
        let mut login = person
            .dibs(&person.home)
            .args(["login", "--server", server.url()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(login.stdin.take().unwrap(), "{token}").unwrap();
        let out = login.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "login of {handle}: {}",
            text(&out.stderr)
        );
        person
    }

    /// `dibs` as this person in `cwd`.
    pub fn dibs(&self, cwd: &Path) -> Command {
        let mut command = clean(&binaries().dibs, &self.home);
        command
            .current_dir(cwd)
            .env("DIBS_CONFIG_DIR", self.home.join("config"))
            .env("DIBS_STATE_DIR", self.home.join("state"));
        command
    }

    /// Runs `dibs args…` in `cwd` and returns what it printed.
    pub fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        self.dibs(cwd).args(args).output().unwrap()
    }

    /// Runs `dibs args…` in `cwd`, which must succeed, and returns its standard output.
    pub fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run(cwd, args);
        assert!(
            out.status.success(),
            "{} dibs {args:?}: {}",
            self.handle,
            text(&out.stderr)
        );
        text(&out.stdout)
    }

    /// Runs `dibs args… --json` in `cwd` and parses the one JSON document it printed, success or
    /// refusal alike. The exit code comes with it.
    pub fn json(&self, cwd: &Path, args: &[&str]) -> (Option<i32>, Value) {
        let out = self.dibs(cwd).args(args).arg("--json").output().unwrap();
        let value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("not JSON: {}", text(&out.stdout)));
        (out.status.code(), value)
    }

    /// The reply of `dibs hook neutral before-tool` for an edit of `paths` (relative to `cwd`) in
    /// `cwd`, by an agent session `session` of this person.
    pub fn before_edit(&self, cwd: &Path, session: &str, paths: &[&str]) -> Value {
        let input = serde_json::json!({
            "protocol": 1,
            "session": session,
            "cwd": cwd,
            "thread": "main",
            "tool": {"kind": "edit", "paths": paths},
        });
        let mut hook = self
            .dibs(cwd)
            .args(["hook", "neutral", "before-tool"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(hook.stdin.take().unwrap(), "{input}").unwrap();
        let out = hook.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "a hook always exits 0: {}",
            text(&out.stderr)
        );
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("not one JSON object: {}", text(&out.stdout)))
    }
}

/// Writes a settings file in `dir` and applies it as the admin.
pub fn apply_settings(admin: &Person, dir: &Path, settings: &str) {
    let file = dir.join("settings.toml");
    fs::write(&file, settings).unwrap();
    admin.ok(
        dir,
        &["admin", "settings", "apply", &file.to_string_lossy()],
    );
}

/// A git checkout at `dir` whose `origin` is `remote`, with the `.dibs.toml` of the project (and
/// of `repo`, when the project lists several), and one commit.
pub fn checkout(dir: &Path, server: &Server, repo: Option<&str>, remote: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.name", "Test"],
        &["config", "user.email", "test@example.com"],
        &["config", "commit.gpgsign", "false"],
        &["remote", "add", "origin", remote],
    ] {
        git(dir, args);
    }
    let mut config = format!(
        "server = \"{}\"\nproject = \"{}\"\n",
        server.url(),
        server.project()
    );
    if let Some(repo) = repo {
        config.push_str(&format!("repo = \"{repo}\"\n"));
    }
    fs::write(dir.join(".dibs.toml"), config).unwrap();
    fs::write(dir.join("README.md"), "hello\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "start"]);
    dir.to_owned()
}

/// Runs git in `dir` with an empty environment.
fn git(dir: &Path, args: &[&str]) {
    let output = clean(Path::new("git"), dir)
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        text(&output.stderr)
    );
}

/// A running server with the project `widgets`, its admin `alice` and its member `bob`, both
/// signed in, and the project's settings applied.
pub struct Team {
    pub server: Server,
    pub root: TempDir,
    pub alice: Person,
    pub bob: Person,
}

impl Team {
    /// Starts the server, adds the people and applies `settings` (TOML) as the admin.
    pub fn start(settings: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let (server, alice_token) = Server::start("widgets", "alice");
        let bob_token = server.add_member("bob");
        let alice = Person::sign_in(&server, root.path(), "alice", &alice_token);
        let bob = Person::sign_in(&server, root.path(), "bob", &bob_token);
        apply_settings(&alice, root.path(), settings);
        Self {
            server,
            root,
            alice,
            bob,
        }
    }

    /// A checkout named `name` under the team's root. `repo` is the repository id it governs, in
    /// a project that lists several.
    pub fn checkout(&self, name: &str, repo: Option<&str>, remote: &str) -> PathBuf {
        checkout(&self.root.path().join(name), &self.server, repo, remote)
    }
}
