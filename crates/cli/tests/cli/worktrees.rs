//! Worktree pools through the command line, in a real git checkout whose `.dibs.toml` declares a
//! pool and whose prepare and reset commands are scripts that record what they were given: the
//! first take prepares and the next one resets; the same holder asking again gets its tree back
//! untouched; two takes are two holders; a worktree is a governed checkout that takes from its
//! checkout's pool; the work a reused worktree held is rescued onto a branch; a worktree mid-rebase
//! or whose reset fails is set aside and the next one taken; a worktree somebody still works in is
//! skipped; a take waits for a release; a take under a deadline starts a preparation in the
//! background; and a checkout without a pool says so.
//!
//! The signs of life left in a worktree keep its holder's hold, and one the server let go of while
//! they could not reach it is taken again for that holder; the upkeep stops the environment of a
//! worktree the server freed once it has been free long enough, and not again until it is taken,
//! leaves a worktree another dibs is resetting alone, and names what lies beyond the pool's count;
//! a server without the worktree routes has the signs of life dropped without a word.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::support::{API_REMOTE, Setup, git, json, make_checkout, status, text, write};

/// A checkout `widgets` lending a pool of `count` worktrees, the scripts its commands run, and the
/// log those scripts write.
struct Pool {
    setup: Setup,
    checkout: PathBuf,
    kit: PathBuf,
    log: PathBuf,
}

const PREPARE: &str = r#"#!/bin/sh
echo "prepare $DIBS_WORKTREE_NAME|$DIBS_WORKTREE_PATH|$DIBS_WORKTREE_BASE|$DIBS_CHECKOUT|$(pwd -P)" >> "$KIT/log"
if [ -e "$KIT/fail-prepare-$DIBS_WORKTREE_NAME" ]; then exit 5; fi
if [ ! -e "$DIBS_WORKTREE_PATH/.git" ]; then
  git -C "$DIBS_CHECKOUT" worktree add -q --detach "$DIBS_WORKTREE_PATH" "$DIBS_WORKTREE_BASE" || exit 6
fi
echo "prepared $DIBS_WORKTREE_NAME"
"#;

const RESET: &str = r#"#!/bin/sh
echo "reset $DIBS_WORKTREE_NAME|$DIBS_WORKTREE_PATH|$DIBS_WORKTREE_BASE|$DIBS_CHECKOUT|$(pwd -P)" >> "$KIT/log"
if [ -e "$KIT/fail-reset-$DIBS_WORKTREE_NAME" ]; then exit 7; fi
cd "$DIBS_WORKTREE_PATH" && git checkout -q --detach "$DIBS_WORKTREE_BASE" && git clean -ffdq
"#;

const IDLE: &str = r#"#!/bin/sh
echo "idle $DIBS_WORKTREE_NAME|$DIBS_WORKTREE_PATH|$DIBS_WORKTREE_BASE|$DIBS_CHECKOUT|$(pwd -P)" >> "$KIT/log"
"#;

/// How long a worktree of the test pools stays free before its environment is stopped.
const IDLE_AFTER: Duration = Duration::from_secs(1);

impl Pool {
    fn new(count: u8) -> Self {
        let setup = Setup::new();
        let server = setup.server.url();
        Self::on(setup, &server, count)
    }

