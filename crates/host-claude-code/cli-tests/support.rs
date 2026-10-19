//! A server with the project `acme` over the repositories `api` and `web`, the admin `alice` and
//! the member `bob`, each signed in with directories of their own, git checkouts to run in, and the
//! Claude Code hook input of each event.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::TempDir;

use dibs_domain::member::Role;
use dibs_test_server::{TestServer, two_repositories};

pub const API_REMOTE: &str = "https://github.com/acme/api.git";

/// The session every hook input names.
pub const SESSION: &str = "5b9c0e1a-7f3d-4c2e-9a61-0d4e8f2b7c19";

pub struct Setup {
    pub server: TestServer,
    pub root: TempDir,
    pub bob_token: String,
}

impl Setup {
    /// The server and its people, both signed in through `dibs login`.
    pub fn new() -> Self {
        let server = TestServer::start().unwrap();
        let alice = server.project("acme", "alice").unwrap();
        let bob = server.member(&alice, "bob", Role::Member).unwrap();
        server.settings(&alice, two_repositories()).unwrap();
        let setup = Self {
            server,
            root: tempfile::tempdir().unwrap(),
            bob_token: bob.token.clone(),
        };
        for (handle, token) in [("alice", &alice.token), ("bob", &bob.token)] {
            let out = setup
                .dibs(handle, &setup.elsewhere())
                .args(["login", "--server", &setup.server.url()])
                .write_stdin(format!("{token}\n"))
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", text(&out.stderr));
        }
        setup
    }

    /// The person's home for dibs: `config/` and `state/`.
    pub fn home(&self, handle: &str) -> PathBuf {
        self.root.path().join("homes").join(handle)
    }

    /// A directory outside every checkout.
    pub fn elsewhere(&self) -> PathBuf {
        let dir = self.root.path().join("elsewhere");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `dibs` run as the person with this handle in `cwd`, its directories the person's own.
    pub fn dibs(&self, handle: &str, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dibs"));
        self.as_person(handle, cwd, command.env_remove("DIBS_OFF"));
        command.timeout(Duration::from_secs(60));
        command
    }

    /// Sets the person's directories and working directory on a command.
    pub fn as_person(&self, handle: &str, cwd: &Path, command: &mut Command) {
        let home = self.home(handle);
        command
            .current_dir(cwd)
            .env("DIBS_CONFIG_DIR", home.join("config"))
            .env("DIBS_STATE_DIR", home.join("state"))
            .env_remove("DIBS_TOKEN")
            .env_remove("DIBS_LOG");
    }

    /// `dibs` as a plain process whose standard streams the test wires itself.
    pub fn process(&self, handle: &str, cwd: &Path) -> std::process::Command {
        let home = self.home(handle);
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_dibs"));
        command
            .current_dir(cwd)
            .env("DIBS_CONFIG_DIR", home.join("config"))
            .env("DIBS_STATE_DIR", home.join("state"))
            .env_remove("DIBS_TOKEN")
            .env_remove("DIBS_LOG")
            .env_remove("DIBS_OFF");
        command
    }

    /// Runs a command as `handle` in `cwd` and expects it to succeed.
    pub fn ok(&self, handle: &str, cwd: &Path, args: &[&str]) -> Output {
        let out = self.dibs(handle, cwd).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        out
    }

    /// A git checkout at `root/<name>` with `origin` and a `.dibs.toml` naming `server`, the
    /// project and `repo`, one commit made.
    pub fn checkout_on(&self, name: &str, server: &str, repo: Option<&str>) -> PathBuf {
        let dir = self.root.path().join(name);
        fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "Bob"],
            &["config", "user.email", "bob@example.com"],
            &["config", "commit.gpgsign", "false"],
            &["remote", "add", "origin", API_REMOTE],
        ] {
            git(&dir, args);
        }
        let mut config = format!("server = \"{server}\"\nproject = \"acme\"\n");
        if let Some(repo) = repo {
            config.push_str(&format!("repo = \"{repo}\"\n"));
        }
        fs::write(dir.join(".dibs.toml"), config).unwrap();
        fs::write(dir.join("README.md"), "hello\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "start"]);
        dir
    }

    /// The checkout of `api` on this server.
    pub fn api(&self) -> PathBuf {
        self.checkout_on("api", &self.server.url(), Some("api"))
    }
}

/// Runs git in `dir`.
fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
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

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A hook input as Claude Code writes it for `event` in `cwd`, with the event's own fields.
pub fn hook_input(event: &str, cwd: &Path, extra: Value) -> String {
    let mut value = json!({
        "session_id": SESSION,
        "transcript_path": "/home/bob/.claude/projects/-work-api/5b9c0e1a.jsonl",
        "cwd": cwd.to_string_lossy(),
        "permission_mode": "default",
        "hook_event_name": event,
    });
    if let (Some(fields), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        fields.extend(more.clone());
    }
    value.to_string()
}

/// The `PreToolUse` input of a `Write` of `path` (absolute, or relative to `cwd`).
pub fn write_of(cwd: &Path, path: &str) -> String {
    let absolute = cwd.join(path);
    hook_input(
        "PreToolUse",
        cwd,
        json!({
            "tool_name": "Write",
            "tool_input": {"file_path": absolute.to_string_lossy(), "content": "x\n"},
            "tool_use_id": "toolu_01ABC123"
        }),
    )
}

/// The reply a hook printed, which it always prints with exit status 0: nothing, or one object.
pub fn reply(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "a hook always exits 0; stderr: {}",
        text(&output.stderr)
    );
    if output.stdout.iter().all(u8::is_ascii_whitespace) {
        return Value::Null;
    }
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("not one JSON object: {}", text(&output.stdout)))
}
