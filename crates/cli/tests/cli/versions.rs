//! A server that serves only newer clients, the daily check for a newer client that `dibs sync
//! --update` makes, and `dibs self-update` refusing a release its build cannot trust.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::TempDir;

use dibs_test_server::{Person, TestServer, two_repositories};

use crate::support::{API_REMOTE, make_checkout, status, text};

/// A server with the client versions given, the project `acme`, its admin `alice`, and a checkout
/// of `api` on it.
struct Gated {
    server: TestServer,
    alice: Person,
    root: TempDir,
    api: PathBuf,
}

impl Gated {
    fn new(min: Option<&str>, latest: Option<&str>) -> Self {
        let server = TestServer::with_client_versions(min, latest).unwrap();
        let alice = server.project("acme", "alice").unwrap();
        server.settings(&alice, two_repositories()).unwrap();
        let root = tempfile::tempdir().unwrap();
        let api = root.path().join("api");
        make_checkout(&api, &server.url(), "acme", Some("api"), API_REMOTE);
        Self {
            server,
            alice,
            root,
            api,
        }
    }

    /// `dibs` (the binary at `exe`) as alice in the checkout, her token handed over directly and
    /// releases read from an empty directory.
    fn run_binary(&self, exe: &Path, args: &[&str], stdin: &str) -> Output {
        let home = self.root.path().join("home");
        let releases = self.root.path().join("no-releases");
        fs::create_dir_all(&releases).unwrap();
        Command::new(exe)
            .current_dir(&self.api)
            .env("DIBS_CONFIG_DIR", home.join("config"))
            .env("DIBS_STATE_DIR", home.join("state"))
            .env("DIBS_TOKEN", &self.alice.token)
            .env(
                "DIBS_RELEASE_URL",
                url::Url::from_directory_path(&releases).unwrap().as_str(),
            )
            .env_remove("DIBS_LOG")
            .env_remove("DIBS_OFF")
            .args(args)
            .write_stdin(stdin.to_owned())
            .timeout(std::time::Duration::from_secs(60))
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str], stdin: &str) -> Output {
        self.run_binary(Path::new(env!("CARGO_BIN_EXE_dibs")), args, stdin)
    }

    fn input(&self, tool: Option<Value>) -> String {
        let mut value = json!({
            "protocol": 1,
            "session": "sess-1234-abcd",
            "cwd": self.api.to_string_lossy(),
            "thread": "main",
        });
        if let Some(tool) = tool {
            value["tool"] = tool;
        }
        value.to_string()
    }
}

