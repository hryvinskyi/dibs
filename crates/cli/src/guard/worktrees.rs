//! The pool of worktrees as the guard reaches it, over the command line's own take, and git as the
//! guard uses it to make and remove the worktrees a host would make and remove itself.
//!
//! A worktree the host would make itself is a linked worktree of the checkout, so the checkout's
//! `.dibs.toml` governs it without anything written into it. A worktree whose git directory marks it
//! as a pool's is never removed. Windows refuses to delete a directory any process works in, and a
//! host starts the guard inside the worktree it gives back: before git removes one, this process
//! moves to the repository's main work tree, and git runs from there. Every path handed to git or
//! printed is in its ordinary spelling.
//!
//! A take for a host waits as long as the host's deadline allows and talks to the server with the
//! client's ordinary limits, as it may wait for a worktree to be freed and reset; every other call
//! is bounded like a hook's. What this machine records of the pool's holds is read without the
//! server, so asking whether a session holds anything costs nothing. A server older than the
//! worktree routes has nothing to release or end: what this machine recorded is forgotten.

use std::cell::{OnceCell, RefCell};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use tokio::runtime::Runtime;

use dibs_agent_host::adapter::OwnWorktrees;
use dibs_agent_host::event::WorktreeLabel;
use dibs_agent_host::ports::{
    Afterwards, Clock, Git as HostGit, PortError, Removal, Retaken, SessionRef, WorktreeHolder,
    WorktreePool, WorktreeRecord, WorktreeTake,
};
use dibs_client::api::{
    ReleaseWorktreeRequest, TakeWorktreeRequest, WorktreeOutcome, WorktreeRelease,
};
use dibs_client::checkout::Checkout;
use dibs_client::git_dirs::GitDirs;
use dibs_client::paths::{plain, resolved};
use dibs_client::state::WorktreeHoldRecord;
use dibs_client::worktree_pool::read_marker;
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
    /// The pool `checkout` lends or belongs to, with this machine's state under `state_base`.
    ///
    /// # Errors
    /// `None` when its repository declares no pool; the sentence saying why when it declares one
    /// that cannot be used.
    pub fn of(
        checkout: &Checkout,
        remote: &Remote,
        state_base: &Path,
    ) -> Result<Self, Option<String>> {
        let (pool, _) = Pool::find(checkout, state_base)
            .map_err(|failure| Some(failure.message))?
            .ok_or(None)?;
        Ok(Self {
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

    /// Forgets this machine's record of `name`, held by `session` or one of its agents; when it
    /// is free `afterwards`, remembers it free since now. A worktree left in use is not written
    /// free: its last sign of life, newer than any release written here, keeps it from other takes
    /// for an agent's silence, or until `session` ends.
    fn forget(&self, name: &WorktreeName, session: SessionId, afterwards: Afterwards) {
        let files = self.pool.files();
        if let Err(error) = files.forget_hold(name) {
            tracing::debug!(error = %error, "a worktree's holder could not be forgotten");
        }
        if afterwards == Afterwards::InUse {
            if let Err(error) = files.mark_left(name, session) {
                tracing::debug!(error = %error, "the session that left a worktree could not be recorded");
            }
            return;
        }
        if let Err(error) = files.mark_free(name, SystemClock.now()) {
            tracing::debug!(error = %error, "a worktree's release could not be recorded");
        }
    }
}

/// What a take came to, as the guard reads it: a worktree made ready, every worktree held, a pool
/// still being prepared; anything else is a pool that cannot serve.
fn took(taken: Result<Taken, TakeError>, notes: Vec<String>) -> WorktreeTake {
    match taken {
        Ok(Taken::Took(taken)) => WorktreeTake::Took {
            worktree: taken.worktree,
            path: taken.path,
            notes,
        },
        Ok(Taken::Busy { sentence, .. }) => WorktreeTake::Busy(sentence),
        Ok(Taken::Preparing { sentence, .. }) => WorktreeTake::Preparing(sentence),
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
        if took.outcome == WorktreeOutcome::Took
            && let Some(held) = took.worktree.as_ref().filter(|held| *held != name)
        {
            return Ok(Retaken::HoldsAnother {
                worktree: held.clone(),
            });
        }
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

    fn release(&self, name: &WorktreeName, afterwards: Afterwards) -> Result<(), PortError> {
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
        self.forget(name, record.session_id, afterwards);
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
                self.forget(&name, id, Afterwards::Free);
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

    fn free_left(&self, session: &SessionRef) {
        let Some(id) = self.known_session(session) else {
            return;
        };
        let files = self.pool.files();
        for name in files.left_by(id) {
            if let Err(error) = files.mark_free(&name, SystemClock.now()) {
                tracing::debug!(error = %error, "a worktree's release could not be recorded");
            }
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

/// The working directory of this process.
pub trait WorkingDirectory {
    /// Makes `dir` this process's working directory.
    ///
    /// # Errors
    /// The system's failure, as when `dir` does not exist.
    fn enter(&self, dir: &Path) -> io::Result<()>;
}

/// This process's own working directory, as the system keeps it.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessDirectory;

impl WorkingDirectory for ProcessDirectory {
    fn enter(&self, dir: &Path) -> io::Result<()> {
        std::env::set_current_dir(dir)
    }
}

/// Git, making and removing the worktrees a host would make and remove itself. Before git removes
/// a worktree, this process moves its working directory to the repository's main work tree, so
/// that it never stands in the directory being deleted.
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnWorktreeGit<D = ProcessDirectory> {
    directory: D,
}

impl<D> OwnWorktreeGit<D> {
    /// Git, moving this process's working directory through `directory`.
    pub const fn new(directory: D) -> Self {
        Self { directory }
    }
}

/// Git run in `dir`: its output, or the first line of what it said when it failed.
fn run_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(plain(dir))
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

impl<D: WorkingDirectory> HostGit for OwnWorktreeGit<D> {
    fn make_worktree(
        &self,
        cwd: &Path,
        own: &OwnWorktrees,
        label: &WorktreeLabel,
    ) -> Result<PathBuf, String> {
        let top = run_git(cwd, &["rev-parse", "--show-toplevel"])
            .map_err(|_| format!("{} lies in no git repository", plain(cwd).display()))?;
        let top = plain(Path::new(&top));
        let path = plain(&top.join(own.dir).join(label.as_str()));
        if !path.join(".git").exists() {
            let branch = format!("{}{label}", own.branch_prefix);
            let at = path.to_string_lossy().into_owned();
            run_git(&top, &["worktree", "add", "-b", &branch, &at, "HEAD"])
                .or_else(|_| run_git(&top, &["worktree", "add", &at, &branch]))?;
        }
        Ok(path)
    }

    fn remove_worktree(&self, path: &Path) -> Removal {
        if !path.exists() {
            return Removal::Absent;
        }
        let Some(dirs) = GitDirs::at(path).filter(GitDirs::is_linked) else {
            return Removal::Kept("it is no linked git worktree".to_owned());
        };
        if read_marker(&dirs).is_some() {
            return Removal::Pooled;
        }
        let home = dirs
            .main_work_tree()
            .unwrap_or_else(|| dirs.common_dir().to_path_buf());
        if let Err(error) = self.directory.enter(&plain(&home)) {
            tracing::debug!(error = %error, "the process could not leave the worktree it removes");
        }
        let at = plain(path).to_string_lossy().into_owned();
        match run_git(&home, &["worktree", "remove", &at]) {
            Ok(_) => Removal::Removed,
            Err(said) => Removal::Kept(said),
        }
    }

    fn plain(&self, path: &Path) -> PathBuf {
        plain(path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use dibs_domain::worktree::AgentKey;

    use dibs_client::checkout::FILE_NAME;
    use dibs_client::paths::canonical;

    use super::*;
    use crate::commands::worktree::take::TakenWorktree;

    fn name(text: &str) -> WorktreeName {
        WorktreeName::parse(text).unwrap()
    }

    #[test]
    fn a_take_answers_held_and_being_prepared_apart_and_anything_else_cannot_serve() {
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
        };
        assert_eq!(
            took(Ok(busy), Vec::new()),
            WorktreeTake::Busy("Every worktree is held.".to_owned())
        );
        let preparing = Taken::Preparing {
            sentence: "worktree1 is being prepared and can be taken once ready.".to_owned(),
            preparing: Some(name("worktree1")),
        };
        assert_eq!(
            took(Ok(preparing), Vec::new()),
            WorktreeTake::Preparing(
                "worktree1 is being prepared and can be taken once ready.".to_owned()
            ),
            "a pool still being prepared is not a pool whose every worktree is held"
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

    fn repository() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = canonical(dir.path()).unwrap().join("widgets");
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
            run_git(&repo, args).unwrap();
        }
        (dir, repo)
    }

    const OWN: OwnWorktrees = OwnWorktrees {
        dir: "tool/worktrees",
        branch_prefix: "worktree-",
    };

    /// A working directory that only records where it was sent, and whether the directory it
    /// watches still stood at that moment.
    #[derive(Default)]
    struct Entered {
        watched: Option<PathBuf>,
        refused: bool,
        entered: RefCell<Vec<(PathBuf, bool)>>,
    }

    impl Entered {
        /// Records whether `dir` still stands each time it is sent somewhere.
        fn watching(dir: &Path) -> Self {
            Self {
                watched: Some(dir.to_path_buf()),
                ..Self::default()
            }
        }

        /// Where it was sent, each made canonical, and whether the watched directory stood then.
        fn entered(&self) -> Vec<(PathBuf, bool)> {
            self.entered
                .borrow()
                .iter()
                .map(|(dir, stood)| (canonical(dir).unwrap_or_else(|_| dir.clone()), *stood))
                .collect()
        }
    }

    impl WorkingDirectory for Entered {
        fn enter(&self, dir: &Path) -> io::Result<()> {
            let stood = self.watched.as_deref().is_some_and(Path::exists);
            self.entered.borrow_mut().push((dir.to_path_buf(), stood));
            if self.refused {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            Ok(())
        }
    }

    /// Git whose moves of the working directory are only recorded: a test never moves the
    /// working directory of the process every test shares.
    fn own() -> OwnWorktreeGit<Entered> {
        OwnWorktreeGit::new(Entered::default())
    }

    #[test]
    fn the_hosts_own_worktree_is_made_on_a_new_branch_and_made_once() {
        let (_dir, repo) = repository();
        fs::create_dir_all(repo.join("src")).unwrap();
        let label = WorktreeLabel::parse("agent-a1").unwrap();
        let path = own()
            .make_worktree(&repo.join("src"), &OWN, &label)
            .unwrap();
        assert_eq!(path, repo.join("tool/worktrees/agent-a1"));
        assert_eq!(
            run_git(&path, &["branch", "--show-current"]).unwrap(),
            "worktree-agent-a1"
        );
        assert_eq!(
            own().make_worktree(&repo, &OWN, &label).unwrap(),
            path,
            "made once, answered again"
        );
        let outside = tempfile::tempdir().unwrap();
        let refused = own()
            .make_worktree(outside.path(), &OWN, &label)
            .unwrap_err();
        assert!(refused.contains("lies in no git repository"), "{refused}");
    }

    #[cfg(windows)]
    #[test]
    fn a_directory_given_in_its_resolved_spelling_is_named_in_its_ordinary_one() {
        let outside = tempfile::tempdir().unwrap();
        let resolved = fs::canonicalize(outside.path()).unwrap();
        assert!(resolved.to_string_lossy().starts_with(r"\\?\"));
        let label = WorktreeLabel::parse("agent-a1").unwrap();
        let refused = own().make_worktree(&resolved, &OWN, &label).unwrap_err();
        assert!(
            refused.contains(&plain(&resolved).display().to_string()) && !refused.contains(r"\\?\"),
            "{refused}"
        );
        assert_eq!(
            own().plain(&resolved.join("api")),
            plain(&resolved).join("api")
        );
    }

    /// A `.dibs.toml` naming the project `acme` and its repository `api`.
    const CONFIG: &str =
        "server = \"https://dibs.example.com\"\nproject = \"acme\"\nrepo = \"api\"\n";

    #[test]
    fn the_hosts_own_worktree_is_governed_by_the_checkouts_file_and_nothing_is_written() {
        let (_dir, repo) = repository();
        fs::write(repo.join(FILE_NAME), CONFIG).unwrap();
        fs::write(repo.join(".git/info/exclude"), "/.dibs.toml\n/tool/\n").unwrap();
        let exclude = fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        let label = WorktreeLabel::parse("agent-a1").unwrap();
        let path = own().make_worktree(&repo, &OWN, &label).unwrap();
        fs::create_dir_all(path.join("src")).unwrap();
        assert!(!path.join(FILE_NAME).exists(), "nothing is written into it");
        assert_eq!(
            fs::read_to_string(repo.join(".git/info/exclude")).unwrap(),
            exclude,
            "nor into the repository's exclusions"
        );
        assert_eq!(
            run_git(&path, &["status", "--porcelain", "--ignored"]).unwrap(),
            ""
        );
        let governed = Checkout::discover(&path.join("src")).unwrap().unwrap();
        assert_eq!(governed.root(), path, "its paths are its own");
        assert_eq!(governed.file(), repo.join(FILE_NAME));
        assert_eq!(
            governed.config().repo.as_ref().map(|repo| repo.as_str()),
            Some("api")
        );
    }

    #[test]
    fn a_worktree_marked_as_a_pools_is_never_removed() {
        let (dir, repo) = repository();
        let pooled = dir.path().join("pool/worktree1");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &pooled.to_string_lossy(),
                "HEAD",
            ],
        )
        .unwrap();
        assert_eq!(
            own().remove_worktree(&pooled),
            Removal::Removed,
            "an unmarked worktree is removed as git removes one"
        );
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &pooled.to_string_lossy(),
                "HEAD",
            ],
        )
        .unwrap();
        let dirs = GitDirs::at(&pooled).unwrap();
        dibs_client::worktree_pool::write_marker(
            &dirs,
            &dibs_client::worktree_pool::PoolWorktree {
                checkout: repo.clone(),
                name: name("worktree1"),
            },
        )
        .unwrap();
        assert_eq!(own().remove_worktree(&pooled), Removal::Pooled);
        assert!(pooled.join(".git").exists(), "kept");
    }

    #[test]
    fn a_worktree_is_removed_unless_it_holds_changes_and_never_the_repository_itself() {
        let (_dir, repo) = repository();
        let clean = own()
            .make_worktree(&repo, &OWN, &WorktreeLabel::parse("clean").unwrap())
            .unwrap();
        assert_eq!(own().remove_worktree(&clean), Removal::Removed);
        assert!(!clean.exists());
        assert_eq!(own().remove_worktree(&clean), Removal::Absent);
        let dirty = own()
            .make_worktree(&repo, &OWN, &WorktreeLabel::parse("dirty").unwrap())
            .unwrap();
        fs::write(dirty.join("notes.txt"), "keep me").unwrap();
        assert!(matches!(
            own().remove_worktree(&dirty),
            Removal::Kept(said) if said.contains("modified or untracked")
        ));
        assert!(dirty.join("notes.txt").exists());
        assert!(matches!(own().remove_worktree(&repo), Removal::Kept(_)));
    }

    #[test]
    fn the_process_moves_to_the_main_work_tree_before_git_removes_a_worktree() {
        let (dir, repo) = repository();
        let label = WorktreeLabel::parse("clean").unwrap();
        let clean = own().make_worktree(&repo, &OWN, &label).unwrap();
        let git = OwnWorktreeGit::new(Entered::watching(&clean));
        assert_eq!(git.remove_worktree(&clean), Removal::Removed);
        assert_eq!(
            git.directory.entered(),
            [(repo.clone(), true)],
            "moved to the main work tree while the worktree still stood"
        );

        assert_eq!(git.remove_worktree(&clean), Removal::Absent);
        assert_eq!(
            git.remove_worktree(&repo),
            Removal::Kept("it is no linked git worktree".to_owned())
        );
        let pooled = dir.path().join("pool/worktree1");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &pooled.to_string_lossy(),
                "HEAD",
            ],
        )
        .unwrap();
        dibs_client::worktree_pool::write_marker(
            &GitDirs::at(&pooled).unwrap(),
            &dibs_client::worktree_pool::PoolWorktree {
                checkout: repo.clone(),
                name: name("worktree1"),
            },
        )
        .unwrap();
        assert_eq!(git.remove_worktree(&pooled), Removal::Pooled);
        assert_eq!(
            git.directory.entered().len(),
            1,
            "nothing removed, nowhere moved"
        );

        let stuck = OwnWorktreeGit::new(Entered {
            refused: true,
            ..Entered::default()
        });
        let again = own().make_worktree(&repo, &OWN, &label).unwrap();
        assert_eq!(
            stuck.remove_worktree(&again),
            Removal::Removed,
            "a process that cannot move still has git try"
        );
        assert_eq!(stuck.directory.entered(), [(repo, false)]);
    }

    #[test]
    fn a_worktree_of_a_repository_without_a_main_work_tree_is_removed_from_its_git_directory() {
        let (dir, repo) = repository();
        let home = canonical(dir.path()).unwrap();
        let bare = home.join("widgets.git");
        run_git(
            &repo,
            &["clone", "-q", "--bare", ".", &bare.to_string_lossy()],
        )
        .unwrap();
        let linked = home.join("linked");
        run_git(
            &bare,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &linked.to_string_lossy(),
                "HEAD",
            ],
        )
        .unwrap();
        let git = own();
        assert_eq!(git.remove_worktree(&linked), Removal::Removed);
        assert!(!linked.exists());
        assert_eq!(
            git.directory.entered(),
            [(canonical(&bare).unwrap(), false)],
            "moved to the repository's git directory"
        );
        assert_eq!(
            own().remove_worktree(&bare),
            Removal::Kept("it is no linked git worktree".to_owned())
        );
    }
}
