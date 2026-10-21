//! How long `dibs hook neutral <event>` takes on a warm cache, process start included: the
//! settings, status and inbox fetched a moment ago, the heartbeat sent, so the hook asks the
//! server nothing — and an edit another member holds, which the hook asks the server about and
//! refuses, counting the refusal; and a `git push` judged against the protected branches from the
//! settings and the checkout's git settings alone: to a branch an exact rule protects, to one a
//! pattern protects (each refused and counted), to a branch of one's own, and a bare push from a
//! release branch of a checkout whose `HEAD` git must read (a reftable); and a push to a branch a
//! slot another member holds freezes by a pattern, which asks the server about that slot. Each case
//! also prints the 95th percentile of 100 runs. Reported, not gated: shared machines are too noisy to
//! gate on milliseconds.

mod support;

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};

use support::{edit_readme, p95, world};

/// An edit of a file in the area another member holds.
fn edit_held() -> Value {
    json!({"kind": "edit", "paths": ["src/billing/rates.rs"]})
}

/// A shell command the agent is about to run.
fn shell(command: &str) -> Value {
    json!({"kind": "shell", "command": command})
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
        (
            "before-tool, push refused by an exact branch",
            "before-tool",
            world.input(Some(shell("git push origin main"))),
        ),
        (
            "before-tool, push refused by a branch pattern",
            "before-tool",
            world.input(Some(shell("git push origin release/2026.10.3"))),
        ),
        (
            "before-tool, push to a branch of one's own",
            "before-tool",
            world.input(Some(shell("git push origin bob/42-rounding"))),
        ),
        (
            "before-tool, push to a branch a held slot freezes by a pattern",
            "before-tool",
            world.input(Some(shell("git push origin hotfix/7"))),
        ),
        (
            "before-tool, bare push from a release branch in a reftable checkout",
            "before-tool",
            world.input_in(&world.reftable, Some(shell("git push"))),
        ),
    ];
    for (name, event, input) in &cases {
        if let Err(error) = world.warm() {
            eprintln!("the cache could not be warmed: {error}");
            return;
        }
        if let Ok(out) = world.hook(event, input) {
            let answer: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
            println!("{name}: decision {}", answer["decision"]);
        }
        let before = world.server.requests();
        group.bench_function(*name, |b| b.iter(|| world.hook(event, input)));
        println!(
            "{name}: {} requests reached the server while it ran",
            world.server.requests() - before
        );
        match p95(&world, "neutral", event, input, 100) {
            Ok(ms) => println!("{name}: p95 of 100 runs {ms:.2} ms"),
            Err(error) => eprintln!("{name}: the runs failed: {error}"),
        }
    }
    group.finish();
}

criterion_group!(benches, warm_hook);
criterion_main!(benches);
