//! `dibs hook neutral <event>` over standard input: the guard's verdicts, the cache that keeps a
//! hook off the network, the background sync a hook starts and never waits for, the switch, and
//! every failure answered as no objection.

use std::path::Path;
use std::process::Output;
use std::time::{Duration as StdDuration, Instant};

use serde_json::{Value, json};

use dibs_test_server::unreachable;

use crate::support::{API_REMOTE, Setup, json, make_checkout, status, text, write};

/// The neutral input of an event in `cwd`.
fn input(cwd: &Path, tool: Option<Value>) -> String {
    let mut value = json!({
        "protocol": 1,
        "session": "sess-1234-abcd",
        "cwd": cwd.to_string_lossy(),
        "thread": "main",
    });
    if let Some(tool) = tool {
        value["tool"] = tool;
    }
    value.to_string()
}

fn edit(cwd: &Path, paths: &[&str]) -> String {
    input(cwd, Some(json!({"kind": "edit", "paths": paths})))
}

impl Setup {
    /// `dibs hook neutral <event>` as `handle` in `cwd`, with `stdin`.
    fn hook(&self, handle: &str, cwd: &Path, event: &str, stdin: &str) -> Output {
        self.dibs_as(handle, cwd)
            .args(["hook", "neutral", event])
            .write_stdin(stdin.to_owned())
            .output()
            .unwrap()
    }

    /// Runs a command in `cwd` as `handle` and expects it to succeed.
    fn ok(&self, handle: &str, cwd: &Path, args: &[&str]) {
        let out = self.dibs_as(handle, cwd).args(args).output().unwrap();
        assert_eq!(status(&out), 0, "{args:?}");
    }
}

/// The reply, which every hook prints with exit status 0.
fn reply(output: &Output) -> Value {
    assert_eq!(status(output), 0, "a hook always exits 0");
    json(output)
}

fn decision(output: &Output) -> String {
    reply(output)["decision"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Waits until `check` holds, at most `limit`.
fn eventually(limit: StdDuration, check: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if check() {
            return true;
        }
        std::thread::sleep(StdDuration::from_millis(50));
    }
    check()
}

fn claim(setup: &Setup, handle: &str, api: &Path, reference: &str, areas: &[&str]) {
    setup.ok(handle, api, &["new", reference, "--title", "Billing work"]);
    let mut args = vec!["claim", reference];
    for area in areas {
        args.extend(["--area", area]);
    }
    setup.ok(handle, api, &args);
}

#[test]
fn a_path_another_member_holds_is_denied_naming_them() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    claim(&setup, "alice", &api, "gh:acme/api#1", &["billing"]);
    let out = setup.hook(
        "bob",
        &api,
        "before-tool",
        &edit(&api, &["src/billing/a.rs"]),
    );
    let answer = reply(&out);
    assert_eq!(answer["decision"], "deny", "{answer}");
    let reason = answer["reason"].as_str().unwrap();
    assert!(reason.contains("alice holds for work item"), "{reason}");
}

#[test]
fn a_free_path_is_no_objection_and_its_area_is_reserved() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    claim(&setup, "bob", &api, "gh:acme/api#2", &[]);
    let absolute = api.join("src/billing/a.rs");
    let out = setup.hook(
        "bob",
        &api,
        "before-tool",
        &edit(&api, &[&absolute.to_string_lossy()]),
    );
    assert_eq!(
        reply(&out),
        json!({"protocol": 1, "decision": "no_objection"})
    );
    let state = setup
        .dibs(&setup.bob, &api)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&state), 0);
    let holds = json(&state)["holds"].clone();
    assert!(
        holds
            .as_array()
            .unwrap()
            .iter()
            .any(|hold| hold["area"] == "api:billing" && hold["holder"] == "bob"),
        "{holds}"
    );
}

#[test]
fn without_a_claim_a_governed_path_is_denied_with_the_way_to_claim() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let out = setup.hook(
        "bob",
        &api,
        "before-tool",
        &edit(&api, &["src/search/q.rs"]),
    );
    let answer = reply(&out);
    assert_eq!(answer["decision"], "deny");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("--area api:search"),
        "{answer}"
    );
}

