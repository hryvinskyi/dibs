//! Usage errors, a server that cannot be reached, and one that answers something that is not
//! dibs.

use std::path::Path;

use crate::support::{API_REMOTE, Setup, make_checkout, status, text};

/// Whether a file named `name` lies anywhere under `dir`.
fn holds_file(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let path = entry.path();
            path.file_name().is_some_and(|file| file == name) || holds_file(&path, name)
        })
    })
}

#[test]
fn usage_errors_exit_2() {
    let setup = Setup::new();
    for args in [
        &["frobnicate"][..],
        &["claim"],
        &["check"],
        &["items", "--status", "lost"],
    ] {
        let out = setup
            .dibs(&setup.bob, &setup.elsewhere())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(status(&out), 2, "{args:?}");
    }
    let version = setup
        .dibs(&setup.bob, &setup.elsewhere())
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(status(&version), 0);
    assert!(text(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn an_unreachable_server_exits_3_and_is_remembered() {
    let setup = Setup::new();
    let url = dibs_test_server::unreachable().unwrap();
    let dir = setup.root.path().join("offline");
    make_checkout(&dir, &url, "acme", Some("api"), API_REMOTE);
    let login = setup
        .dibs(&setup.bob, &dir)
        .arg("login")
        .write_stdin(setup.bob.token.clone())
        .output()
        .unwrap();
    assert_eq!(status(&login), 3);

    let out = setup
        .dibs(&setup.bob, &dir)
        .env("DIBS_TOKEN", &setup.bob.token)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 3);
    // A closed port refuses at once on Unix; on Windows the connect retries until it times out.
    let said = text(&out.stderr);
    assert!(
        said.contains("could not be reached") || said.contains("did not answer in time"),
        "{said}"
    );
    let answer = text(&out.stdout);
    assert!(
        answer.contains("\"network\"") || answer.contains("\"timeout\""),
        "{answer}"
    );
    assert!(holds_file(
        &setup.home(&setup.bob).join("state"),
        "unreachable"
    ));

    let range = setup
        .dibs(&setup.bob, &dir)
        .env("DIBS_TOKEN", &setup.bob.token)
        .args(["check", "--git-range", "HEAD~0..HEAD"])
        .output()
        .unwrap();
    assert_eq!(status(&range), 0);
    let warned = text(&range.stderr);
    assert!(
        warned.contains("could not be reached") || warned.contains("did not answer in time"),
        "{warned}"
    );
}

#[test]
fn a_challenge_page_in_front_of_the_server_exits_3_without_showing_it() {
    let setup = Setup::new();
    let url = dibs_test_server::fixed_answer(
        403,
        "text/html",
        "<html><title>Checking your browser</title></html>",
    )
    .unwrap();
    let dir = setup.root.path().join("proxied");
    make_checkout(&dir, &url, "acme", Some("api"), API_REMOTE);
    let out = setup
        .dibs(&setup.bob, &dir)
        .env("DIBS_TOKEN", &setup.bob.token)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(status(&out), 3);
    let stderr = text(&out.stderr);
    assert!(stderr.contains("does not understand"), "{stderr}");
    assert!(!stderr.contains("Checking your browser"), "{stderr}");
}

#[test]
fn an_invalid_configuration_file_exits_2_naming_it() {
    let setup = Setup::new();
    let dir = setup.root.path().join("broken");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(".dibs.toml"),
        "server = \"dibs.example.com\"\nproject = \"acme\"\n",
    )
    .unwrap();
    let out = setup.dibs(&setup.bob, &dir).arg("status").output().unwrap();
    assert_eq!(status(&out), 2);
    assert!(text(&out.stderr).contains(".dibs.toml"));
}

#[test]
fn a_person_waits_out_a_slow_server_and_a_hook_still_gives_up_within_its_limit() {
    use std::time::{Duration, Instant};

    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    let edit = serde_json::json!({
        "protocol": 1,
        "session": "sess-1234-abcd",
        "cwd": api.to_string_lossy(),
        "thread": "main",
        "tool": {"kind": "edit", "paths": ["src/billing/a.rs"]},
    })
    .to_string();
    let hook = || {
        setup
            .dibs(&setup.bob, &api)
            .args(["hook", "neutral", "before-tool"])
            .write_stdin(edit.clone())
            .output()
            .unwrap()
    };
    assert_eq!(
        status(&hook()),
        0,
        "a first hook warms this machine's cache"
    );
    setup.server.delay_answers(Duration::from_secs(6));

    let started = Instant::now();
    let out = setup.dibs(&setup.bob, &api).arg("whoami").output().unwrap();
    assert_eq!(status(&out), 0, "past the five seconds a hook may wait");
    assert!(text(&out.stdout).starts_with("bob"));
    assert!(started.elapsed() >= Duration::from_secs(6));

    let started = Instant::now();
    let out = hook();
    let took = started.elapsed();
    assert_eq!(status(&out), 0);
    let said = text(&out.stdout);
    assert!(
        said.contains("did not answer in time") || said.contains("could not be reached"),
        "the hook asked the server and gave up: {}",
        text(&out.stdout)
    );
    assert!(
        took < Duration::from_secs(4),
        "the hook kept its two-second limit: {took:?}"
    );
}

#[test]
fn a_connection_reset_under_a_persons_sign_in_is_tried_once_more() {
    let setup = Setup::new();
    let resetting = dibs_test_server::resetting_first_connection(&setup.server.url()).unwrap();
    let out = setup
        .dibs_as("bob-elsewhere", &setup.elsewhere())
        .args(["login", "--server", &resetting])
        .write_stdin(format!("{}\n", setup.bob.token))
        .output()
        .unwrap();
    assert_eq!(status(&out), 0, "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("as bob"));
}
