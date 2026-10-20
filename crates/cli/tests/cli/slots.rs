//! Slots through the command line: taking, waiting for an offer, releasing, leaving, the list, the
//! verdict of a settings file, a takeover and an admin's forced release, and `dibs slot run` with
//! a command that fails, one ended by a signal, an interruption passed to the command, and a hold
//! taken over while the command runs.

use std::io::Read;
use std::process::Stdio;
use std::time::{Duration as StdDuration, Instant};

use serde_json::{Value, json};
use time::Duration;

use dibs_client::server_url::ServerUrl;
use dibs_client::token::Token;
use dibs_client::{Client, ClientOptions};
use dibs_test_server::two_repositories;

use crate::support::{API_REMOTE, Setup, json, status, text, write};

/// The setup with two slots: `deploy:prod`, freezing `api`'s `main`, and `migrate`, which runs
/// only inside `dibs slot run`.
fn setup() -> Setup {
    let setup = Setup::new();
    let mut settings = two_repositories();
    settings["slots"] = json!([
        {"name": "deploy:prod", "max_hold": "30m", "freezes": ["api:refs/heads/main"],
         "commands": [{"matches": "^deploy\\.sh prod"}]},
        {"name": "migrate", "max_hold": "1h", "run_only": true}
    ]);
    setup.server.settings(&setup.alice, settings).unwrap();
    setup
}

/// The kinds and payloads of the project's events, read as alice.
fn events(setup: &Setup) -> Vec<(String, Value)> {
    let client = Client::new(
        ServerUrl::parse(&setup.server.url()).unwrap(),
        Token::parse(&setup.alice.token).unwrap(),
        ClientOptions::default(),
    )
    .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(client.events(None, None))
        .unwrap()
        .events
        .into_iter()
        .map(|event| {
            let value = serde_json::to_value(&event.kind).unwrap();
            (
                value["kind"].as_str().unwrap().to_owned(),
                value["payload"].clone(),
            )
        })
        .collect()
}

fn released(setup: &Setup) -> Vec<Value> {
    events(setup)
        .into_iter()
        .filter(|(kind, _)| kind == "slot_released")
        .map(|(_, payload)| payload)
        .collect()
}

#[test]
fn a_take_queues_the_second_member_who_waits_for_the_offer_and_takes_it() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let alice = |args: &[&str]| setup.dibs(&setup.alice, &api).args(args).output().unwrap();
    let bob = |args: &[&str]| setup.dibs(&setup.bob, &api).args(args).output().unwrap();

    let taken = alice(&["slot", "take", "deploy:prod", "--purpose", "ship 2.4"]);
    assert_eq!(status(&taken), 0);
    assert!(
        text(&taken.stdout).starts_with("You hold deploy:prod until "),
        "{}",
        text(&taken.stdout)
    );
    let queued = bob(&[
        "slot",
        "take",
        "deploy:prod",
        "--purpose",
        "hotfix",
        "--json",
    ]);
    assert_eq!(status(&queued), 1, "still queued");
    assert_eq!(json(&queued)["outcome"], "queued");
    assert_eq!(json(&queued)["position"], 1);
    let shown = bob(&["slot", "take", "deploy:prod", "--purpose", "hotfix"]);
    assert_eq!(status(&shown), 1);
    let said = text(&shown.stdout);
    assert!(
        said.contains("held by alice (ship 2.4)") && said.contains("number 1 in its queue"),
        "{said}"
    );

    let mut waiting = setup
        .process_as("bob", &api)
        .args([
            "slot",
            "take",
            "deploy:prod",
            "--purpose",
            "hotfix",
            "--wait",
            "20s",
            "--poll-every",
            "1s",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(StdDuration::from_millis(1500));
    let released = alice(&["slot", "release", "deploy:prod"]);
    assert_eq!(status(&released), 0);
    assert!(text(&released.stdout).contains("It is offered to bob until"));
    let started = Instant::now();
    let done = waiting.wait().unwrap();
    let mut out = String::new();
    waiting
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert_eq!(done.code(), Some(0), "{out}");
    assert!(out.starts_with("You hold deploy:prod"), "{out}");
    assert!(started.elapsed() < StdDuration::from_secs(10));

    let list = alice(&["slot", "list"]);
    assert_eq!(status(&list), 0);
    let listed = text(&list.stdout);
    assert!(
        listed.contains("deploy:prod: held by bob (hotfix), until"),
        "{listed}"
    );
    assert!(listed.contains("migrate: free"), "{listed}");
    let left = alice(&["slot", "leave", "deploy:prod"]);
    assert_eq!(status(&left), 1);
    assert!(
        text(&left.stderr).contains("not waiting"),
        "{}",
        text(&left.stderr)
    );
    let freed = bob(&["slot", "release", "deploy:prod"]);
    assert_eq!(status(&freed), 0);
}

#[test]
fn a_wait_longer_than_nine_minutes_and_a_bad_until_run_are_refused_before_any_call() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let long = setup
        .dibs(&setup.alice, &api)
        .args([
            "slot",
            "take",
            "deploy:prod",
            "--purpose",
            "x",
            "--wait",
            "10m",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&long), 2);
    let bad = setup
        .dibs(&setup.alice, &api)
        .args([
            "slot",
            "take",
            "deploy:prod",
            "--purpose",
            "x",
            "--until-run",
            "deploy.yml",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&bad), 2);
    let until = setup
        .dibs(&setup.alice, &api)
        .args([
            "slot",
            "take",
            "deploy:prod",
            "--purpose",
            "x",
            "--until-run",
            "deploy.yml@abc1234",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&until), 0);
    assert_eq!(
        json(&until)["slot"]["hold"]["liveness"],
        json!({"mode": "until_run", "workflow": "deploy.yml", "sha": "abc1234"})
    );
}