#[test]
fn an_unreachable_server_is_a_notice_and_then_a_minute_without_calls() {
    let setup = Setup::new();
    let server = unreachable().unwrap();
    let dir = setup.root.path().join("away");
    make_checkout(&dir, &server, "acme", Some("api"), API_REMOTE);
    let run = || {
        setup
            .dibs_as("bob", &dir)
            .env("DIBS_TOKEN", &setup.bob.token)
            .args(["hook", "neutral", "before-tool"])
            .write_stdin(edit(&dir, &["src/billing/a.rs"]))
            .output()
            .unwrap()
    };
    let first = reply(&run());
    assert_eq!(first["decision"], "no_objection");
    let notice = first["notice"].as_str().unwrap();
    // A closed port refuses at once on Unix; on Windows the connect retries until it times out.
    assert!(
        notice.contains("could not be reached") || notice.contains("did not answer in time"),
        "{notice}"
    );
    let started = Instant::now();
    let second = reply(&run());
    assert_eq!(second, json!({"protocol": 1, "decision": "no_objection"}));
    assert!(
        started.elapsed() < StdDuration::from_secs(1),
        "the circuit kept it off the network"
    );
}

#[test]
fn a_warm_cache_answers_without_a_single_request() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    claim(&setup, "bob", &api, "gh:acme/api#3", &["billing"]);
    setup.ok(
        "bob",
        &api,
        &[
            "sync",
            "--force",
            "--update",
            "--host",
            "neutral",
            "--session",
            "sess-1234-abcd",
        ],
    );
    let warm_up = setup.hook("bob", &api, "before-tool", &edit(&api, &["README.md"]));
    assert_eq!(decision(&warm_up), "no_objection");
    let before = setup.server.requests();
    for (event, stdin) in [
        ("after-tool", input(&api, None)),
        ("prompt-submitted", input(&api, None)),
        ("before-tool", edit(&api, &["README.md", "docs/guide.md"])),
        ("before-tool", edit(&api, &["src/billing/held.rs"])),
        ("session-started", input(&api, None)),
    ] {
        let out = setup.hook("bob", &api, event, &stdin);
        assert_eq!(decision(&out), "no_objection", "{event}");
    }
    assert_eq!(
        setup.server.request_paths()[before..],
        [] as [String; 0],
        "no hook asked the server"
    );
}

#[test]
fn a_due_heartbeat_starts_one_background_sync_and_the_hook_does_not_wait_for_it() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    claim(&setup, "bob", &api, "gh:acme/api#4", &[]);
    let registered = setup.hook(
        "bob",
        &api,
        "before-tool",
        &edit(&api, &["src/billing/a.rs"]),
    );
    assert_eq!(decision(&registered), "no_objection");
    assert_eq!(setup.server.requests_to("/v1/heartbeat"), 0);
    setup.server.delay_answers(StdDuration::from_secs(3));
    let started = Instant::now();
    let out = setup.hook("bob", &api, "after-tool", &input(&api, None));
    let took = started.elapsed();
    assert_eq!(decision(&out), "no_objection");
    assert!(
        took < StdDuration::from_millis(2_500),
        "the hook waited {took:?}"
    );
    assert!(
        eventually(StdDuration::from_secs(5), || setup
            .server
            .requests_to("/v1/heartbeat")
            == 1),
        "the background sync sent the heartbeat: {:?}",
        setup.server.request_paths()
    );
    let again = setup.hook("bob", &api, "after-tool", &input(&api, None));
    assert_eq!(decision(&again), "no_objection");
    std::thread::sleep(StdDuration::from_secs(4));
    assert_eq!(
        setup.server.requests_to("/v1/heartbeat"),
        1,
        "a second hook started no second sync: {:?}",
        setup.server.request_paths()
    );
}

