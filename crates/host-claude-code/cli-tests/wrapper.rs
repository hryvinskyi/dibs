//! The plugin's `hooks/dibs-hook` wrapper, run with `sh` the way the plugin's hook configuration
//! runs it, over a `PATH` without dibs, with fake `dibs` programs, and with the real binary: every
//! event but a wanted worktree exits 0, and a wanted worktree is dibs's answer when dibs gives one
//! and Claude Code's own worktree whenever it does not.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

use crate::support::{Setup, canonical, git, hook_input, last_line, reply, text, write_of};

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
    assert!(hint.contains("dibs self-update"), "{hint}");
    assert!(hint.contains("#install"), "{hint}");
    assert_eq!(check("dibs 999.0.0"), Value::Null);
    let plugin_needs = check("dibs 0.0.1-rc.1");
    assert!(
        line_for_the_person(&plugin_needs).contains("dibs 0.0.1 is installed"),
        "a pre-release is read by its numbers: {plugin_needs}"
    );
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

/// A git repository with one commit, outside every checkout dibs governs.
fn repository() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.name", "Bob"],
        &["config", "user.email", "bob@example.com"],
        &["config", "commit.gpgsign", "false"],
        &["commit", "-q", "--allow-empty", "-m", "start"],
    ] {
        git(dir.path(), args);
    }
    dir
}

/// A fake `dibs` of `version` that writes every command line it is given, one per line, to
/// `calls` in its own directory and answers the hook with `answer`.
fn logging_dibs(version: &str, answer: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls");
    let fake = fake_dibs(&format!(
        "echo \"$*\" >> '{}'\n[ \"$1\" = --version ] && {{ echo 'dibs {version}'; exit 0; }}\ncat >/dev/null\n{answer}",
        calls.display()
    ));
    fs::rename(fake.path().join("dibs"), dir.path().join("dibs")).unwrap();
    dir
}