#[test]
fn slot_test_prints_the_verdict_of_a_settings_file_without_a_server() {
    let setup = setup();
    let dir = setup.elsewhere();
    write(
        &dir,
        "slots.toml",
        "[[slots]]\nname = \"deploy:prod\"\nmax_hold = \"30m\"\nfreezes = [\"refs/heads/main\"]\n\
         commands = [{ matches = '^deploy\\.sh prod', unless = '--dry-run' }]\n\
         [[slots]]\nname = \"migrate\"\nmax_hold = \"1h\"\nrun_only = true\n\
         commands = [{ matches = '^make migrate' }]\n",
    );
    let test = |line: &str| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_dibs"))
            .current_dir(&dir)
            .env("DIBS_CONFIG_DIR", dir.join("none"))
            .env("DIBS_STATE_DIR", dir.join("none"))
            .args(["--json", "slot", "test", "slots.toml", "--", line])
            .output()
            .unwrap();
        assert_eq!(status(&output), 0);
        json(&output)
    };
    let deploy = test("cd ops && FOO=1 ./deploy.sh prod");
    assert_eq!(deploy["verdict"], "needs_slot");
    assert_eq!(deploy["slot"], "deploy:prod");
    assert_eq!(deploy["commands"], json!(["cd ops", "deploy.sh prod"]));
    assert_eq!(test("./deploy.sh prod --dry-run")["verdict"], "free");
    assert_eq!(test("echo './deploy.sh prod'")["verdict"], "free");
    let migrate = test("make migrate");
    assert_eq!(
        (migrate["slot"].clone(), migrate["run_only"].clone()),
        (json!("migrate"), json!(true))
    );
    let push = test("git push origin main");
    assert_eq!(push["verdict"], "free");
    assert_eq!(push["frozen_while_held"], json!(["deploy:prod"]));
    assert_eq!(
        test("git push origin feature")["frozen_while_held"],
        json!([])
    );
    let invalid = std::process::Command::new(env!("CARGO_BIN_EXE_dibs"))
        .current_dir(&dir)
        .args(["slot", "test", "missing.toml", "--", "ls"])
        .output()
        .unwrap();
    assert_eq!(status(&invalid), 2);
}

#[test]
fn slot_run_releases_with_the_commands_exit_status_and_exits_with_it() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let ran = setup
        .dibs(&setup.alice, &api)
        .args(["slot", "run", "migrate", "--purpose", "invoices", "--"])
        .args(shell("echo migrating; exit 3"))
        .output()
        .unwrap();
    assert_eq!(status(&ran), 3);
    assert_eq!(
        text(&ran.stdout).trim(),
        "migrating",
        "the command's output is its own"
    );
    let said = text(&ran.stderr);
    assert!(said.contains("Released migrate."), "{said}");
    assert!(said.contains("The command ended with status 3."), "{said}");
    let payloads = released(&setup);
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["exit"], json!({"code": 3}));

    let fine = setup
        .dibs(&setup.alice, &api)
        .args(["slot", "run", "migrate", "--purpose", "invoices", "--"])
        .args(shell("exit 0"))
        .output()
        .unwrap();
    assert_eq!(status(&fine), 0);
    assert_eq!(released(&setup)[1]["exit"], json!({"code": 0}));
}

