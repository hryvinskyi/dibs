//! The edits a hook lets through without asking the server, carried by the next heartbeat of the
//! background sync: removed once the server accepted that heartbeat, kept when the sync failed.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::{API_REMOTE, Setup, json, status, text};

const SESSION: &str = "sess-5555-eeee";

/// The bodies of the heartbeats the server received since the `from`th request, in order.
fn heartbeats_since(setup: &Setup, from: usize) -> Vec<String> {
    setup.server.request_log()[from..]
        .iter()
        .filter(|request| request.starts_with("POST /v1/heartbeat"))
        .cloned()
        .collect()
}

/// `dibs sync --force` for the hook's session, run until it is not held back by a sync started in
/// the background; answers its exit status and what it printed.
fn sync(setup: &Setup, cwd: &Path) -> (i32, Value) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let out = setup
            .dibs_as("bob", cwd)
            .args(["sync", "--force", "--json", "--host", "neutral"])
            .args(["--session", SESSION])
            .output()
            .unwrap();
        let code = status(&out);
        if code != 0 {
            return (code, Value::Null);
        }
        let synced = json(&out);
        if synced["heartbeat"] == true {
            return (code, synced);
        }
        assert!(Instant::now() < deadline, "no heartbeat was sent: {synced}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn edit(setup: &Setup, cwd: &Path, path: &str) {
    let event = json!({
        "protocol": 1,
        "session": SESSION,
        "cwd": cwd.to_string_lossy(),
        "thread": "main",
        "tool": {"kind": "edit", "paths": [path]},
    });
    let out = setup
        .dibs_as("bob", cwd)
        .args(["hook", "neutral", "before-tool"])
        .write_stdin(event.to_string())
        .output()
        .unwrap();
    assert_eq!(status(&out), 0, "a hook always exits 0");
    assert_eq!(json(&out)["decision"], "no_objection");
}

#[test]
fn an_edit_let_through_while_the_server_is_down_reaches_the_next_heartbeat() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    for args in [
        vec!["new", "gh:acme/api#1", "--title", "Billing work"],
        vec!["claim", "gh:acme/api#1"],
    ] {
        let out = setup.dibs_as("bob", &api).args(&args).output().unwrap();
        assert_eq!(status(&out), 0, "{args:?}: {}", text(&out.stderr));
    }
    let (code, _) = sync(&setup, &api);
    assert_eq!(code, 0);

    setup.server.stop();
    edit(&setup, &api, "src/billing/invoice.rs");
    let before = setup.server.requests();
    setup.server.start_again().unwrap();
    let out = setup
        .dibs_as("bob", &api)
        .args(["status"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0, "{}", text(&out.stderr));
    let (code, _) = sync(&setup, &api);
    assert_eq!(code, 0);
    let (code, _) = sync(&setup, &api);
    assert_eq!(code, 0);

    let sent = heartbeats_since(&setup, before);
    let carrying: Vec<&String> = sent
        .iter()
        .filter(|body| body.contains(r#""edits":{"areas":["api:billing"],"outside":false}"#))
        .collect();
    assert_eq!(carrying.len(), 1, "carried once: {sent:?}");
    assert!(!carrying[0].contains("invoice"), "never the path: {sent:?}");
}

#[test]
fn the_next_heartbeat_carries_an_unchecked_edit_once_and_a_failed_sync_keeps_it() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    for args in [
        vec!["new", "gh:acme/api#1", "--title", "Billing work"],
        vec!["claim", "gh:acme/api#1"],
    ] {
        let out = setup.dibs_as("bob", &api).args(&args).output().unwrap();
        assert_eq!(status(&out), 0, "{args:?}: {}", text(&out.stderr));
    }
    let (code, synced) = sync(&setup, &api);
    assert_eq!(code, 0);
    assert_eq!(synced["edits"], false, "nothing was edited yet");

    edit(&setup, &api, "README.md");
    setup.server.stop();
    let (failed, _) = sync(&setup, &api);
    assert_ne!(failed, 0, "the server could not be reached");
    setup.server.start_again().unwrap();
    let out = setup
        .dibs_as("bob", &api)
        .args(["status"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0, "{}", text(&out.stderr));

    let before = setup.server.requests();
    let (code, synced) = sync(&setup, &api);
    assert_eq!(code, 0);
    assert_eq!(synced["edits"], true);
    let sent = heartbeats_since(&setup, before);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains(r#""edits":{"areas":[],"outside":true}"#),
        "{sent:?}"
    );
    assert!(!sent[0].contains("README"), "never the path: {sent:?}");

    let again = setup.server.requests();
    let (code, synced) = sync(&setup, &api);
    assert_eq!(code, 0);
    assert_eq!(synced["edits"], false);
    let resent = heartbeats_since(&setup, again);
    assert!(
        resent[0].contains(r#""edits":{"areas":[],"outside":false}"#),
        "sent once: {resent:?}"
    );
}
