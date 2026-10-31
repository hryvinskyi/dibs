//! The pool of worktrees as the guard reaches it, over the command line's own take, and git as the
//! guard uses it to make and remove the worktrees a host would make and remove itself.
//!
//! A take for a host waits as long as the host's deadline allows and talks to the server with the
//! client's ordinary limits, as it may wait for a worktree to be freed and reset; every other call
//! is bounded like a hook's. What this machine records of the pool's holds is read without the
//! server, so asking whether a session holds anything costs nothing. A server older than the
//! worktree routes has nothing to release or end: what this machine recorded is forgotten.

use std::cell::{OnceCell, RefCell};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use tokio::runtime::Runtime;

use dibs_agent_host::adapter::OwnWorktrees;
use dibs_agent_host::event::WorktreeLabel;
use dibs_agent_host::ports::{
    Clock, Git as HostGit, PortError, Removal, Retaken, SessionRef, WorktreeHolder, WorktreePool,
    WorktreeRecord, WorktreeTake,
};
use dibs_client::api::{
    ReleaseWorktreeRequest, TakeWorktreeRequest, WorktreeOutcome, WorktreeRelease,
};
use dibs_client::checkout::Checkout;
use dibs_client::state::WorktreeHoldRecord;
use dibs_client::{Client, ClientError, ClientOptions};
use dibs_domain::ids::SessionId;
use dibs_domain::report::MachineId;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::WorktreeName;

use crate::commands::worktree::DEFAULT_PURPOSE;
use crate::commands::worktree::pool::Pool;
use crate::commands::worktree::project::ShellCommands;
use crate::commands::worktree::rescue::{Author, WorktreeGit};
use crate::commands::worktree::take::{Ask, TakeError, Taken, Taker};
use crate::failure::sentence;
use crate::guard::SystemClock;
use crate::guard::machine::machine_id;
use crate::guard::remote::{Remote, RemoteError, session_label};
use crate::guard::views;

/// How often a take for a host asks again while every worktree is held.
const POLL_EVERY: Duration = Duration::from_secs(5);

/// The pool of one checkout, as the guard reaches it.
pub struct ClientWorktreePool {
    pool: Pool,
    remote: Remote,
    state_base: PathBuf,
    root: OnceCell<PathBuf>,
    runtime: OnceCell<Runtime>,
    client: OnceCell<Client>,
}

impl ClientWorktreePool {
    /// The pool `checkout` lends or belongs to, with this machine's state under `state_base`;
    /// `None` when it has none.
    pub fn of(checkout: &Checkout, remote: &Remote, state_base: &Path) -> Option<Self> {
        let (pool, _) = Pool::locate(checkout, state_base).ok()?;
        Some(Self {
            pool,
            remote: remote.clone(),
            state_base: state_base.to_path_buf(),
            root: OnceCell::new(),
            runtime: OnceCell::new(),
            client: OnceCell::new(),
        })
    }

    fn runtime(&self) -> Result<&Runtime, PortError> {
        if let Some(runtime) = self.runtime.get() {
            return Ok(runtime);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| PortError::Refused("dibs could not start its runtime.".to_owned()))?;
        Ok(self.runtime.get_or_init(|| runtime))
    }

    /// The client of the calls bounded like a hook's.
    fn client(&self) -> Result<&Client, PortError> {
        if let Some(client) = self.client.get() {
            return Ok(client);
        }
        let client = self.remote.client().map_err(|error| remote_error(&error))?;
        Ok(self.client.get_or_init(|| client))
    }

    fn machine(&self) -> Result<MachineId, PortError> {
        machine_id(&self.state_base).ok_or_else(|| {
            PortError::Refused(
                "This machine's id can be neither read nor kept, so its pool of worktrees cannot be named."
                    .to_owned(),
            )
        })
    }

    /// The id this machine registered `session` under, read without the server.
    fn known_session(&self, session: &SessionRef) -> Option<SessionId> {
        let label = session_label(session).ok()?;
        self.pool.state().session(&label)
    }

    /// The pool's directory as the file system resolves it.
    fn root(&self) -> &Path {
        self.root
            .get_or_init(|| resolved(&self.pool.settings().root))
    }

