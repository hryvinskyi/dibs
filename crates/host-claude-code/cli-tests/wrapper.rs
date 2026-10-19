//! The plugin's `hooks/dibs-hook` wrapper, run with `sh` the way the plugin's hook configuration
//! runs it, over a `PATH` without dibs, with fake `dibs` programs, and with the real binary.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{Setup, reply, text, write_of};

/// The wrapper, in the plugin.
fn wrapper() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/claude-code/hooks/dibs-hook")
}

/// The `PATH` with `first` in front and every directory that holds a `dibs` of its own left out,
/// so that the only `dibs` the wrapper can find is the one a test puts there.
fn path_with(first: &[&Path]) -> OsString {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let has_dibs = |dir: &Path| dir.join("dibs").exists() || dir.join("dibs.exe").exists();
    let kept = std::env::split_paths(&original).filter(|dir| !has_dibs(dir));
    let dirs: Vec<PathBuf> = first
        .iter()
        .map(|dir| dir.to_path_buf())
        .chain(kept)
        .collect();
    std::env::join_paths(dirs).unwrap()
}

/// A directory holding a fake `dibs`: a shell script with this body.
fn fake_dibs(body: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("dibs");
    fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// The wrapper for `event`, with `path` as its `PATH`, in `cwd`.
fn run_wrapper(path: &OsString, cwd: &Path, event: &str, stdin: &str) -> Command {
    let mut command = Command::new("sh");
    command
        .arg(wrapper())
        .arg(event)
        .current_dir(cwd)
        .env("PATH", path)
        .env_remove("DIBS_OFF")
        .write_stdin(stdin.to_owned())
        .timeout(std::time::Duration::from_secs(60));
    command
}

fn output(mut command: Command) -> Output {
    command.output().unwrap()
}

fn line_for_the_person(value: &Value) -> &str {
    value["systemMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("no line for the person: {value}"))
}

#[test]
fn without_dibs_on_the_path_the_wrapper_gives_the_install_hint_and_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let path = path_with(&[]);
    let hook = output(run_wrapper(&path, dir.path(), "pre-tool-use", "{}"));
    assert_eq!(reply(&hook), Value::Null, "no reply for the edit itself");
    assert!(
        text(&hook.stderr).contains("not installed"),
        "{}",
        text(&hook.stderr)
    );
    let check = reply(&output(run_wrapper(
        &path,
        dir.path(),
        "check-binary",
        "{}",
    )));
    let hint = line_for_the_person(&check);
    assert!(hint.contains("not installed"), "{hint}");
    assert!(hint.contains("#install"), "{hint}");
    let mut off = run_wrapper(&path, dir.path(), "check-binary", "{}");
    off.env("DIBS_OFF", "1");
    assert_eq!(
        reply(&output(off)),
        Value::Null,
        "switched off, nothing is said"
    );
}

#[test]
fn a_dibs_that_fails_or_lacks_the_subcommand_is_one_warning_and_exit_0() {
    let failing = [
        (
            r#"echo '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"half a reply"}}'; exit 2"#,
            "status 2",
        ),
        (
            "echo \"error: unrecognized subcommand 'hook'\" >&2; exit 2",
            "status 2",
        ),
        ("exit 1", "status 1"),
    ];
    let cwd = tempfile::tempdir().unwrap();
    for (body, expected) in failing {
        let fake = fake_dibs(body);
        let path = path_with(&[fake.path()]);
        let answer = reply(&output(run_wrapper(
            &path,
            cwd.path(),
            "pre-tool-use",
            "{}",
        )));
        assert_eq!(answer.get("hookSpecificOutput"), None, "{body}: {answer}");
        let warning = line_for_the_person(&answer);
        assert!(warning.contains(expected), "{body}: {warning}");
        assert!(warning.contains("not coordinated"), "{body}: {warning}");
    }
}

#[test]
fn an_installed_dibs_older_than_the_plugin_needs_gets_the_update_hint() {
    let cwd = tempfile::tempdir().unwrap();
    let check = |version_line: &str| {
        let fake = fake_dibs(&format!(
            "[ \"$1\" = --version ] && echo '{version_line}'; exit 0"
        ));
        reply(&output(run_wrapper(
            &path_with(&[fake.path()]),
            cwd.path(),
            "check-binary",
            "",
        )))
    };
    let old = check("dibs 0.0.1");
    let hint = line_for_the_person(&old);
    assert!(hint.contains("dibs 0.0.1 is installed"), "{hint}");
    assert!(hint.contains("#install"), "{hint}");
    assert_eq!(check("dibs 999.0.0"), Value::Null);
    let unknown = check("dibs");
    assert!(line_for_the_person(&unknown).contains("did not name a version"));
}

/// The directory of the binary this crate built.
fn binary_dir() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_dibs"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn the_binary_built_with_the_plugin_meets_its_min_binary() {
    let cwd = tempfile::tempdir().unwrap();
    let path = path_with(&[&binary_dir()]);
    assert_eq!(
        reply(&output(run_wrapper(&path, cwd.path(), "check-binary", ""))),
        Value::Null
    );
}

#[test]
fn through_the_wrapper_the_real_binary_answers_and_a_broken_configuration_exits_0() {
    let setup = Setup::new();
    let api = setup.api();
    setup.ok(
        "alice",
        &api,
        &["new", "gh:acme/api#8", "--title", "Billing"],
    );
    setup.ok(
        "alice",
        &api,
        &["claim", "gh:acme/api#8", "--area", "billing"],
    );
    let path = path_with(&[&binary_dir()]);
    let as_bob = |stdin: &str| {
        let mut command = run_wrapper(&path, &api, "pre-tool-use", stdin);
        setup.as_person("bob", &api, &mut command);
        reply(&output(command))
    };
    let held = as_bob(&write_of(&api, "src/billing/a.rs"));
    assert_eq!(
        held["hookSpecificOutput"]["permissionDecision"], "deny",
        "{held}"
    );
    fs::write(api.join(".dibs.toml"), "server = [\n").unwrap();
    let broken = as_bob(&write_of(&api, "src/billing/a.rs"));
    assert_eq!(broken.get("hookSpecificOutput"), None, "{broken}");
    assert!(
        line_for_the_person(&broken).contains(".dibs.toml"),
        "{broken}"
    );
}