#[test]
fn the_background_sync_outlives_the_hook_that_started_it() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    claim(&setup, "bob", &api, "gh:acme/api#5", &[]);
    setup.hook(
        "bob",
        &api,
        "before-tool",
        &edit(&api, &["src/billing/a.rs"]),
    );
    setup.server.delay_answers(StdDuration::from_millis(800));
    let started = Instant::now();
    let out = setup.hook("bob", &api, "after-tool", &input(&api, None));
    let ended = started.elapsed();
    assert_eq!(decision(&out), "no_objection");
    // On Windows a child inherits the hook's own output pipe, so the hook's caller sees the pipe
    // close only when the background sync ends; the standard library offers no safe way to stop
    // that inheritance yet.
    #[cfg(not(windows))]
    assert!(
        ended < StdDuration::from_millis(800),
        "the hook took {ended:?}"
    );
    assert!(
        eventually(StdDuration::from_secs(8), || setup
            .server
            .requests_to("/v1/inbox")
            == 1),
        "the sync went on after the hook ended: {:?}",
        setup.server.request_paths()
    );
    assert!(started.elapsed() > StdDuration::from_millis(1_600));
}

#[test]
fn switched_off_a_hook_is_silent_and_asks_nothing() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let before = setup.server.requests();
    for (event, stdin) in [
        ("session-started", input(&api, None)),
        ("before-tool", edit(&api, &["src/billing/a.rs"])),
        ("after-tool", input(&api, None)),
    ] {
        let out = setup
            .dibs_as("bob", &api)
            .env("DIBS_OFF", "1")
            .args(["hook", "neutral", event])
            .write_stdin(stdin)
            .output()
            .unwrap();
        assert_eq!(
            reply(&out),
            json!({"protocol": 1, "decision": "no_objection"})
        );
    }
    std::thread::sleep(StdDuration::from_millis(500));
    assert_eq!(setup.server.requests(), before);
}

#[test]
fn a_session_starts_with_a_brief() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    claim(&setup, "alice", &api, "gh:acme/api#6", &["search"]);
    let out = setup.hook("bob", &api, "session-started", &input(&api, None));
    let context = reply(&out)["context"].as_str().unwrap().to_owned();
    assert!(context.contains("you are bob"), "{context}");
    assert!(context.contains("Other members' live claims:"), "{context}");
    assert!(context.contains("alice, areas api:search"), "{context}");
}

#[test]
fn a_brief_names_the_messages_that_ask_me_for_an_ack() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let sent = setup
        .dibs(&setup.alice, &api)
        .args([
            "send",
            "--to",
            "bob",
            "--subject",
            "Staging is down",
            "--importance",
            "urgent",
            "--ack",
            "Do not deploy.",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&sent), 0, "{}", text(&sent.stderr));
    let message = crate::support::json(&sent)["message"]["id"]
        .as_u64()
        .unwrap();
    let out = setup.hook("bob", &api, "session-started", &input(&api, None));
    let context = reply(&out)["context"].as_str().unwrap().to_owned();
    assert!(
        context.contains(&format!(
            "- urgent from alice: \"Staging is down\" (message {message})"
        )),
        "{context}"
    );
    let alices = setup.hook("alice", &api, "session-started", &input(&api, None));
    let context = reply(&alices)["context"].as_str().unwrap().to_owned();
    assert!(
        !context.contains("ask for your acknowledgement"),
        "never one's own message: {context}"
    );
}

#[test]
fn whatever_goes_wrong_a_hook_exits_0_without_a_decision() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let elsewhere = setup.elsewhere();
    let run = |cwd: &Path, args: &[&str], stdin: &str| {
        setup
            .dibs_as("bob", cwd)
            .args(args)
            .write_stdin(stdin.to_owned())
            .output()
            .unwrap()
    };
    let unknown_host = run(
        &api,
        &["hook", "elsewhere", "before-tool"],
        &input(&api, None),
    );
    assert_eq!(status(&unknown_host), 0);
    assert!(unknown_host.stdout.is_empty());
    assert!(text(&unknown_host.stderr).contains("'elsewhere'"));
    for args in [
        &["hook", "neutral", "pre-tool-use"][..],
        &["hook", "neutral"][..],
    ] {
        let out = run(&api, args, &edit(&api, &["src/billing/a.rs"]));
        let answer = reply(&out);
        assert_eq!(answer["decision"], "no_objection", "{args:?}");
        assert!(answer["notice"].as_str().is_some(), "{args:?}: {answer}");
    }
    let newer = run(
        &api,
        &["hook", "neutral", "before-tool", "--from-a-newer-plugin"],
        &edit(&api, &["README.md"]),
    );
    assert_eq!(
        reply(&newer),
        json!({"protocol": 1, "decision": "no_objection"}),
        "an option this dibs does not know does not stop the hook"
    );
    let garbage = run(&api, &["hook", "neutral", "before-tool"], "{ not json");
    assert!(reply(&garbage)["notice"].as_str().is_some());
    let outside = run(
        &elsewhere,
        &["hook", "neutral", "before-tool"],
        &edit(&elsewhere, &["a.rs"]),
    );
    assert_eq!(
        reply(&outside),
        json!({"protocol": 1, "decision": "no_objection"})
    );
    write(&api, ".dibs.toml", "server = 12\n");
    let broken = run(
        &api,
        &["hook", "neutral", "before-tool"],
        &edit(&api, &["a.rs"]),
    );
    let notice = reply(&broken)["notice"].as_str().unwrap().to_owned();
    assert!(notice.contains(".dibs.toml"), "{notice}");
}