    /// Takes the worktree for `holder` within `deadline`, every step that can fail answered as
    /// the reason the pool cannot serve.
    fn take_within(
        &self,
        holder: &WorktreeHolder,
        deadline: Duration,
    ) -> Result<WorktreeTake, String> {
        let started = Instant::now();
        let runtime = self.runtime().map_err(|error| error.to_string())?;
        let client = self
            .remote
            .client_with(ClientOptions::default())
            .map_err(|error| remote_error(&error).to_string())?;
        let machine = self.machine().map_err(|error| error.to_string())?;
        let purpose = Purpose::parse(DEFAULT_PURPOSE)
            .map_err(|_| "dibs could not name the take's purpose.".to_owned())?;
        runtime.block_on(async {
            let session = self
                .remote
                .session(&client, &holder.session)
                .await
                .map_err(|error| remote_error(&error).to_string())?;
            let me = self
                .remote
                .me(&client, self.pool.checkout(), SystemClock.now())
                .await
                .map_err(|error| remote_error(&error).to_string())?;
            let notes = RefCell::new(Vec::new());
            let note = |text: &str| notes.borrow_mut().push(text.to_owned());
            let taker = Taker {
                client: &client,
                machine,
                pool: &self.pool,
                commands: &ShellCommands,
                clock: &SystemClock,
                author: Author::of(&me.member),
                notes: &note,
            };
            let left = deadline.saturating_sub(started.elapsed());
            let ask = Ask {
                session,
                agent: holder.agent.clone(),
                purpose,
                item: None,
                wait: left,
                deadline: Some(left),
                poll_every: POLL_EVERY,
            };
            let taken = taker.take(&ask).await;
            Ok(took(taken, notes.take()))
        })
    }

    /// Forgets this machine's record of `name` and remembers it free since now.
    fn forget(&self, name: &WorktreeName) {
        let files = self.pool.files();
        if let Err(error) = files.forget_hold(name) {
            tracing::debug!(error = %error, "a worktree's holder could not be forgotten");
        }
        if let Err(error) = files.mark_free(name, SystemClock.now()) {
            tracing::debug!(error = %error, "a worktree's release could not be recorded");
        }
    }
}

/// What a take came to, as the guard reads it: anything but a worktree made ready or every
/// worktree held is a pool that cannot serve.
fn took(taken: Result<Taken, TakeError>, notes: Vec<String>) -> WorktreeTake {
    match taken {
        Ok(Taken::Took(taken)) => WorktreeTake::Took {
            worktree: taken.worktree,
            path: taken.path,
            notes,
        },
        Ok(Taken::Busy { sentence, .. }) => WorktreeTake::Busy(sentence),
        Err(TakeError::Server(error)) => WorktreeTake::CannotServe(sentence(&error.to_string())),
        Err(TakeError::Unusable(reason)) => WorktreeTake::CannotServe(reason),
    }
}

fn remote_error(error: &RemoteError) -> PortError {
    match error {
        RemoteError::Client(error) => views::port_error(error),
        RemoteError::Config(text) => PortError::Refused(sentence(text)),
    }
}

/// Whether a failed release or end leaves nothing for the server to let go of: the hold was gone
/// already, or the server keeps no worktree holds at all.
fn nothing_to_let_go(error: &ClientError) -> bool {
    matches!(error, ClientError::ServerTooOld { .. }) || error.code() == "worktree_not_held"
}

impl WorktreePool for ClientWorktreePool {
    fn take(&self, holder: &WorktreeHolder, deadline: Duration) -> WorktreeTake {
        self.take_within(holder, deadline)
            .unwrap_or_else(WorktreeTake::CannotServe)
    }

    fn retake(&self, name: &WorktreeName, holder: &WorktreeHolder) -> Result<Retaken, PortError> {
        let client = self.client()?;
        let machine = self.machine()?;
        let purpose = Purpose::parse(DEFAULT_PURPOSE).map_err(|_| {
            PortError::Refused("dibs could not name the take's purpose.".to_owned())
        })?;
        let runtime = self.runtime()?;
        let (session, took) = runtime
            .block_on(async {
                let session = self.remote.session(client, &holder.session).await?;
                let request = TakeWorktreeRequest {
                    session_id: session,
                    machine,
                    pool: self.pool.id().clone(),
                    names: vec![name.clone()],
                    agent: holder.agent.clone(),
                    purpose: purpose.clone(),
                    item: None,
                };
                let took = client.take_worktree(&request).await?;
                Ok::<_, RemoteError>((session, took))
            })
            .map_err(|error| remote_error(&error))?;
        if took.outcome == WorktreeOutcome::Took && took.worktree.as_ref() == Some(name) {
            let record = WorktreeHoldRecord {
                session_id: session,
                agent: holder.agent.clone(),
                purpose: Some(purpose),
                item: None,
                taken_at: SystemClock.now(),
                reset_done: true,
            };
            let files = self.pool.files();
            if let Err(error) = files.remember_hold(name, &record) {
                tracing::debug!(error = %error, "a worktree's holder could not be recorded");
            }
            if let Err(error) = files.mark_taken(name) {
                tracing::debug!(error = %error, "the free and stopped marks could not be cleared");
            }
            return Ok(Retaken::Took);
        }
        let hold = took.held.iter().find(|hold| hold.worktree == *name);
        Ok(Retaken::Held {
            agent: hold.and_then(|hold| hold.agent.clone()),
            purpose: hold.map(|hold| hold.purpose.clone()),
        })
    }

