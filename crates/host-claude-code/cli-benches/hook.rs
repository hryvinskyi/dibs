//! How long `dibs hook claude-code <event>` takes on a warm cache for the events that stamp whether
//! the session works or waits — after a tool, and at the end of a turn — beside the neutral host's
//! after-tool, which stamps nothing, so the difference is what a stamp costs. Each case also prints
//! the 95th percentile of 100 runs. Reported, not gated.
//!
//! This is a benchmark of the `dibs` command-line crate, which builds the binary it runs; its
//! source lives beside the adapter, so that only the adapter's directory names the host.

#[path = "../../cli/benches/support/mod.rs"]
mod support;

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::json;

use support::{SESSION, p95, world};

fn stamped_hook(c: &mut Criterion) {
    let world = match world() {
        Ok(world) => world,
        Err(error) => {
            eprintln!("the stamped hook benchmark could not be set up: {error}");
            return;
        }
    };
    let cwd = world.api.to_string_lossy().into_owned();
    let input = |event: &str, extra: serde_json::Value| {
        let mut value = json!({
            "session_id": SESSION, "cwd": cwd, "permission_mode": "default",
            "hook_event_name": event,
            "transcript_path": "/home/bob/.claude/projects/-work-api/bench.jsonl",
        });
        if let (Some(fields), Some(more)) = (value.as_object_mut(), extra.as_object()) {
            fields.extend(more.clone());
        }
        value.to_string()
    };
    let mut group = c.benchmark_group("stamped hook");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(30);
    let cases = [
        (
            "claude-code post-tool-use, stamps that it works",
            "claude-code",
            "post-tool-use",
            input(
                "PostToolUse",
                json!({"tool_name": "Read", "tool_input": {"file_path": "README.md"}, "tool_response": {}}),
            ),
        ),
        (
            "claude-code stop, stamps that it waits",
            "claude-code",
            "stop",
            input(
                "Stop",
                json!({"stop_hook_active": false, "last_assistant_message": "Done."}),
            ),
        ),
        (
            "neutral after-tool, no stamp",
            "neutral",
            "after-tool",
            world.input(None),
        ),
    ];
    for (name, host, event, input) in &cases {
        if let Err(error) = world.warm() {
            eprintln!("the cache could not be warmed: {error}");
            return;
        }
        let before = world.server.requests();
        group.bench_function(*name, |b| b.iter(|| world.hook_of(host, event, input)));
        println!(
            "{name}: {} requests reached the server while it ran",
            world.server.requests() - before
        );
        match p95(&world, host, event, input, 100) {
            Ok(ms) => println!("{name}: p95 of 100 runs {ms:.2} ms"),
            Err(error) => eprintln!("{name}: the runs failed: {error}"),
        }
    }
    group.finish();
}

criterion_group!(benches, stamped_hook);
criterion_main!(benches);