/// The command lines a logging fake was given.
fn calls(fake: &TempDir) -> Vec<String> {
    fs::read_to_string(fake.path().join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The wrapper answering a `WorktreeCreate` for the worktree `name` wanted from `cwd`.
fn create(path: &OsString, cwd: &Path, name: &str) -> Command {
    run_wrapper(
        path,
        cwd,
        "worktree-create",
        &hook_input("WorktreeCreate", cwd, serde_json::json!({"name": name})),
    )
}

#[test]
fn a_worktree_dibs_gives_is_passed_on_and_its_refusal_fails_the_creation() {
    let repo = repository();
    let lent = tempfile::tempdir().unwrap();
    let giving = logging_dibs(
        "999.0.0",
        &format!(
            "echo 'dibs: worktree2 was reset.' >&2\necho '{}'",
            lent.path().display()
        ),
    );
    let given = output(create(&path_with(&[giving.path()]), repo.path(), "agent-x"));
    assert_eq!(given.status.code(), Some(0), "{}", text(&given.stderr));
    assert_eq!(last_line(&given), canonical(lent.path()));
    assert!(text(&given.stderr).contains("worktree2 was reset"));
    assert!(
        calls(&giving).contains(&"hook claude-code worktree-create".to_owned()),
        "{:?}",
        calls(&giving)
    );

    let refusing = logging_dibs(
        "999.0.0",
        "echo 'dibs: every worktree of the pool is held: worktree1 by bob.' >&2\nexit 1",
    );
    let refused = output(create(
        &path_with(&[refusing.path()]),
        repo.path(),
        "agent-x",
    ));
    assert_eq!(refused.status.code(), Some(1), "the creation fails");
    assert_eq!(text(&refused.stdout), "");
    assert!(
        text(&refused.stderr).contains("every worktree of the pool is held"),
        "{}",
        text(&refused.stderr)
    );
    assert!(
        !repo.path().join(".claude/worktrees/agent-x").exists(),
        "a refusal makes no worktree"
    );
}

#[test]
fn whenever_dibs_gives_no_worktree_the_wrapper_makes_claude_codes_own_and_says_why() {
    let older = logging_dibs("0.0.1", "echo /");
    let no_json_path = logging_dibs(
        "999.0.0",
        "echo '{\"systemMessage\":\"dibs: the event is unknown.\"}'",
    );
    let crashing = logging_dibs("999.0.0", "echo 'thread main panicked' >&2\nexit 101");
    let missing_dir = logging_dibs("999.0.0", "echo /no/such/worktree");
    let nothing = logging_dibs("999.0.0", "exit 0");
    let none = path_with(&[]);
    let cases: [(&str, OsString, bool, &str); 7] = [
        ("missing", none, false, "dibs is not installed"),
        (
            "off",
            path_with(&[&binary_dir()]),
            true,
            "dibs is switched off",
        ),
        (
            "older",
            path_with(&[older.path()]),
            false,
            "dibs 0.0.1 is older than the",
        ),
        (
            "json",
            path_with(&[no_json_path.path()]),
            false,
            "dibs answered without a worktree",
        ),
        (
            "crash",
            path_with(&[crashing.path()]),
            false,
            "dibs ended with status 101",
        ),
        (
            "gone",
            path_with(&[missing_dir.path()]),
            false,
            "dibs answered without a worktree",
        ),
        (
            "silent",
            path_with(&[nothing.path()]),
            false,
            "dibs answered without a worktree",
        ),
    ];
    for (case, path, off, why) in cases {
        let repo = repository();
        let name = format!("agent-{case}");
        let mut command = create(&path, repo.path(), &name);
        if off {
            command.env("DIBS_OFF", "1");
        }
        let made = output(command);
        assert_eq!(
            made.status.code(),
            Some(0),
            "{case}: {}",
            text(&made.stderr)
        );
        let own = canonical(&repo.path().join(".claude/worktrees").join(&name));
        assert_eq!(last_line(&made), own, "{case}");
        assert_eq!(
            git(&own, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            format!("worktree-{name}"),
            "{case}"
        );
        let said = text(&made.stderr);
        assert!(
            said.contains("pool of worktrees was not used"),
            "{case}: {said}"
        );
        assert!(said.contains(why), "{case}: {said}");
    }
    assert_eq!(
        calls(&older),
        ["--version"],
        "an older dibs is never asked for a worktree"
    );
}

#[test]
fn a_worktree_wanted_again_under_the_same_name_is_the_one_already_made() {
    let repo = repository();
    let none = path_with(&[]);
    let first = output(create(&none, repo.path(), "fix-totals"));
    let second = output(create(&none, repo.path(), "fix-totals"));
    assert_eq!(
        (first.status.code(), second.status.code()),
        (Some(0), Some(0))
    );
    assert_eq!(last_line(&first), last_line(&second));
    git(
        repo.path(),
        &["worktree", "remove", &last_line(&first).to_string_lossy()],
    );
    let third = output(create(&none, repo.path(), "fix-totals"));
    assert_eq!(
        third.status.code(),
        Some(0),
        "the branch left behind is checked out again: {}",
        text(&third.stderr)
    );
    assert_eq!(last_line(&third), last_line(&first));
}

#[test]
fn a_worktree_that_cannot_be_made_at_all_fails_the_creation_with_why() {
    let elsewhere = tempfile::tempdir().unwrap();
    let none = path_with(&[]);
    let made = output(create(&none, elsewhere.path(), "agent-x"));
    assert_eq!(made.status.code(), Some(1));
    assert_eq!(text(&made.stdout), "");
    assert!(
        text(&made.stderr).contains("lies in no git repository"),
        "{}",
        text(&made.stderr)
    );
    let repo = repository();
    let climbing = output(create(&none, repo.path(), "../outside"));
    assert_eq!(climbing.status.code(), Some(1));
    assert!(!repo.path().join(".claude/outside").exists());
}

#[test]
fn without_dibs_a_worktree_given_back_is_removed_only_when_it_is_claude_codes_own_and_clean() {
    let repo = repository();
    let none = path_with(&[]);
    let remove = |path: &Path| {
        let given_back = hook_input(
            "WorktreeRemove",
            path,
            serde_json::json!({"worktree_path": path.to_string_lossy()}),
        );
        output(run_wrapper(&none, path, "worktree-remove", &given_back))
    };
    let clean = last_line(&output(create(&none, repo.path(), "clean")));
    let removed = remove(&clean);
    assert_eq!(reply(&removed), Value::Null);
    assert!(!clean.exists(), "a clean worktree of its own is removed");

    let dirty = last_line(&output(create(&none, repo.path(), "dirty")));
    fs::write(dirty.join("notes.txt"), "keep me\n").unwrap();
    let kept = remove(&dirty);
    assert_eq!(reply(&kept), Value::Null);
    assert!(dirty.join("notes.txt").exists(), "never removed by force");
    assert!(
        text(&kept.stderr).contains("was kept"),
        "{}",
        text(&kept.stderr)
    );

    let pool = tempfile::tempdir().unwrap();
    let lent = pool.path().join("worktree1");
    git(
        repo.path(),
        &["worktree", "add", "-q", "--detach", &lent.to_string_lossy()],
    );
    assert_eq!(reply(&remove(&lent)), Value::Null);
    assert!(lent.exists(), "a worktree of anything else is left alone");
}

#[test]
fn the_end_of_a_subagent_or_a_session_reaches_only_a_dibs_that_knows_it() {
    let cwd = tempfile::tempdir().unwrap();
    let ends = [
        (
            "subagent-stop",
            "SubagentStop",
            serde_json::json!({"agent_id": "x"}),
        ),
        (
            "session-end",
            "SessionEnd",
            serde_json::json!({"reason": "other"}),
        ),
    ];
    let older = logging_dibs("0.0.1", "exit 0");
    let recent = logging_dibs("999.0.0", "exit 0");
    for (event, name, fields) in &ends {
        let input = hook_input(name, cwd.path(), fields.clone());
        for path in [path_with(&[]), path_with(&[older.path()])] {
            let skipped = output(run_wrapper(&path, cwd.path(), event, &input));
            assert_eq!(reply(&skipped), Value::Null, "{event}");
            assert_eq!(text(&skipped.stderr), "", "{event}: not a word");
        }
        let mut off = run_wrapper(&path_with(&[recent.path()]), cwd.path(), event, &input);
        off.env("DIBS_OFF", "1");
        assert_eq!(reply(&output(off)), Value::Null, "{event}");
        let run = output(run_wrapper(
            &path_with(&[recent.path()]),
            cwd.path(),
            event,
            &input,
        ));
        assert_eq!(reply(&run), Value::Null, "{event}");
    }
    assert!(
        calls(&older).iter().all(|call| call == "--version"),
        "{:?}",
        calls(&older)
    );
    assert_eq!(
        calls(&recent)
            .iter()
            .filter(|call| call.starts_with("hook "))
            .cloned()
            .collect::<Vec<_>>(),
        [
            "hook claude-code subagent-stop",
            "hook claude-code session-end"
        ],
        "switched off, neither was asked"
    );
}

#[test]
fn through_the_wrapper_the_real_binary_lends_a_pool_worktree_and_makes_claude_codes_own_elsewhere()
{
    let setup = Setup::new();
    let widgets = setup.pool(1);
    setup.ok("bob", &widgets, &["worktree", "prepare", "worktree1"]);
    let path = path_with(&[&binary_dir()]);
    let as_bob = |cwd: &Path, name: &str| {
        let mut command = create(&path, cwd, name);
        setup.as_person("bob", cwd, &mut command);
        output(command)
    };
    let lent = as_bob(&widgets, "agent-x");
    assert_eq!(lent.status.code(), Some(0), "{}", text(&lent.stderr));
    assert_eq!(last_line(&lent), setup.pool_worktree("worktree1"));

    let api = setup.api();
    let own = as_bob(&api, "agent-y");
    assert_eq!(own.status.code(), Some(0), "{}", text(&own.stderr));
    assert_eq!(
        last_line(&own),
        canonical(&api.join(".claude/worktrees/agent-y"))
    );
    assert!(
        text(&own.stderr).contains("no [worktrees] table"),
        "dibs made it and said why: {}",
        text(&own.stderr)
    );
}