    fn release(&self, name: &WorktreeName) -> Result<(), PortError> {
        let Some(record) = self.pool.files().hold(name) else {
            return Ok(());
        };
        let client = self.client()?;
        let request = ReleaseWorktreeRequest {
            session_id: record.session_id,
            machine: self.machine()?,
            pool: self.pool.id().clone(),
            release: WorktreeRelease::Holder {
                holder_session: record.session_id,
                agent: record.agent.clone(),
            },
        };
        match self.runtime()?.block_on(client.release_worktree(&request)) {
            Ok(_) => {}
            Err(error) if nothing_to_let_go(&error) => {}
            Err(error) => return Err(views::port_error(&error)),
        }
        self.forget(name);
        Ok(())
    }

    fn end_session(&self, session: &SessionRef) -> Result<(), PortError> {
        let Some(id) = self.known_session(session) else {
            return Ok(());
        };
        let client = self.client()?;
        match self.runtime()?.block_on(client.end_session(id)) {
            Ok(_) => {}
            Err(error) if nothing_to_let_go(&error) => {}
            Err(error) => return Err(views::port_error(&error)),
        }
        for (name, record) in self.pool.files().holds() {
            if record.session_id == id {
                self.forget(&name);
            }
        }
        Ok(())
    }

    fn records(&self, session: &SessionRef) -> Vec<WorktreeRecord> {
        let id = self.known_session(session);
        self.pool
            .files()
            .holds()
            .into_iter()
            .map(|(worktree, record)| WorktreeRecord {
                worktree,
                this_session: Some(record.session_id) == id,
                agent: record.agent,
                purpose: record.purpose,
                since: record.taken_at,
            })
            .collect()
    }

    fn rescued(&self, name: &WorktreeName) -> Option<String> {
        WorktreeGit::new(self.pool.checkout().root())
            .rescue_branches(name)
            .into_iter()
            .max()
    }

    fn touch(&self, name: &WorktreeName, at: OffsetDateTime) {
        if let Err(error) = self.pool.files().touch_beat(name, at) {
            tracing::debug!(error = %error, "a sign of life could not be left");
        }
    }

    fn worktree_at(&self, cwd: &Path, path: &Path) -> Option<WorktreeName> {
        let full = resolved(&cwd.join(path));
        let inside = full.strip_prefix(self.root()).ok()?;
        let Some(Component::Normal(first)) = inside.components().next() else {
            return None;
        };
        let name = WorktreeName::parse(&first.to_string_lossy()).ok()?;
        self.pool.names().contains(&name).then_some(name)
    }
}

/// `path` as the file system resolves it: its longest part that exists made canonical, the rest
/// added back as written, so a file not made yet resolves like the directory it goes into.
fn resolved(path: &Path) -> PathBuf {
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(canonical) = fs::canonicalize(existing) {
            let mut resolved = canonical;
            for part in rest.iter().rev() {
                resolved.push(part);
            }
            return resolved;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Git, making and removing the worktrees a host would make and remove itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnWorktreeGit;

impl OwnWorktreeGit {
    fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map_err(|_| "git could not be started".to_owned())?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
        }
        let said = String::from_utf8_lossy(&output.stderr);
        let first = said
            .lines()
            .map(|line| line.trim().trim_start_matches("fatal: "))
            .find(|line| !line.is_empty())
            .unwrap_or("git failed");
        Err(first.to_owned())
    }
}

impl HostGit for OwnWorktreeGit {
    fn make_worktree(
        &self,
        cwd: &Path,
        own: &OwnWorktrees,
        label: &WorktreeLabel,
    ) -> Result<PathBuf, String> {
        let top = Self::git(cwd, &["rev-parse", "--show-toplevel"])
            .map_err(|_| format!("{} lies in no git repository", cwd.display()))?;
        let top = PathBuf::from(top);
        let path = top.join(own.dir).join(label.as_str());
        if path.join(".git").exists() {
            return Ok(path);
        }
        let branch = format!("{}{label}", own.branch_prefix);
        let at = path.to_string_lossy().into_owned();
        Self::git(&top, &["worktree", "add", "-b", &branch, &at, "HEAD"])
            .or_else(|_| Self::git(&top, &["worktree", "add", &at, &branch]))
            .map(|_| path)
    }

