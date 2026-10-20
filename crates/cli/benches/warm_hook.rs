//! How long `dibs hook neutral <event>` takes on a warm cache, process start included: the
//! settings, status and inbox fetched a moment ago, the heartbeat sent, so the hook asks the
//! server nothing — and an edit another member holds, which the hook asks the server about and
//! refuses, counting the refusal. Each case also prints the 95th percentile of 100 runs. Reported,
//! not gated: shared machines are too noisy to gate on milliseconds.

mod support;

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};

use support::{edit_readme, p95, world};

/// An edit of a file in the area another member holds.
fn edit_held() -> Value {
    json!({"kind": "edit", "paths": ["src/billing/rates.rs"]})
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
        match p95(&world, "neutral", event, input, 100) {
            Ok(ms) => println!("{name}: p95 of 100 runs {ms:.2} ms"),
            Err(error) => eprintln!("{name}: the runs failed: {error}"),
        }
    }
    group.finish();
}

criterion_group!(benches, warm_hook);
criterion_main!(benches);
