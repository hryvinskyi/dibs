//! How long `dibs hook neutral <event>` takes on a warm cache, process start included: the
//! settings, status and inbox fetched a moment ago, the heartbeat sent, so the hook asks the
//! server nothing — and an edit another member holds, which the hook asks the server about and
//! refuses, counting the refusal. Each case also prints the 95th percentile of 100 runs. Reported,
//! not gated: shared machines are too noisy to gate on milliseconds.

use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};
use tempfile::TempDir;

use dibs_domain::member::Role;
use dibs_test_server::{TestServer, two_repositories};

const REMOTE: &str = "https://github.com/acme/api.git";
const SESSION: &str = "bench-session";

type Setup<T> = Result<T, Box<dyn Error>>;

/// A server, a signed-in member and a checkout of theirs.
struct World {
    server: TestServer,
    home: PathBuf,
    api: PathBuf,
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

fn world() -> Setup<World> {
    let server = TestServer::start()?;
    let alice = server.project("acme", "alice")?;
    let bob = server.member(&alice, "bob", Role::Member)?;
    server.settings(&alice, two_repositories())?;
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let alices = root.path().join("alice");
    let api = root.path().join("api");
    fs::create_dir_all(&api)?;
    git(&api, &["init", "-q", "-b", "main"])?;
    git(&api, &["remote", "add", "origin", REMOTE])?;
    fs::write(
        api.join(".dibs.toml"),
        format!(
            "server = \"{}\"\nproject = \"acme\"\nrepo = \"api\"\n",
            server.url()
        ),
    )?;
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
    ] {
        if !dibs(&alices, &api, args, None)?.status.success() {
            return Err(format!("dibs {args:?} failed").into());
        }
    }
    Ok(World {
        server,
        home,
        api,
        _root: root,
    })
}

impl World {
    fn input(&self, tool: Option<Value>) -> String {
        let mut value = json!({
            "protocol": 1, "session": SESSION, "cwd": self.api.to_string_lossy(), "thread": "main"
        });
        if let Some(tool) = tool {
            value["tool"] = tool;
        }
        value.to_string()
    }

    /// Fills every cache and sends the heartbeat, so nothing is due for a while.
    fn warm(&self) -> Setup<()> {
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
        Ok(())
    }

    fn hook(&self, event: &str, input: &str) -> Setup<Output> {
        dibs(
            &self.home,
            &self.api,
            &["hook", "neutral", event],
            Some(input),
        )
    }
}

fn edit_readme() -> Value {
    json!({"kind": "edit", "paths": ["README.md"]})
}

fn edit_held() -> Value {
    json!({"kind": "edit", "paths": ["src/billing/rates.rs"]})
}

/// The 95th percentile of `runs` runs of the hook, in milliseconds.
fn p95(world: &World, event: &str, input: &str, runs: usize) -> Setup<f64> {
    let mut took = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        world.hook(event, input)?;
        took.push(started.elapsed().as_secs_f64() * 1_000.0);
    }
    took.sort_by(f64::total_cmp);
    let at = (runs * 95).div_ceil(100).saturating_sub(1);
    took.get(at).copied().ok_or_else(|| "no runs".into())
}

fn warm_hook(c: &mut Criterion) {
    let world = match world() {
        Ok(world) => world,
        Err(error) => {
            eprintln!("the warm hook benchmark could not be set up: {error}");
            return;
        }
    };
    let mut group = c.benchmark_group("warm hook");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(30);
    let cases = [
        (
            "before-tool, ungoverned path",
            "before-tool",
            world.input(Some(edit_readme())),
        ),
        ("after-tool", "after-tool", world.input(None)),
        (
            "before-tool, refused edit",
            "before-tool",
            world.input(Some(edit_held())),
        ),
    ];
    for (name, event, input) in &cases {
        if let Err(error) = world.warm() {
            eprintln!("the cache could not be warmed: {error}");
            return;
        }
        let before = world.server.requests();
        group.bench_function(*name, |b| b.iter(|| world.hook(event, input)));
        println!(
            "{name}: {} requests reached the server while it ran",
            world.server.requests() - before
        );
        match p95(&world, event, input, 100) {
            Ok(ms) => println!("{name}: p95 of 100 runs {ms:.2} ms"),
            Err(error) => eprintln!("{name}: the runs failed: {error}"),
        }
    }
    group.finish();
}

criterion_group!(benches, warm_hook);
criterion_main!(benches);