fn reply(output: &Output) -> Value {
    assert_eq!(
        status(output),
        0,
        "a hook always exits 0: {}",
        text(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn a_hook_answered_426_gives_no_decision_one_warning_and_then_leaves_the_server_alone() {
    let gated = Gated::new(Some("9.0.0"), Some("9.0.0"));
    let edit = gated.input(Some(json!({"kind": "edit", "paths": ["src/billing/a.rs"]})));
    let first = reply(&gated.run(&["hook", "neutral", "before-tool"], &edit));
    assert_eq!(first["decision"], "no_objection", "{first}");
    let notice = first["notice"].as_str().unwrap();
    assert!(notice.contains("9.0.0 or newer"), "{notice}");
    assert!(notice.contains("dibs self-update"), "{notice}");
    let asked = gated.server.requests();
    let second = reply(&gated.run(&["hook", "neutral", "before-tool"], &edit));
    assert_eq!(second, json!({"protocol": 1, "decision": "no_objection"}));
    assert_eq!(
        gated.server.requests(),
        asked,
        "no request within the minute"
    );
}

#[test]
fn a_command_answered_426_says_how_to_update_and_exits_3() {
    let gated = Gated::new(Some("9.0.0"), None);
    let out = gated.run(&["status"], "");
    assert_eq!(status(&out), 3, "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("dibs self-update"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn the_check_for_a_newer_client_is_made_at_most_once_a_day_and_only_when_asked() {
    let gated = Gated::new(None, Some(env!("CARGO_PKG_VERSION")));
    let sync = |extra: &[&str]| {
        let mut args = vec!["sync", "--quiet", "--host", "neutral", "--session", "s-1"];
        args.extend_from_slice(extra);
        let out = gated.run(&args, "");
        assert_eq!(status(&out), 0, "{}", text(&out.stderr));
    };
    sync(&[]);
    assert_eq!(gated.server.requests_to("/v1/version"), 0, "not asked for");
    sync(&["--update"]);
    assert_eq!(gated.server.requests_to("/v1/version"), 1);
    sync(&["--update"]);
    sync(&["--update", "--force"]);
    assert_eq!(
        gated.server.requests_to("/v1/version"),
        2,
        "once a day, unless forced"
    );
}

#[test]
fn a_newer_client_the_build_cannot_trust_is_not_installed_and_the_brief_names_it() {
    let gated = Gated::new(None, Some("9.0.0"));
    let exe = gated
        .root
        .path()
        .join("bin")
        .join(if cfg!(windows) { "dibs.exe" } else { "dibs" });
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_dibs"), &exe).unwrap();
    let before = fs::read(&exe).unwrap();
    let out = gated.run_binary(&exe, &["sync", "--update"], "");
    assert_eq!(status(&out), 0, "{}", text(&out.stderr));
    let said = text(&out.stdout);
    if cfg!(windows) {
        assert!(
            said.contains("dibs 9.0.0 is recommended; run dibs self-update"),
            "on Windows the background only tells: {said}"
        );
    } else {
        assert!(
            said.contains("dibs 9.0.0 was not installed"),
            "elsewhere the background tries to install: {said}"
        );
    }
    assert_eq!(fs::read(&exe).unwrap(), before, "the binary was kept");
    let brief = reply(&gated.run_binary(
        &exe,
        &["hook", "neutral", "session-started"],
        &gated.input(None),
    ));
    let context = brief["context"].as_str().unwrap();
    assert!(
        context.contains(&format!(
            "[DIBS] update dibs: {} → 9.0.0",
            env!("CARGO_PKG_VERSION")
        )),
        "{context}"
    );
}

/// A release directory whose release `version`, also its newest, is signed by a key made for this
/// test.
fn signed_release(root: &Path, version: &str) -> String {
    let pair = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let sums = format!(
        "{}  dibs-{version}-x86_64-unknown-linux-gnu.tar.gz\n",
        "0".repeat(64)
    );
    let signature = minisign::sign(
        Some(&pair.pk),
        &pair.sk,
        std::io::Cursor::new(sums.as_bytes()),
        None,
        None,
    )
    .unwrap();
    for dir in [
        root.join("latest/download"),
        root.join(format!("download/v{version}")),
    ] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SHA256SUMS"), &sums).unwrap();
        fs::write(dir.join("SHA256SUMS.minisig"), signature.to_string()).unwrap();
    }
    url::Url::from_directory_path(root).unwrap().to_string()
}

#[test]
fn self_update_refuses_a_release_not_signed_by_the_key_built_in_and_keeps_the_binary() {
    let root = tempfile::tempdir().unwrap();
    let releases = signed_release(&root.path().join("releases"), "9.0.0");
    let exe = root
        .path()
        .join(if cfg!(windows) { "dibs.exe" } else { "dibs" });
    fs::copy(env!("CARGO_BIN_EXE_dibs"), &exe).unwrap();
    let before = fs::read(&exe).unwrap();
    for args in [&["self-update"][..], &["self-update", "--version", "9.0.0"]] {
        let out = Command::new(&exe)
            .current_dir(root.path())
            .env("DIBS_STATE_DIR", root.path().join("state"))
            .env("DIBS_CONFIG_DIR", root.path().join("config"))
            .env("DIBS_RELEASE_URL", &releases)
            .args(args)
            .timeout(std::time::Duration::from_secs(60))
            .output()
            .unwrap();
        assert_eq!(status(&out), 1, "{args:?}: {}", text(&out.stderr));
        let said = text(&out.stderr);
        assert!(
            said.contains("no release signing key") || said.contains("does not verify"),
            "{said}"
        );
        assert_eq!(fs::read(&exe).unwrap(), before, "the binary was kept");
    }
    let names: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|name| !name.starts_with(".dibs-update-")),
        "{names:?}"
    );
}