#[test]
fn slot_run_does_not_run_the_command_while_another_member_holds_the_slot() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let taken = setup
        .dibs(&setup.bob, &api)
        .args(["slot", "take", "migrate", "--purpose", "first"])
        .output()
        .unwrap();
    assert_eq!(status(&taken), 0);
    let marker = setup.elsewhere().join("ran");
    let ran = setup
        .dibs(&setup.alice, &api)
        .args(["slot", "run", "migrate", "--purpose", "second", "--"])
        .args(shell(&format!("touch '{}'", marker.display())))
        .output()
        .unwrap();
    assert_eq!(status(&ran), 1);
    assert!(!marker.exists(), "the command never ran");
}

#[test]
fn slot_run_keeps_running_when_its_hold_is_taken_over_and_releases_nothing() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let gate = setup.elsewhere().join("gate");
    let mut running = setup
        .process_as("alice", &api)
        .args([
            "slot",
            "run",
            "migrate",
            "--purpose",
            "invoices",
            "--heartbeat-every",
            "1s",
            "--",
        ])
        .args(shell(&format!(
            "while [ ! -e '{}' ]; do sleep 0.2; done; echo finished",
            gate.display()
        )))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    loop {
        let list = setup
            .dibs(&setup.bob, &api)
            .args(["--json", "slot", "list"])
            .output()
            .unwrap();
        if json(&list)["slots"][1]["hold"]["holder"] == "alice" {
            break;
        }
        assert!(
            started.elapsed() < StdDuration::from_secs(20),
            "the run never took the slot"
        );
        std::thread::sleep(StdDuration::from_millis(100));
    }
    let mut attempts = 0;
    loop {
        setup.server.advance(Duration::minutes(3));
        let taken = setup
            .dibs(&setup.bob, &api)
            .args([
                "slot",
                "takeover",
                "migrate",
                "--purpose",
                "finish it",
                "--reason",
                "the migration log stopped; checked the table",
            ])
            .output()
            .unwrap();
        if taken.status.success() {
            break;
        }
        attempts += 1;
        assert!(
            attempts < 5 && text(&taken.stderr).contains("not stalled"),
            "a heartbeat came between the clock and the takeover too often: {}",
            text(&taken.stderr)
        );
    }
    std::thread::sleep(StdDuration::from_millis(2500));
    std::fs::write(&gate, "").unwrap();
    let done = running.wait().unwrap();
    let mut out = String::new();
    let mut err = String::new();
    running
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    running
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut err)
        .unwrap();
    assert_eq!(done.code(), Some(0), "{err}");
    assert_eq!(out.trim(), "finished");
    assert_eq!(
        err.matches("took migrate over while this command ran")
            .count(),
        1,
        "{err}"
    );
    assert!(!err.contains("Released"), "{err}");
    assert!(released(&setup).is_empty(), "nothing released");
    let list = setup
        .dibs(&setup.bob, &api)
        .args(["--json", "slot", "list"])
        .output()
        .unwrap();
    assert_eq!(json(&list)["slots"][1]["hold"]["holder"], "bob");
}

#[test]
fn an_admin_forces_a_release_through_the_command_line() {
    let setup = setup();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let taken = setup
        .dibs(&setup.bob, &api)
        .args(["slot", "take", "deploy:prod", "--purpose", "deploy"])
        .output()
        .unwrap();
    assert_eq!(status(&taken), 0);
    let refused = setup
        .dibs(&setup.bob, &api)
        .args([
            "admin",
            "slot",
            "force-release",
            "deploy:prod",
            "--reason",
            "x",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&refused), 1);
    let forced = setup
        .dibs(&setup.alice, &api)
        .args([
            "admin",
            "slot",
            "force-release",
            "deploy:prod",
            "--reason",
            "cancelled",
        ])
        .output()
        .unwrap();
    assert_eq!(status(&forced), 0);
    assert!(text(&forced.stdout).contains("Ended bob's hold on deploy:prod"));
}

