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
    assert!(text(&out.stderr).contains("could not be reached"));
    assert!(text(&out.stdout).contains("\"network\""));
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
    assert!(text(&range.stderr).contains("could not be reached"));
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