    /// A pool whose checkout names the server at `server`, alice signed in to it.
    fn on(setup: Setup, server: &str, count: u8) -> Self {
        let checkout = setup.root.path().join("widgets");
        make_checkout(&checkout, server, "acme", Some("api"), API_REMOTE);
        if server != setup.server.url() {
            let out = setup
                .dibs(&setup.alice, &setup.elsewhere())
                .args(["login", "--server", server])
                .write_stdin(format!("{}\n", setup.alice.token))
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", text(&out.stderr));
        }
        git(&checkout, &["rm", "-q", "--cached", ".dibs.toml"]);
        let exclude = checkout.join(".git/info/exclude");
        let mut excluded = fs::read_to_string(&exclude).unwrap_or_default();
        excluded.push_str("\n.dibs.toml\n");
        fs::write(&exclude, excluded).unwrap();
        git(
            &checkout,
            &["commit", "-q", "-m", "keep the configuration local"],
        );
        let kit = setup.root.path().join("kit");
        for (name, script) in [
            ("prepare.sh", PREPARE),
            ("reset.sh", RESET),
            ("idle.sh", IDLE),
        ] {
            let body = script.replacen(
                "#!/bin/sh\n",
                &format!("#!/bin/sh\nKIT='{}'\n", kit.display()),
                1,
            );
            write(&kit, name, &body);
        }
        let mut config = fs::read_to_string(checkout.join(".dibs.toml")).unwrap();
        config.push_str(&format!(
            "\n[worktrees]\ncount = {count}\nbase = \"main\"\nprepare = \"sh '{}'\"\nreset = \"sh '{}'\"\nidle = \"sh '{}'\"\nidle_after = \"{}s\"\n",
            kit.join("prepare.sh").display(),
            kit.join("reset.sh").display(),
            kit.join("idle.sh").display(),
            IDLE_AFTER.as_secs()
        ));
        fs::write(checkout.join(".dibs.toml"), config).unwrap();
        let log = kit.join("log");
        Self {
            setup,
            checkout,
            kit,
            log,
        }
    }

    /// The directory of a worktree, as the file system resolves it.
    fn worktree(&self, name: &str) -> PathBuf {
        canonical(&self.setup.root.path().join("widgets-worktrees").join(name))
    }

    /// `dibs` as alice, in `cwd`.
    fn dibs_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.setup
            .dibs(&self.setup.alice, cwd)
            .args(args)
            .output()
            .unwrap()
    }

    /// `dibs` as alice, in the checkout.
    fn dibs(&self, args: &[&str]) -> Output {
        self.dibs_in(&self.checkout, args)
    }

    /// A take by `agent` that must succeed; answers the path it printed as its last line.
    fn take(&self, agent: &str) -> PathBuf {
        let out = self.dibs(&["worktree", "take", "--agent", agent]);
        assert_eq!(status(&out), 0);
        last_line(&out)
    }

    fn release(&self, name: &str) {
        let out = self.dibs(&["worktree", "release", name]);
        assert_eq!(status(&out), 0);
    }

    /// The steps the scripts ran, as `<step> <worktree>`.
    fn steps(&self) -> Vec<String> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| line.split('|').next().unwrap_or_default().to_owned())
            .collect()
    }

    /// The rescue branches of the checkout.
    fn rescues(&self) -> Vec<String> {
        git(
            &self.checkout,
            &[
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/heads/dibs/rescue/",
            ],
        )
        .lines()
        .map(str::to_owned)
        .collect()
    }

    /// The one rescue branch that is not among `before`.
    fn new_rescue(&self, before: &[String]) -> String {
        let made: Vec<String> = self
            .rescues()
            .into_iter()
            .filter(|branch| !before.contains(branch))
            .collect();
        assert_eq!(made.len(), 1, "one rescue branch made: {made:?}");
        made[0].clone()
    }

    /// This machine's file of `what` for the worktree `name` (`worktree1.beat`).
    fn state_file(&self, name: &str, what: &str) -> PathBuf {
        let base = self.setup.home(&self.setup.alice).join("state/worktrees");
        let file = format!("{name}.{what}");
        for scope in fs::read_dir(&base).unwrap() {
            for pool in fs::read_dir(scope.unwrap().path()).unwrap() {
                let dir = pool.unwrap().path();
                if dir.join(format!("{name}.free-since")).exists()
                    || dir.join(format!("{name}.hold.json")).exists()
                {
                    return dir.join(file);
                }
            }
        }
        panic!("no state of {name} under {}", base.display());
    }

    fn listed(&self) -> serde_json::Value {
        let out = self.dibs(&["worktree", "list", "--json"]);
        assert_eq!(status(&out), 0);
        json(&out)
    }

    /// `dibs worktree upkeep --json`, which must succeed; answers what it printed.
    fn upkeep(&self) -> serde_json::Value {
        let out = self.dibs(&["worktree", "upkeep", "--json"]);
        assert_eq!(status(&out), 0);
        json(&out)
    }

    /// Leaves a sign of life in a worktree through the command line.
    fn beat(&self, name: &str) {
        let out = self.dibs(&["worktree", "beat", name]);
        assert_eq!(status(&out), 0);
    }

    /// How often the idle command ran for `name`.
    fn idles(&self, name: &str) -> usize {
        let step = format!("idle {name}");
        self.steps().iter().filter(|line| **line == step).count()
    }

    /// Waits until the worktrees freed now have been free long enough to be stopped.
    fn wait_out_idle_after(&self) {
        std::thread::sleep(IDLE_AFTER + Duration::from_millis(200));
    }
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn last_line(out: &Output) -> PathBuf {
    let stdout = text(&out.stdout);
    let line = stdout.lines().last().unwrap_or_default().to_owned();
    canonical(Path::new(&line))
}

