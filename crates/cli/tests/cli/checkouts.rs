//! The checkout a sync names to the server: a token keyed with a secret this machine keeps in its
//! state directory, readable by its user alone, so the token tells nobody without the machine where
//! the checkout is; only a session that works in the checkout names it, never the sync a command
//! of the command line starts to send what the hooks counted; and every session sends its own
//! heartbeat each interval, so the server hears from each session that shares a checkout.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::support::{API_REMOTE, Setup, git, json, status, text};

/// The bodies of the heartbeats the server received since the `from`th request, in order.
fn heartbeats_since(setup: &Setup, from: usize) -> Vec<String> {
    setup.server.request_log()[from..]
        .iter()
        .filter(|request| request.starts_with("POST /v1/heartbeat"))
        .cloned()
        .collect()
}

/// The checkout token a request names, when it names one.
fn token_of(request: &str) -> Option<String> {
    let start = request.find(r#""checkout":""#)? + r#""checkout":""#.len();
    request.get(start..start + 32).map(str::to_owned)
}

/// `dibs sync --force` as `handle` in `cwd`, run again until it sends a heartbeat: a sync started
/// in the background may hold the project's sync lock for a moment.
fn sync_with_heartbeat(setup: &Setup, handle: &str, cwd: &Path, extra: &[&str]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let out = setup
            .dibs_as(handle, cwd)
            .args(["sync", "--force", "--json"])
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(status(&out), 0, "{}", text(&out.stderr));
        let synced = json(&out);
        if synced["heartbeat"] == true {
            return synced;
        }
        assert!(Instant::now() < deadline, "no heartbeat was sent: {synced}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The branches of the claim `dibs status` shows first, as `handle` in `cwd`.
fn claim_branches(setup: &Setup, handle: &str, cwd: &Path) -> Value {
    let out = setup
        .dibs_as(handle, cwd)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0, "{}", text(&out.stderr));
    json(&out)["live"][0]["claim"]["branches"].clone()
}

#[test]
fn the_checkout_token_is_keyed_with_a_secret_only_this_machine_keeps() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let state = setup.home(&setup.bob).join("state");
    let key = state.join("checkout-key");
    assert!(!key.exists(), "no key before a token is needed");

    let before = setup.server.request_log().len();
    sync_with_heartbeat(&setup, "bob", &api, &[]);
    assert!(
        key.is_file(),
        "the first sync that names its checkout keeps a key"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "only its user may read the key: {mode:o}");
    }
    let sent: Vec<String> = heartbeats_since(&setup, before)
        .iter()
        .filter_map(|request| token_of(request))
        .collect();
    assert_eq!(sent.len(), 1, "one heartbeat named the checkout: {sent:?}");
    let token = &sent[0];

    let machine: String =
        serde_json::from_slice(&fs::read(state.join("machine-id")).unwrap()).unwrap();
    let root = fs::canonicalize(&api).unwrap();
    let mut unkeyed = machine.into_bytes();
    unkeyed.push(0);
    unkeyed.extend_from_slice(root.to_string_lossy().as_bytes());
    let digest: String = Sha256::digest(&unkeyed)[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_ne!(
        *token, digest,
        "the machine's id and the path, both of which the server may know, do not give the token"
    );

    let kept = fs::read(&key).unwrap();
    let again = setup.server.request_log().len();
    sync_with_heartbeat(&setup, "bob", &api, &[]);
    let resent: Vec<String> = heartbeats_since(&setup, again)
        .iter()
        .filter_map(|request| token_of(request))
        .collect();
    assert_eq!(
        resent,
        std::slice::from_ref(token),
        "the same checkout, the same token"
    );
    assert_eq!(fs::read(&key).unwrap(), kept, "the key is made once");
}

#[test]
fn the_sync_a_command_starts_to_send_the_hooks_counts_names_no_checkout() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let other = setup.checkout("api-two", Some("api"), API_REMOTE);
    git(&api, &["checkout", "-q", "-b", "bob/5-rates"]);
    for args in [
        &["new", "gh:acme/api#5", "--title", "Rates"][..],
        &["claim", "gh:acme/api#5"][..],
    ] {
        let out = setup.dibs_as("bob", &api).args(args).output().unwrap();
        assert_eq!(status(&out), 0, "{args:?}: {}", text(&out.stderr));
    }
    sync_with_heartbeat(&setup, "bob", &api, &[]);
    assert_eq!(
        claim_branches(&setup, "bob", &api),
        json!({"api": "bob/5-rates"}),
        "the command line's session took the claim and works alone in its checkout"
    );

    let before = setup.server.request_log().len();
    sync_with_heartbeat(
        &setup,
        "bob",
        &other,
        &["--host", "neutral", "--session", "sess-1234-abcd"],
    );
    let host_token = heartbeats_since(&setup, before)
        .iter()
        .find_map(|request| token_of(request))
        .expect("the host's session names the checkout it works in");

    let counted = setup
        .dibs_as("bob", &other)
        .env("DIBS_OFF", "1")
        .args(["hook", "neutral", "session-started"])
        .write_stdin(
            json!({"protocol": 1, "session": "sess-5678-efgh", "cwd": other, "thread": "main"})
                .to_string(),
        )
        .output()
        .unwrap();
    assert_eq!(status(&counted), 0);
    let reporting = setup.server.request_log().len();
    let shown = setup
        .dibs_as("bob", &other)
        .args(["status"])
        .output()
        .unwrap();
    assert_eq!(status(&shown), 0, "{}", text(&shown.stderr));
    let deadline = Instant::now() + Duration::from_secs(20);
    let report = loop {
        let found = heartbeats_since(&setup, reporting)
            .into_iter()
            .find(|request| request.contains(r#""switched_off":1"#));
        if let Some(found) = found {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "the command started no sync that reports"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        token_of(&report),
        None,
        "the sync that only reports works in no checkout: {report}"
    );
    assert!(
        !report.contains(r#""branch":"#),
        "nor on a branch: {report}"
    );
    assert!(
        heartbeats_since(&setup, 0)
            .iter()
            .filter(|request| token_of(request).as_deref() == Some(host_token.as_str()))
            .all(|request| !request.contains(r#""switched_off""#)),
        "only the host's session named its checkout"
    );

    git(&api, &["checkout", "-q", "-b", "bob/5-next"]);
    sync_with_heartbeat(&setup, "bob", &api, &[]);
    assert_eq!(
        claim_branches(&setup, "bob", &api),
        json!({"api": "bob/5-next"}),
        "the command line's session still reported its own checkout alone"
    );
}

/// `dibs sync --json` as bob in `cwd`, sending only what is due, run again while another sync
/// holds the project's lock.
fn sync_due(setup: &Setup, cwd: &Path, extra: &[&str]) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let out = setup
            .dibs_as("bob", cwd)
            .args(["sync", "--json"])
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(status(&out), 0, "{}", text(&out.stderr));
        let synced = json(&out);
        if synced["synced"] != false {
            return synced;
        }
        assert!(
            Instant::now() < deadline,
            "the sync lock stayed taken: {synced}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The session a heartbeat request is for.
fn session_of(request: &str) -> Option<u64> {
    let start = request.find(r#""session_id":"#)? + r#""session_id":"#.len();
    let digits: String = request[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The branches of the claim on `item` among the live claims `dibs status` shows as bob.
fn branches_of(setup: &Setup, cwd: &Path, item: u64) -> Value {
    let out = setup
        .dibs_as("bob", cwd)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0, "{}", text(&out.stderr));
    let shown = json(&out);
    shown["live"]
        .as_array()
        .unwrap()
        .iter()
        .find(|live| live["claim"]["item"] == item)
        .unwrap_or_else(|| panic!("no live claim on {item}: {shown}"))["claim"]["branches"]
        .clone()
}

const HOST_SESSION: [&str; 4] = ["--host", "neutral", "--session", "sess-1234-abcd"];

#[test]
fn every_session_in_one_checkout_sends_its_own_heartbeat_each_interval() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    git(&api, &["checkout", "-q", "-b", "bob/5-rates"]);
    let mut items = Vec::new();
    for args in [
        &["new", "gh:acme/api#5", "--title", "Rates", "--json"][..],
        &["new", "gh:acme/api#6", "--title", "Search", "--json"][..],
        &["claim", "gh:acme/api#5"][..],
    ] {
        let out = setup.dibs_as("bob", &api).args(args).output().unwrap();
        assert_eq!(status(&out), 0, "{args:?}: {}", text(&out.stderr));
        if args[0] == "new" {
            items.push(json(&out)["item"]["id"].as_u64().unwrap());
        }
    }
    let (rates, search) = (items[0], items[1]);

    let before = setup.server.request_log().len();
    let host = sync_due(&setup, &api, &HOST_SESSION);
    assert_eq!(
        host["heartbeat"], true,
        "the first heartbeat is due: {host}"
    );
    let host_session = heartbeats_since(&setup, before)
        .iter()
        .find_map(|request| session_of(request))
        .expect("the host's session sent a heartbeat");
    setup
        .server
        .claim(&setup.bob, search, host_session, &[])
        .unwrap();

    let line = sync_due(&setup, &api, &[]);
    assert_eq!(
        line["heartbeat"], true,
        "another session's heartbeat a moment ago holds back none of this session's: {line}"
    );
    let again = sync_due(&setup, &api, &HOST_SESSION);
    assert_eq!(
        again["heartbeat"], false,
        "a session's own heartbeat holds back its next one for an interval: {again}"
    );

    let reports = heartbeats_since(&setup, before);
    let sessions: Vec<Option<u64>> = reports.iter().map(|request| session_of(request)).collect();
    assert_eq!(sessions.len(), 2, "one heartbeat per session: {reports:?}");
    assert_ne!(sessions[0], sessions[1], "{reports:?}");
    let tokens: Vec<Option<String>> = reports.iter().map(|request| token_of(request)).collect();
    assert!(tokens[0].is_some(), "{reports:?}");
    assert_eq!(
        tokens[0], tokens[1],
        "both name the one checkout: {reports:?}"
    );

    sync_with_heartbeat(&setup, "bob", &api, &HOST_SESSION);
    assert_eq!(
        branches_of(&setup, &api, rates),
        json!({}),
        "the command line's session shares its checkout, so its claim takes no branch from it"
    );
    assert_eq!(
        branches_of(&setup, &api, search),
        json!({}),
        "nor does the host's session's claim"
    );
}

#[test]
fn a_session_that_ends_leaves_no_heartbeat_moment_behind() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    for extra in [&HOST_SESSION[..], &[][..]] {
        let synced = sync_due(&setup, &api, extra);
        assert_eq!(synced["heartbeat"], true, "{extra:?}: {synced}");
    }
    let held_back = sync_due(&setup, &api, &HOST_SESSION);
    assert_eq!(held_back["heartbeat"], false, "{held_back}");

    let ended = setup
        .dibs_as("bob", &api)
        .args(["hook", "neutral", "session-ended"])
        .write_stdin(
            json!({"protocol": 1, "session": "sess-1234-abcd", "cwd": api, "thread": "main"})
                .to_string(),
        )
        .output()
        .unwrap();
    assert_eq!(status(&ended), 0, "{}", text(&ended.stderr));

    let line = sync_due(&setup, &api, &[]);
    assert_eq!(
        line["heartbeat"], false,
        "another session's moment stays: {line}"
    );
    let after = sync_due(&setup, &api, &HOST_SESSION);
    assert_eq!(
        after["heartbeat"], true,
        "the ended session's moment went with it, so nothing holds a heartbeat of its key back: {after}"
    );
}