/// The command line that runs `script` in the system's shell.
/// The scripts are POSIX shell; on Windows the `sh` that ships with Git for Windows runs them.
fn shell(script: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

#[cfg(unix)]
mod signals {
    use super::*;

    #[test]
    fn a_command_ended_by_a_signal_is_released_with_that_signal() {
        let setup = setup();
        let api = setup.checkout("api", Some("api"), API_REMOTE);
        let ran = setup
            .dibs(&setup.alice, &api)
            .args(["slot", "run", "migrate", "--purpose", "invoices", "--"])
            .args(shell("kill -TERM $$"))
            .output()
            .unwrap();
        assert_eq!(status(&ran), 128 + 15);
        assert!(text(&ran.stderr).contains("The command ended on SIGTERM."));
        assert_eq!(released(&setup)[0]["exit"], json!({"signal": "SIGTERM"}));
    }

    /// Starts `dibs slot run` over a shell that writes `ready` once it runs, says `stopping` and
    /// exits 0 on `SIGTERM`, says `interrupted` and exits 0 on `SIGINT`, and otherwise ends by itself
    /// once `gate` exists, saying `finished`. Answers once the command runs.
    fn run_until_gate(
        setup: &Setup,
        api: &std::path::Path,
        name: &str,
    ) -> (std::process::Child, std::path::PathBuf) {
        let dir = setup.elsewhere().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let ready = dir.join("ready");
        let gate = dir.join("gate");
        let running = setup
            .process_as("alice", api)
            .args(["slot", "run", "migrate", "--purpose", "invoices", "--"])
            .args(shell(&format!(
                "trap 'echo stopping; exit 0' TERM; trap 'echo interrupted; exit 0' INT; \
                 touch '{}'; while [ ! -e '{}' ]; do sleep 0.1; done; echo finished",
                ready.display(),
                gate.display()
            )))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        while !ready.exists() {
            assert!(
                started.elapsed() < StdDuration::from_secs(20),
                "the command never started"
            );
            std::thread::sleep(StdDuration::from_millis(50));
        }
        (running, gate)
    }

    fn signal(child: &std::process::Child, name: &str) {
        let sent = std::process::Command::new("kill")
            .args([&format!("-{name}"), &child.id().to_string()])
            .status()
            .unwrap();
        assert!(sent.success());
    }

    fn output(mut child: std::process::Child) -> (Option<i32>, String, String) {
        let done = child.wait().unwrap();
        let mut out = String::new();
        let mut err = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .unwrap();
        (done.code(), out, err)
    }

    #[test]
    fn a_sigterm_to_the_wrapper_is_passed_to_the_command_and_released_with() {
        let setup = setup();
        let api = setup.checkout("api", Some("api"), API_REMOTE);
        let (running, _gate) = run_until_gate(&setup, &api, "term");
        signal(&running, "TERM");
        let (code, out, err) = output(running);
        assert_eq!(out.trim(), "stopping", "the command got the signal: {err}");
        assert_eq!(code, Some(0), "the command chose its status: {err}");
        assert_eq!(released(&setup)[0]["exit"], json!({"signal": "SIGTERM"}));
    }

    #[test]
    fn a_sigint_to_the_wrapper_alone_leaves_the_command_running_until_it_ends_by_itself() {
        let setup = setup();
        let api = setup.checkout("api", Some("api"), API_REMOTE);
        let (mut running, gate) = run_until_gate(&setup, &api, "int");
        signal(&running, "INT");
        std::thread::sleep(StdDuration::from_millis(800));
        assert!(
            running.try_wait().unwrap().is_none(),
            "neither the wrapper nor the command stopped"
        );
        assert!(released(&setup).is_empty(), "nothing released yet");
        std::fs::write(&gate, "").unwrap();
        let (code, out, err) = output(running);
        assert_eq!(
            out.trim(),
            "finished",
            "the command never got a SIGINT: {err}"
        );
        assert_eq!(code, Some(0), "{err}");
        assert_eq!(
            released(&setup)[0]["exit"],
            json!({"code": 0}),
            "released with the command's own outcome"
        );
    }
}