#[test]
fn a_take_prepares_a_new_worktree_and_resets_it_when_it_is_taken_again() {
    let pool = Pool::new(3);
    let out = pool.dibs(&["worktree", "take", "--purpose", "fix the totals"]);
    assert_eq!(status(&out), 0);
    assert_eq!(last_line(&out), pool.worktree("worktree1"));
    assert_eq!(pool.steps(), ["prepare worktree1"]);
    let line = fs::read_to_string(&pool.log).unwrap();
    let fields: Vec<&str> = line.trim().split('|').collect();
    assert_eq!(canonical(Path::new(fields[1])), pool.worktree("worktree1"));
    assert_eq!(fields[2], "main");
    assert_eq!(canonical(Path::new(fields[3])), canonical(&pool.checkout));
    assert_eq!(
        canonical(Path::new(fields[4])),
        canonical(&pool.checkout),
        "a command runs in the checkout"
    );
    let said = text(&out.stderr);
    assert!(said.contains("prepared worktree1"), "{said}");
    assert!(
        !text(&out.stdout).contains("prepared worktree1"),
        "a command's output stays off the path's stream"
    );
    assert!(
        said.contains("--agent cli-"),
        "the minted key is told: {said}"
    );

    let own = fs::read_to_string(pool.worktree("worktree1").join(".dibs.toml")).unwrap();
    assert!(own.contains("[pool_worktree]"), "{own}");
    assert!(own.contains("name = \"worktree1\""), "{own}");
    assert!(own.contains("project = \"acme\""), "{own}");
    assert!(!own.contains("[worktrees]"), "{own}");

    pool.release("worktree1");
    let again = pool.dibs(&["worktree", "take", "--json"]);
    assert_eq!(status(&again), 0);
    let answer = json(&again);
    assert_eq!(answer["worktree"], "worktree1");
    assert_eq!(answer["prepared"], false);
    assert_eq!(answer["again"], false);
    assert_eq!(pool.steps(), ["prepare worktree1", "reset worktree1"]);
}

