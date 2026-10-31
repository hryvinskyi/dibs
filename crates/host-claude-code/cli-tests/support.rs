//! A server with the project `acme` over the repositories `api` and `web`, the admin `alice` and
//! the member `bob`, each signed in with directories of their own, git checkouts to run in, one of
//! them lending a pool of worktrees, and the Claude Code hook input of each event.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::TempDir;

use dibs_domain::member::Role;
use dibs_test_server::{Person, TestServer, two_repositories};

pub const API_REMOTE: &str = "https://github.com/acme/api.git";

/// The session every hook input names.
pub const SESSION: &str = "5b9c0e1a-7f3d-4c2e-9a61-0d4e8f2b7c19";

pub struct Setup {
    pub server: TestServer,
    pub root: TempDir,
    pub alice: Person,
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
            alice: alice.clone(),
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

    /// A checkout `widgets` of `api` lending a pool of `count` worktrees beside it, its
    /// `.dibs.toml` kept out of git as a pool's checkout keeps it, and the scripts its pool's
    /// commands run: a preparation that adds the worktree, and a reset that checks out the base and
    /// cleans it.
    pub fn pool(&self, count: u8) -> PathBuf {
        let checkout = self.checkout_on("widgets", &self.server.url(), Some("api"));
        git(&checkout, &["rm", "-q", "--cached", ".dibs.toml"]);
        let exclude = checkout.join(".git/info/exclude");
        let mut excluded = fs::read_to_string(&exclude).unwrap_or_default();
        excluded.push_str("\n.dibs.toml\n.claude/\n");
        fs::write(&exclude, excluded).unwrap();
        git(
            &checkout,
            &["commit", "-q", "-m", "keep the configuration local"],
        );
        let kit = self.root.path().join("kit");
        fs::create_dir_all(&kit).unwrap();
        for (name, script) in [("prepare.sh", PREPARE), ("reset.sh", RESET)] {
            fs::write(kit.join(name), script).unwrap();
        }
        let mut config = fs::read_to_string(checkout.join(".dibs.toml")).unwrap();
        config.push_str(&format!(
            "\n[worktrees]\ncount = {count}\nbase = \"main\"\nprepare = \"sh '{}'\"\nreset = \"sh '{}'\"\n",
            kit.join("prepare.sh").display(),
            kit.join("reset.sh").display(),
        ));
        fs::write(checkout.join(".dibs.toml"), config).unwrap();
        checkout
    }

    /// The directory of the worktree `name` of the pool beside `widgets`, as the file system
    /// resolves it.
    pub fn pool_worktree(&self, name: &str) -> PathBuf {
        canonical(&self.root.path().join("widgets-worktrees").join(name))
    }

    /// This machine's file of `what` for the worktree `name` of the person's pool
    /// (`worktree1.beat`), wherever the pool's state lies.
    pub fn worktree_state(&self, handle: &str, name: &str, what: &str) -> PathBuf {
        let base = self.home(handle).join("state/worktrees");
        for scope in fs::read_dir(&base).unwrap() {
            for pool in fs::read_dir(scope.unwrap().path()).unwrap() {
                let dir = pool.unwrap().path();
                if dir.is_dir() {
                    return dir.join(format!("{name}.{what}"));
                }
            }
        }
        panic!("no pool state under {}", base.display());
    }

    /// `dibs worktree list --json` as `handle` in `cwd`: the pool's worktrees, their holds and
    /// what this machine remembers of them.
    pub fn worktrees(&self, handle: &str, cwd: &Path) -> Vec<Value> {
        let out = self.ok(handle, cwd, &["worktree", "list", "--json"]);
        let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
        listed["worktrees"].as_array().unwrap().clone()
    }
}

/// The preparation of a pool worktree: a detached worktree of the checkout at the base.
const PREPARE: &str = r#"#!/bin/sh
if [ ! -e "$DIBS_WORKTREE_PATH/.git" ]; then
  git -C "$DIBS_CHECKOUT" worktree add -q --detach "$DIBS_WORKTREE_PATH" "$DIBS_WORKTREE_BASE" || exit 6
fi
"#;

/// The reset of a pool worktree: the base checked out and everything git does not ignore cleaned.
const RESET: &str = r#"#!/bin/sh
cd "$DIBS_WORKTREE_PATH" && git checkout -q --detach "$DIBS_WORKTREE_BASE" && git clean -ffdq
"#;

/// Runs git in `dir`; answers what it printed.
pub fn git(dir: &Path, args: &[&str]) -> String {
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
    text(&output.stdout)
}

/// The path as the file system resolves it, or as it is when it does not exist.
pub fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The last line a command printed on standard output, as a path the file system resolves.
pub fn last_line(output: &Output) -> PathBuf {
    let stdout = text(&output.stdout);
    canonical(Path::new(stdout.lines().last().unwrap_or_default()))
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
