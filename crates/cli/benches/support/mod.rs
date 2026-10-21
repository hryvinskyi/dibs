//! A server with a project and a member signed in, a checkout of theirs, and `dibs hook` run there
//! on a warm cache, process start included: what the hook benchmarks share. The checkout's
//! repository protects `main` and every `release/` branch for another member, who holds a slot
//! that freezes every `hotfix/` branch; a second checkout of it keeps its refs in a reftable.

use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Instant;

use serde_json::{Value, json};
use tempfile::TempDir;

use dibs_domain::member::Role;
use dibs_test_server::{TestServer, two_repositories};

pub const REMOTE: &str = "https://github.com/acme/api.git";
pub const SESSION: &str = "bench-session";

pub type Setup<T> = Result<T, Box<dyn Error>>;

/// A server, a signed-in member and a checkout of theirs.
pub struct World {
    pub server: TestServer,
    home: PathBuf,
    pub api: PathBuf,
    /// A checkout of the same repository whose refs git keeps in a reftable, on a release branch.
    pub reftable: PathBuf,
    _root: TempDir,
}

fn dibs(home: &Path, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Setup<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dibs"))
        .current_dir(cwd)
        .env("DIBS_CONFIG_DIR", home.join("config"))
        .env("DIBS_STATE_DIR", home.join("state"))
        .env_remove("DIBS_TOKEN")
        .env_remove("DIBS_OFF")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut pipe) = child.stdin.take()
        && let Some(text) = stdin
    {
        pipe.write_all(text.as_bytes())?;
    }
    Ok(child.wait_with_output()?)
}

fn git(dir: &Path, args: &[&str]) -> Setup<()> {
    let status = Command::new("git").arg("-C").arg(dir).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {args:?} failed").into())
    }
}

pub fn world() -> Setup<World> {
    let server = TestServer::start()?;
    let alice = server.project("acme", "alice")?;
    let bob = server.member(&alice, "bob", Role::Member)?;
    let mut settings = two_repositories();
    settings["repos"][0]["protected_refs"] = json!([
        {"ref": "refs/heads/main", "pushers": ["alice"]},
        {"ref": "refs/heads/release/*", "pushers": ["alice"]}
    ]);
    settings["slots"] = json!([
        {"name": "hotfix:freeze", "max_hold": "24h", "freezes": ["api:refs/heads/hotfix/*"]}
    ]);
    server.settings(&alice, settings)?;
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let alices = root.path().join("alice");
    let api = root.path().join("api");
    let reftable = root.path().join("api-reftable");
    for (dir, format) in [(&api, "files"), (&reftable, "reftable")] {
        fs::create_dir_all(dir)?;
        git(
            dir,
            &[
                "init",
                "-q",
                "-b",
                "main",
                &format!("--ref-format={format}"),
            ],
        )?;
        git(dir, &["remote", "add", "origin", REMOTE])?;
        fs::write(
            dir.join(".dibs.toml"),
            format!(
                "server = \"{}\"\nproject = \"acme\"\nrepo = \"api\"\n",
                server.url()
            ),
        )?;
    }
    git(&reftable, &["checkout", "-q", "-b", "release/2.4"])?;
    for (who, token) in [(&home, &bob.token), (&alices, &alice.token)] {
        let login = dibs(
            who,
            root.path(),
            &["login", "--server", &server.url()],
            Some(&format!("{token}\n")),
        )?;
        if !login.status.success() {
            return Err("dibs login failed".into());
        }
    }
    for args in [
        &["new", "gh:acme/api#1", "--title", "Billing"][..],
        &["claim", "gh:acme/api#1", "--area", "billing"],
        &[
            "slot",
            "take",
            "hotfix:freeze",
            "--purpose",
            "hotfix review",
        ],
    ] {
        if !dibs(&alices, &api, args, None)?.status.success() {
            return Err(format!("dibs {args:?} failed").into());
        }
    }
    Ok(World {
        server,
        home,
        api,
        reftable,
        _root: root,
    })
}

impl World {
    /// The neutral host's input for an event of the session, with a tool when given.
    pub fn input(&self, tool: Option<Value>) -> String {
        self.input_in(&self.api, tool)
    }

    /// [`World::input`] for a session working in `cwd`.
    pub fn input_in(&self, cwd: &Path, tool: Option<Value>) -> String {
        let mut value = json!({
            "protocol": 1, "session": SESSION, "cwd": cwd.to_string_lossy(), "thread": "main"
        });
        if let Some(tool) = tool {
            value["tool"] = tool;
        }
        value.to_string()
    }

    /// Fills every cache, for both checkouts, and sends the heartbeat, so nothing is due for a
    /// while.
    pub fn warm(&self) -> Setup<()> {
        let sync = dibs(
            &self.home,
            &self.api,
            &[
                "sync",
                "--force",
                "--quiet",
                "--host",
                "neutral",
                "--session",
                SESSION,
            ],
            None,
        )?;
        if !sync.status.success() {
            return Err("dibs sync failed".into());
        }
        self.hook("before-tool", &self.input(Some(edit_readme())))?;
        self.hook(
            "before-tool",
            &self.input_in(&self.reftable, Some(edit_readme())),
        )?;
        Ok(())
    }

    /// `dibs hook neutral <event>` with `input`.
    pub fn hook(&self, event: &str, input: &str) -> Setup<Output> {
        self.hook_of("neutral", event, input)
    }

    /// `dibs hook <host> <event>` with `input`.
    pub fn hook_of(&self, host: &str, event: &str, input: &str) -> Setup<Output> {
        dibs(&self.home, &self.api, &["hook", host, event], Some(input))
    }
}

pub fn edit_readme() -> Value {
    json!({"kind": "edit", "paths": ["README.md"]})
}

/// The 95th percentile of `runs` runs of `dibs hook <host> <event>`, in milliseconds.
pub fn p95(world: &World, host: &str, event: &str, input: &str, runs: usize) -> Setup<f64> {
    let mut took = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        world.hook_of(host, event, input)?;
        took.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    took.sort_by(f64::total_cmp);
    let at = (runs * 95).div_ceil(100).saturating_sub(1);
    took.get(at).copied().ok_or_else(|| "no runs".into())
}