    fn remove_worktree(&self, path: &Path) -> Removal {
        if !path.exists() {
            return Removal::Absent;
        }
        if !path.join(".git").is_file() {
            return Removal::Kept("it is no linked git worktree".to_owned());
        }
        let at = path.to_string_lossy().into_owned();
        match Self::git(path, &["worktree", "remove", &at]) {
            Ok(_) => Removal::Removed,
            Err(said) => Removal::Kept(said),
        }
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::worktree::AgentKey;

    use super::*;
    use crate::commands::worktree::take::TakenWorktree;

    fn name(text: &str) -> WorktreeName {
        WorktreeName::parse(text).unwrap()
    }

    #[test]
    fn a_take_that_neither_made_a_worktree_ready_nor_found_all_held_cannot_serve() {
        let ready = Taken::Took(TakenWorktree {
            worktree: name("worktree2"),
            path: PathBuf::from("/pool/worktree2"),
            session_id: SessionId(3),
            agent: AgentKey::parse("a1").ok(),
            again: false,
            prepared: false,
            rescued: None,
        });
        assert_eq!(
            took(Ok(ready), vec!["saved".to_owned()]),
            WorktreeTake::Took {
                worktree: name("worktree2"),
                path: PathBuf::from("/pool/worktree2"),
                notes: vec!["saved".to_owned()],
            }
        );
        let busy = Taken::Busy {
            sentence: "Every worktree is held.".to_owned(),
            held: Vec::new(),
            preparing: None,
        };
        assert_eq!(
            took(Ok(busy), Vec::new()),
            WorktreeTake::Busy("Every worktree is held.".to_owned())
        );
        assert_eq!(
            took(
                Err(TakeError::Unusable(
                    "worktree1: its reset failed".to_owned()
                )),
                Vec::new()
            ),
            WorktreeTake::CannotServe("worktree1: its reset failed".to_owned())
        );
        let unreachable = ClientError::Network {
            server: "https://dibs.example.com".to_owned(),
        };
        assert!(matches!(
            took(Err(TakeError::Server(unreachable)), Vec::new()),
            WorktreeTake::CannotServe(_)
        ));
    }

    #[test]
    fn a_path_not_made_yet_resolves_like_the_directory_it_goes_into() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            resolved(&dir.path().join("worktree1/src/new.rs")),
            canonical.join("worktree1/src/new.rs")
        );
        assert_eq!(resolved(dir.path()), canonical);
    }

    fn repository() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = fs::canonicalize(dir.path()).unwrap().join("widgets");
        fs::create_dir_all(&repo).unwrap();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=a",
                "-c",
                "user.email=a@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
        ] {
            OwnWorktreeGit::git(&repo, args).unwrap();
        }
        (dir, repo)
    }

    const OWN: OwnWorktrees = OwnWorktrees {
        dir: "tool/worktrees",
        branch_prefix: "worktree-",
    };

    #[test]
    fn the_hosts_own_worktree_is_made_on_a_new_branch_and_made_once() {
        let (_dir, repo) = repository();
        fs::create_dir_all(repo.join("src")).unwrap();
        let label = WorktreeLabel::parse("agent-a1").unwrap();
        let path = OwnWorktreeGit
            .make_worktree(&repo.join("src"), &OWN, &label)
            .unwrap();
        assert_eq!(path, repo.join("tool/worktrees/agent-a1"));
        assert_eq!(
            OwnWorktreeGit::git(&path, &["branch", "--show-current"]).unwrap(),
            "worktree-agent-a1"
        );
        assert_eq!(
            OwnWorktreeGit.make_worktree(&repo, &OWN, &label).unwrap(),
            path,
            "made once, answered again"
        );
        let outside = tempfile::tempdir().unwrap();
        let refused = OwnWorktreeGit
            .make_worktree(outside.path(), &OWN, &label)
            .unwrap_err();
        assert!(refused.contains("lies in no git repository"), "{refused}");
    }

    #[test]
    fn a_worktree_is_removed_unless_it_holds_changes_and_never_the_repository_itself() {
        let (_dir, repo) = repository();
        let clean = OwnWorktreeGit
            .make_worktree(&repo, &OWN, &WorktreeLabel::parse("clean").unwrap())
            .unwrap();
        assert_eq!(OwnWorktreeGit.remove_worktree(&clean), Removal::Removed);
        assert!(!clean.exists());
        assert_eq!(OwnWorktreeGit.remove_worktree(&clean), Removal::Absent);
        let dirty = OwnWorktreeGit
            .make_worktree(&repo, &OWN, &WorktreeLabel::parse("dirty").unwrap())
            .unwrap();
        fs::write(dirty.join("notes.txt"), "keep me").unwrap();
        assert!(matches!(
            OwnWorktreeGit.remove_worktree(&dirty),
            Removal::Kept(said) if said.contains("modified or untracked")
        ));
        assert!(dirty.join("notes.txt").exists());
        assert!(matches!(
            OwnWorktreeGit.remove_worktree(&repo),
            Removal::Kept(_)
        ));
    }
}