#[test]
fn the_same_holder_asking_again_gets_its_worktree_untouched() {
    let pool = Pool::new(2);
    let path = pool.take("agent-7");
    write(&path, "half-done.txt", "work in progress\n");
    let out = pool.dibs(&["worktree", "take", "--agent", "agent-7"]);
    assert_eq!(status(&out), 0);
    assert_eq!(last_line(&out), path);
    assert!(
        text(&out.stderr).contains("left as it is"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        pool.steps(),
        ["prepare worktree1"],
        "neither reset nor prepared"
    );
    assert!(pool.rescues().is_empty(), "nothing rescued");
    assert!(path.join("half-done.txt").exists());
}

#[test]
fn two_takes_without_an_agent_are_two_holders_with_two_worktrees() {
    let pool = Pool::new(3);
    let first = pool.dibs(&["worktree", "take", "--json"]);
    let second = pool.dibs(&["worktree", "take", "--json"]);
    assert_eq!((status(&first), status(&second)), (0, 0));
    let (first, second) = (json(&first), json(&second));
    assert_eq!(first["worktree"], "worktree1");
    assert_eq!(second["worktree"], "worktree2");
    assert_ne!(first["agent"], second["agent"]);
    assert_eq!(first["session_id"], second["session_id"]);
}

#[test]
fn a_worktree_is_a_governed_checkout_that_takes_from_its_checkouts_pool() {
    let pool = Pool::new(3);
    let first = pool.take("agent-1");
    let out = pool.dibs_in(&first, &["worktree", "take", "--agent", "agent-2"]);
    assert_eq!(status(&out), 0);
    let second = last_line(&out);
    assert_eq!(second, pool.worktree("worktree2"));
    let status_inside = pool.dibs_in(&second, &["status"]);
    assert_eq!(
        status(&status_inside),
        0,
        "the worktree's .dibs.toml signs in"
    );
    let released = pool.dibs_in(&second, &["worktree", "release"]);
    assert_eq!(status(&released), 0);
    assert!(text(&released.stdout).contains("Released worktree2."));
    let listed = pool.listed();
    assert_eq!(listed["worktrees"][1]["held"], serde_json::Value::Null);
    assert_eq!(listed["worktrees"][0]["held"]["agent"], "agent-1");
}

#[test]
fn the_work_a_reused_worktree_held_is_rescued_and_a_clean_one_is_not() {
    let pool = Pool::new(1);
    let path = pool.take("agent-1");
    pool.release("worktree1");

    write(&path, "notes.txt", "keep me\n");
    let out = pool.dibs(&["worktree", "take", "--agent", "agent-2"]);
    assert_eq!(status(&out), 0);
    let rescues = pool.rescues();
    assert_eq!(rescues.len(), 1, "{rescues:?}");
    let dirty = rescues[0].clone();
    assert!(dirty.starts_with("dibs/rescue/worktree1-"), "{dirty}");
    assert!(text(&out.stderr).contains(&dirty), "{}", text(&out.stderr));
    assert_eq!(
        git(&pool.checkout, &["show", &format!("{dirty}:notes.txt")]),
        "keep me\n"
    );
    assert_eq!(
        git(&pool.checkout, &["log", "-1", "--format=%an|%s", &dirty]).trim(),
        "alice|dibs: rescued from worktree1 before reuse"
    );
    assert!(
        !path.join("notes.txt").exists(),
        "the reset cleaned the tree"
    );

    pool.release("worktree1");
    pool.take("agent-3");
    assert_eq!(pool.rescues().len(), 1, "a clean worktree is not rescued");

    write(&path, "detached.txt", "a commit on no branch\n");
    git(&path, &["add", "detached.txt"]);
    git(&path, &["commit", "-q", "-m", "detached work"]);
    let detached_commit = git(&path, &["rev-parse", "HEAD"]);
    pool.release("worktree1");
    let before = pool.rescues();
    pool.take("agent-4");
    let detached = pool.new_rescue(&before);
    assert_eq!(
        git(&pool.checkout, &["rev-parse", &detached]),
        detached_commit,
        "the detached commit is kept on a rescue branch"
    );

    git(&path, &["switch", "-q", "-c", "feature"]);
    write(&path, "feature.txt", "committed\n");
    git(&path, &["add", "feature.txt"]);
    git(&path, &["commit", "-q", "-m", "feature work"]);
    let feature = git(&path, &["rev-parse", "HEAD"]);
    write(&path, "feature.txt", "changed after the commit\n");
    pool.release("worktree1");
    let before = pool.rescues();
    pool.take("agent-5");
    assert_eq!(
        git(&pool.checkout, &["rev-parse", "feature"]),
        feature,
        "the branch the worktree was on is not moved"
    );
    let on_branch = pool.new_rescue(&before);
    assert_eq!(
        git(
            &pool.checkout,
            &["show", &format!("{on_branch}:feature.txt")]
        ),
        "changed after the commit\n",
        "the uncommitted change is kept"
    );
    let listed = pool.listed();
    assert_eq!(
        listed["worktrees"][0]["rescue_branches"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn a_worktree_mid_rebase_is_set_aside_and_the_next_one_is_taken() {
    let pool = Pool::new(2);
    let first = pool.take("agent-1");
    pool.release("worktree1");
    let git_dir = git(&first, &["rev-parse", "--absolute-git-dir"]);
    fs::create_dir_all(Path::new(git_dir.trim()).join("rebase-merge")).unwrap();

    let out = pool.dibs(&["worktree", "take", "--agent", "agent-2"]);
    assert_eq!(status(&out), 0);
    assert_eq!(last_line(&out), pool.worktree("worktree2"));
    let said = text(&out.stderr);
    assert!(said.contains("a rebase is in progress"), "{said}");
    assert!(said.contains("dibs worktree prepare worktree1"), "{said}");
    assert!(pool.rescues().is_empty());
    let listed = pool.listed();
    assert!(
        listed["worktrees"][0]["broken"]["reason"]
            .as_str()
            .unwrap()
            .contains("rebase"),
        "{listed}"
    );
    assert_eq!(
        listed["worktrees"][0]["held"],
        serde_json::Value::Null,
        "its hold was released"
    );

    let busy = pool.dibs(&["worktree", "take", "--agent", "agent-3"]);
    assert_eq!(status(&busy), 1, "the broken one is not handed out");
    assert!(text(&busy.stderr).contains("Set aside as broken: worktree1"));

    let prepared = pool.dibs(&["worktree", "prepare", "worktree1"]);
    assert_eq!(status(&prepared), 0);
    assert_eq!(
        pool.listed()["worktrees"][0]["broken"],
        serde_json::Value::Null
    );
}

#[test]
fn a_failing_reset_sets_the_worktree_aside_and_the_next_one_is_taken() {
    let pool = Pool::new(2);
    pool.take("agent-1");
    pool.release("worktree1");
    write(&pool.kit, "fail-reset-worktree1", "");
    let out = pool.dibs(&["worktree", "take", "--agent", "agent-2"]);
    assert_eq!(status(&out), 0);
    assert_eq!(last_line(&out), pool.worktree("worktree2"));
    assert_eq!(
        pool.steps(),
        ["prepare worktree1", "reset worktree1", "prepare worktree2"]
    );
    let listed = pool.listed();
    let reason = listed["worktrees"][0]["broken"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("reset command failed with status 7"),
        "{reason}"
    );

    fs::remove_file(pool.kit.join("fail-reset-worktree1")).unwrap();
    write(&pool.kit, "fail-prepare-worktree1", "");
    let failed = pool.dibs(&["worktree", "prepare", "worktree1"]);
    assert_eq!(status(&failed), 1);
    assert!(
        text(&failed.stderr).contains("prepare command failed with status 5"),
        "{}",
        text(&failed.stderr)
    );
}

#[test]
fn every_worktree_failing_ends_the_take_with_why() {
    let pool = Pool::new(2);
    write(&pool.kit, "fail-prepare-worktree1", "");
    write(&pool.kit, "fail-prepare-worktree2", "");
    let out = pool.dibs(&["worktree", "take", "--agent", "agent-1"]);
    assert_eq!(status(&out), 1);
    let said = text(&out.stderr);
    assert!(
        said.contains("No worktree of this pool could be made ready"),
        "{said}"
    );
    assert!(
        said.contains("worktree2: its prepare command failed"),
        "{said}"
    );
    let listed = pool.listed();
    for worktree in listed["worktrees"].as_array().unwrap() {
        assert_eq!(worktree["held"], serde_json::Value::Null, "{worktree}");
    }
}

#[test]
fn a_worktree_with_a_sign_of_life_after_its_release_is_skipped() {
    let pool = Pool::new(2);
    pool.take("agent-1");
    pool.release("worktree1");
    let beat = pool.state_file("worktree1", "beat");
    fs::write(&beat, b"").unwrap();
    let out = pool.dibs(&["worktree", "take", "--agent", "agent-2"]);
    assert_eq!(status(&out), 0);
    assert_eq!(last_line(&out), pool.worktree("worktree2"));
    assert_eq!(pool.steps(), ["prepare worktree1", "prepare worktree2"]);
}

#[test]
fn a_waiting_take_gets_the_worktree_once_its_holder_releases_it() {
    let pool = Pool::new(1);
    let path = pool.take("agent-1");
    let busy = pool.dibs(&["worktree", "take", "--agent", "agent-2"]);
    assert_eq!(status(&busy), 1);
    let said = text(&busy.stderr);
    assert!(said.contains("worktree1 by the agent agent-1"), "{said}");

    let waiting = pool
        .setup
        .process_as("alice", &pool.checkout)
        .args([
            "worktree",
            "take",
            "--agent",
            "agent-2",
            "--wait",
            "20s",
            "--poll-every",
            "1s",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let released = pool.dibs(&["worktree", "release", "--agent", "agent-1"]);
    assert_eq!(status(&released), 0);
    let out = waiting.wait_with_output().unwrap();
    assert_eq!(status(&out), 0);
    assert_eq!(last_line(&out), path);
}

#[test]
fn a_take_under_a_deadline_never_prepares_and_starts_a_preparation_instead() {
    let pool = Pool::new(2);
    let out = pool.dibs(&[
        "worktree",
        "take",
        "--agent",
        "agent-1",
        "--deadline",
        "30s",
    ]);
    assert_eq!(status(&out), 1);
    let said = text(&out.stderr);
    assert!(said.contains("worktree1 is being prepared"), "{said}");
    let started = Instant::now();
    while !pool.worktree("worktree1").join(".dibs.toml").exists() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the background preparation never finished"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(pool.steps(), ["prepare worktree1"]);
    let took = pool.dibs(&[
        "worktree",
        "take",
        "--agent",
        "agent-1",
        "--deadline",
        "30s",
    ]);
    assert_eq!(status(&took), 0);
    assert_eq!(last_line(&took), pool.worktree("worktree1"));
    assert_eq!(pool.steps(), ["prepare worktree1", "reset worktree1"]);
}

#[test]
fn a_checkout_without_a_pool_says_how_to_declare_one() {
    let setup = Setup::new();
    let api = setup.checkout("api", Some("api"), API_REMOTE);
    for args in [
        &["worktree", "take"][..],
        &["worktree", "list"],
        &["worktree", "release", "worktree1"],
        &["worktree", "prepare", "--all"],
    ] {
        let out = setup.dibs(&setup.alice, &api).args(args).output().unwrap();
        assert_eq!(status(&out), 2, "{args:?}");
        let said = text(&out.stderr);
        assert!(said.contains("[worktrees]"), "{args:?}: {said}");
    }
    let outside = setup
        .dibs(&setup.alice, &setup.elsewhere())
        .args(["worktree", "take"])
        .output()
        .unwrap();
    assert_eq!(status(&outside), 2);
}

impl Pool {
    /// The lock file of the worktree `name`, which a take, a preparation or an upkeep holds while
    /// it works on it.
    fn lock_file(&self, name: &str) -> PathBuf {
        let cache = self.setup.home(&self.setup.alice).join("state/cache");
        let suffix = format!("-{name}.lock");
        for scope in fs::read_dir(&cache).unwrap() {
            for file in fs::read_dir(scope.unwrap().path()).unwrap() {
                let path = file.unwrap().path();
                let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
                if file_name.starts_with("worktree-") && file_name.ends_with(&suffix) {
                    return path;
                }
            }
        }
        panic!("no lock of {name} under {}", cache.display());
    }
}

#[test]
fn signs_of_life_keep_the_hold_of_the_holder_their_record_names() {
    let pool = Pool::new(2);
    pool.take("agent-1");
    pool.setup.server.advance(time::Duration::minutes(15));
    pool.beat("worktree1");
    let upkept = pool.upkeep();
    assert_eq!(upkept["beats"]["beats"], 1, "{upkept}");
    assert_eq!(upkept["beats"]["retaken"], serde_json::json!([]));
    assert_eq!(upkept["let_go"], serde_json::json!([]));

    pool.setup.server.advance(time::Duration::minutes(10));
    let listed = pool.listed();
    let held = &listed["worktrees"][0]["held"];
    assert_eq!(
        held["agent"], "agent-1",
        "twenty-five minutes after the take, ten after its sign of life: {listed}"
    );
    assert!(
        held["last_seen"]
            .as_str()
            .unwrap()
            .starts_with("2026-03-02T09:15"),
        "{held}"
    );
    let again = pool.upkeep();
    assert_eq!(again["beats"]["beats"], 0, "a sign of life is sent once");
}

#[test]
fn a_hold_the_server_let_go_of_while_its_holder_worked_is_taken_again_for_it() {
    let pool = Pool::new(1);
    let out = pool.dibs(&[
        "worktree",
        "take",
        "--agent",
        "agent-1",
        "--purpose",
        "fix the totals",
    ]);
    assert_eq!(status(&out), 0);
    pool.setup.server.advance(time::Duration::minutes(21));
    pool.beat("worktree1");
    let upkept = pool.upkeep();
    assert_eq!(
        upkept["beats"]["retaken"],
        serde_json::json!(["worktree1"]),
        "{upkept}"
    );
    assert_eq!(upkept["let_go"], serde_json::json!([]));
    assert_eq!(upkept["freed"], serde_json::json!([]));
    let listed = pool.listed();
    let held = &listed["worktrees"][0]["held"];
    assert_eq!(held["agent"], "agent-1", "{listed}");
    assert_eq!(held["purpose"], "fix the totals");
    assert_eq!(pool.steps(), ["prepare worktree1"], "nothing was reset");
    let said = pool.dibs(&["worktree", "upkeep"]);
    assert_eq!(status(&said), 0);
    assert!(
        text(&said.stdout).contains("Nothing was due."),
        "{}",
        text(&said.stdout)
    );
}

#[test]
fn upkeep_stops_a_worktree_the_server_freed_once_until_it_is_taken_again() {
    let pool = Pool::new(1);
    pool.take("agent-1");
    pool.wait_out_idle_after();
    let held = pool.upkeep();
    assert_eq!(held["freed"], serde_json::json!([]), "{held}");
    assert_eq!(
        pool.idles("worktree1"),
        0,
        "a held worktree keeps its environment"
    );

    pool.setup.server.advance(time::Duration::minutes(21));
    let freed = pool.upkeep();
    assert_eq!(freed["let_go"], serde_json::json!(["worktree1"]), "{freed}");
    assert_eq!(freed["freed"], serde_json::json!(["worktree1"]));
    assert_eq!(pool.idles("worktree1"), 0, "free for less than idle_after");

    pool.wait_out_idle_after();
    let stopped = pool.upkeep();
    assert_eq!(
        stopped["stopped"],
        serde_json::json!(["worktree1"]),
        "{stopped}"
    );
    assert_eq!(pool.idles("worktree1"), 1);
    let log = fs::read_to_string(&pool.log).unwrap();
    let line = log.lines().find(|line| line.starts_with("idle ")).unwrap();
    let fields: Vec<&str> = line.split('|').collect();
    assert_eq!(canonical(Path::new(fields[1])), pool.worktree("worktree1"));
    assert_eq!(canonical(Path::new(fields[4])), canonical(&pool.checkout));
    assert_eq!(pool.listed()["worktrees"][0]["stopped"], true);

    pool.wait_out_idle_after();
    pool.upkeep();
    assert_eq!(pool.idles("worktree1"), 1, "stopped once");

    pool.take("agent-2");
    assert_eq!(
        pool.listed()["worktrees"][0]["stopped"],
        false,
        "a take clears the stop"
    );
    pool.release("worktree1");
    pool.wait_out_idle_after();
    pool.upkeep();
    assert_eq!(
        pool.idles("worktree1"),
        2,
        "stopped again once it was taken and freed"
    );
}

#[test]
fn upkeep_leaves_a_worktree_alone_while_another_dibs_holds_its_lock() {
    let pool = Pool::new(1);
    pool.take("agent-1");
    pool.release("worktree1");
    pool.wait_out_idle_after();
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(pool.lock_file("worktree1"))
        .unwrap();
    lock.lock().unwrap();
    let started = Instant::now();
    let busy = pool.upkeep();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the upkeep does not wait for the lock"
    );
    assert_eq!(busy["busy"], serde_json::json!(["worktree1"]), "{busy}");
    assert_eq!(pool.idles("worktree1"), 0);
    drop(lock);
    let stopped = pool.upkeep();
    assert_eq!(
        stopped["stopped"],
        serde_json::json!(["worktree1"]),
        "{stopped}"
    );
    assert_eq!(pool.idles("worktree1"), 1);
}

#[test]
fn upkeep_does_nothing_without_a_pool_and_names_what_lies_beyond_the_count() {
    let pool = Pool::new(1);
    let api = pool.setup.checkout("api", Some("api"), API_REMOTE);
    let out = pool
        .setup
        .dibs(&pool.setup.alice, &api)
        .args(["worktree", "upkeep"])
        .output()
        .unwrap();
    assert_eq!(status(&out), 0);
    assert!(text(&out.stdout).contains("[worktrees]"));

    pool.take("agent-1");
    let stray = pool.setup.root.path().join("widgets-worktrees/worktree2");
    git(
        &pool.checkout,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            stray.to_str().unwrap(),
            "main",
        ],
    );
    let upkept = pool.upkeep();
    assert_eq!(
        upkept["beyond"],
        serde_json::json!(["worktree2"]),
        "{upkept}"
    );
    let said = pool.dibs(&["worktree", "upkeep"]);
    assert_eq!(status(&said), 0);
    assert!(
        text(&said.stdout)
            .contains("worktree2 lies in the pool's directory beyond its 1 worktrees"),
        "{}",
        text(&said.stdout)
    );
    assert!(stray.join(".git").exists(), "left as it is");
    assert_eq!(pool.idles("worktree2"), 0);
}

#[test]
fn a_server_without_the_worktree_routes_has_the_signs_of_life_dropped_without_a_word() {
    let setup = Setup::new();
    let old = OldServer::before(&setup.server.url());
    let pool = Pool::on(setup, &old.url, 1);
    pool.take("agent-1");
    old.missing.store(true, Ordering::SeqCst);

    pool.beat("worktree1");
    let out = pool.dibs(&["sync"]);
    assert_eq!(status(&out), 0);
    assert_eq!(text(&out.stderr), "", "no word about the signs of life");
    assert_eq!(old.beats.load(Ordering::SeqCst), 1);

    pool.beat("worktree1");
    let again = pool.dibs(&["sync", "--force"]);
    assert_eq!(status(&again), 0);
    assert_eq!(text(&again.stderr), "");
    assert_eq!(
        old.beats.load(Ordering::SeqCst),
        1,
        "a server without the routes is not asked again within a day"
    );

    let upkeep = pool.dibs(&["worktree", "upkeep"]);
    assert_eq!(status(&upkeep), 0);
    assert!(
        text(&upkeep.stderr).contains("keeps no worktree holds"),
        "{}",
        text(&upkeep.stderr)
    );
    assert_eq!(old.beats.load(Ordering::SeqCst), 1);
}

/// A server in front of the test server that, once `missing` is set, answers the worktree routes
/// as a server older than them does, and counts the beats it is sent. Every connection carries one
/// request.
struct OldServer {
    url: String,
    missing: Arc<AtomicBool>,
    beats: Arc<AtomicUsize>,
}

impl OldServer {
    fn before(target: &str) -> Self {
        let upstream: SocketAddr = target.strip_prefix("http://").unwrap().parse().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let missing = Arc::new(AtomicBool::new(false));
        let beats = Arc::new(AtomicUsize::new(0));
        let (routes_missing, beaten) = (missing.clone(), beats.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(client) = stream else { continue };
                let (routes_missing, beaten) = (routes_missing.clone(), beaten.clone());
                std::thread::spawn(move || answer_one(client, upstream, &routes_missing, &beaten));
            }
        });
        Self {
            url,
            missing,
            beats,
        }
    }
}

/// Reads one request from `client`; answers a worktree route itself while `missing` is set, and
/// passes any other request to `upstream`, asking it to close the connection after its answer.
fn answer_one(
    mut client: TcpStream,
    upstream: SocketAddr,
    missing: &AtomicBool,
    beats: &AtomicUsize,
) {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 4096];
    let head_end = loop {
        if let Some(end) = request.windows(4).position(|four| four == b"\r\n\r\n") {
            break end + 4;
        }
        match client.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
    };
    let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while request.len() < head_end + length {
        match client.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
    }
    let path = head.split_whitespace().nth(1).unwrap_or_default();
    if path.starts_with("/v1/worktrees/beat") {
        beats.fetch_add(1, Ordering::SeqCst);
    }
    if missing.load(Ordering::SeqCst)
        && (path.starts_with("/v1/worktrees") || path.ends_with("/end"))
    {
        let body = r#"{"error":{"code":"not_found","message":"there is no such route"}}"#;
        let _ = write!(
            client,
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        return;
    }
    let mut forwarded: Vec<u8> = head
        .lines()
        .filter(|line| !line.is_empty() && !line.to_ascii_lowercase().starts_with("connection:"))
        .flat_map(|line| format!("{line}\r\n").into_bytes())
        .collect();
    forwarded.extend_from_slice(b"Connection: close\r\n\r\n");
    forwarded.extend_from_slice(&request[head_end..]);
    let Ok(mut server) = TcpStream::connect(upstream) else {
        return;
    };
    if server.write_all(&forwarded).is_err() {
        return;
    }
    let _ = std::io::copy(&mut server, &mut client);
}
