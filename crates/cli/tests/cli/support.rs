//! A server with the project `acme` over two repositories, the admin `alice` and the member `bob`,
//! each with configuration and state directories of their own, and git checkouts to run in.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

use dibs_domain::member::Role;
use dibs_test_server::{Person, TestServer, two_repositories};

pub const API_REMOTE: &str = "https://github.com/acme/api.git";
pub const WEB_REMOTE: &str = "git@github.com:acme/web.git";

pub struct Setup {
    pub server: TestServer,
    pub alice: Person,
    pub bob: Person,
    pub root: TempDir,
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
            alice,
            bob,
            root: tempfile::tempdir().unwrap(),
        };
        for who in [&setup.alice, &setup.bob] {
            let out = setup.login(who, &who.token);
            assert!(out.status.success(), "{}", text(&out.stderr));
        }
        setup
    }

    /// The person's home for dibs: `config/` and `state/`.
    pub fn home(&self, who: &Person) -> PathBuf {
        self.home_of(&who.handle)
    }

    /// The home of the person with this handle.
    pub fn home_of(&self, handle: &str) -> PathBuf {
        self.root.path().join("homes").join(handle)
    }

    /// A directory outside every checkout.
    pub fn elsewhere(&self) -> PathBuf {
        let dir = self.root.path().join("elsewhere");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `dibs` run as `who` in `cwd`, its directories the person's own.
    pub fn dibs(&self, who: &Person, cwd: &Path) -> Command {
        self.dibs_as(&who.handle, cwd)
    }

    /// `dibs` run as the person with this handle.
    pub fn dibs_as(&self, handle: &str, cwd: &Path) -> Command {
        let home = self.home_of(handle);
        let mut command = Command::new(env!("CARGO_BIN_EXE_dibs"));
        command
            .current_dir(cwd)
            .env("DIBS_CONFIG_DIR", home.join("config"))
            .env("DIBS_STATE_DIR", home.join("state"))
            .env_remove("DIBS_TOKEN")
            .env_remove("DIBS_LOG")
            .timeout(std::time::Duration::from_secs(60));
        command
    }

    /// `dibs` as the person with this handle, as a plain process whose standard streams the test
    /// wires itself, its directories the person's own.
    pub fn process_as(&self, handle: &str, cwd: &Path) -> std::process::Command {
        let home = self.home_of(handle);
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_dibs"));
        command
            .current_dir(cwd)
            .env("DIBS_CONFIG_DIR", home.join("config"))
            .env("DIBS_STATE_DIR", home.join("state"))
            .env_remove("DIBS_TOKEN")
            .env_remove("DIBS_LOG");
        command
    }

    /// `dibs login --server <url>` as `who` with `token` on standard input, outside any checkout.
    pub fn login(&self, who: &Person, token: &str) -> Output {
        self.login_as(&who.handle, token)
    }

    /// `dibs login` as the person with this handle.
    pub fn login_as(&self, handle: &str, token: &str) -> Output {
        self.dibs_as(handle, &self.elsewhere())
            .args(["login", "--server", &self.server.url()])
            .write_stdin(format!("{token}\n"))
            .output()
            .unwrap()
    }

    /// A git checkout at `root/<name>` with `origin` and a `.dibs.toml` naming this server, the
    /// project and `repo`, one commit made.
    pub fn checkout(&self, name: &str, repo: Option<&str>, origin: &str) -> PathBuf {
        let dir = self.root.path().join(name);
        make_checkout(&dir, &self.server.url(), "acme", repo, origin);
        dir
    }
}

/// A git repository at `dir` with `origin`, a `.dibs.toml` and a first commit by `bob`.
pub fn make_checkout(dir: &Path, server: &str, project: &str, repo: Option<&str>, origin: &str) {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Bob"]);
    git(dir, &["config", "user.email", "bob@example.com"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["remote", "add", "origin", origin]);
    let mut config = format!("server = \"{server}\"\nproject = \"{project}\"\n");
    if let Some(repo) = repo {
        config.push_str(&format!("repo = \"{repo}\"\n"));
    }
    fs::write(dir.join(".dibs.toml"), config).unwrap();
    write(dir, "README.md", "hello\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "start"]);
}

/// Runs git in `dir` and answers its standard output.
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

/// Writes a file under `dir`, its directories made.
pub fn write(dir: &Path, path: &str, content: &str) {
    let file = dir.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, content).unwrap();
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("not JSON: {}", text(&output.stdout)))
}

/// The exit status, with the output shown when it is not the one expected.
pub fn status(output: &Output) -> i32 {
    let code = output.status.code().unwrap();
    eprintln!(
        "exit {code}\nstdout: {}\nstderr: {}",
        text(&output.stdout),
        text(&output.stderr)
    );
    code
}