#[test]
fn a_token_kept_in_the_os_store_that_cannot_be_read_is_no_decision_and_one_warning() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let file = setup
        .home(&setup.bob)
        .join("config")
        .join("credentials.toml");
    let kept_in_store = format!(
        "[[credential]]\nserver = \"{}\"\nproject = \"acme\"\nkeychain = true\n",
        setup.server.url()
    );
    let me = dibs_client::private_file::Principal::from_env(&|name| std::env::var(name).ok());
    dibs_client::private_file::write(&file, &kept_in_store, &me).unwrap();
    let started = Instant::now();
    let out = setup.hook(
        "bob",
        &api,
        "before-tool",
        &edit(&api, &["src/billing/a.rs"]),
    );
    let answer = reply(&out);
    assert_eq!(answer["decision"], "no_objection", "{answer}");
    let notice = answer["notice"].as_str().unwrap_or_default();
    assert!(notice.contains("dibs login"), "{answer}");
    assert!(
        started.elapsed() < StdDuration::from_secs(10),
        "the hook never waits on the OS store"
    );
}

#[test]
fn doctor_lists_the_hosts_and_what_they_carry() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let out = setup
        .dibs(&setup.bob, &api)
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    let report = json(&out);
    assert_eq!(report["hosts"][0]["id"], "neutral");
    assert_eq!(report["hosts"][0]["capabilities"]["deny_edit"], true);
    assert_eq!(report["checkout"]["signed_in"], true);
    assert_eq!(report["switched_off"], false);
    let human = setup.dibs(&setup.bob, &api).arg("doctor").output().unwrap();
    let shown = text(&human.stdout);
    assert!(shown.contains("neutral: carries"), "{shown}");
    assert!(shown.contains("nothing to install"), "{shown}");
}

#[test]
fn an_edit_check_names_the_branch_the_checkout_is_on_and_a_detached_head_none() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    crate::support::git(&api, &["checkout", "-q", "-b", "bob/5-rates"]);
    claim(&setup, "bob", &api, "gh:acme/api#5", &[]);
    let edited = setup.hook(
        "bob",
        &api,
        "before-tool",
        &edit(&api, &["src/billing/a.rs"]),
    );
    assert_eq!(decision(&edited), "no_objection");
    let sent: Vec<String> = setup
        .server
        .request_log()
        .into_iter()
        .filter(|request| request.contains("/v1/edit-check"))
        .collect();
    assert!(
        sent.iter()
            .any(|request| request.contains(r#""branch":"bob/5-rates""#)),
        "{sent:?}"
    );
    let state = setup
        .dibs(&setup.bob, &api)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        json(&state)["live"][0]["claim"]["branches"],
        json!({"api": "bob/5-rates"})
    );

    crate::support::git(&api, &["checkout", "-q", "--detach"]);
    let before = setup.server.request_log().len();
    let detached = setup.hook(
        "bob",
        &api,
        "before-tool",
        &edit(&api, &["src/search/a.rs"]),
    );
    assert_eq!(decision(&detached), "no_objection");
    let after: Vec<String> = setup.server.request_log()[before..]
        .iter()
        .filter(|request| request.contains("/v1/edit-check"))
        .cloned()
        .collect();
    assert!(
        !after.is_empty() && after.iter().all(|request| !request.contains("\"branch\"")),
        "{after:?}"
    );
}
