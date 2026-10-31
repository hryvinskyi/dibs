//! The governed checkouts the guard works in: the nearest `.dibs.toml` above an event's directory,
//! the repository it says it is (confirmed against its `origin`), the project's state, server and
//! upkeep, and the pool of worktrees the checkout lends or belongs to.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use dibs_agent_host::ports::{
    CoordinationGateway, DeliveryLedger, Git as HostGit, LocalCache, PortError, Tallies, Upkeep,
    Workspace, Workspaces, WorktreePool,
};
use dibs_agent_host::upkeep::is_fresh;
use dibs_client::branch::{current_branch, head};
use dibs_client::checkout::Checkout;
use dibs_client::locations::Locations;
use dibs_client::state::{StateDir, read_json, write_json};
use dibs_domain::paths::RelPath;
use dibs_domain::push::GitFacts;
use dibs_domain::radar::BranchName;
use dibs_domain::repo::{RemoteUrl, RepoId};
use dibs_domain::settings::ProjectSettings;

use crate::failure::sentence;
use crate::git::Git;
use crate::guard::cache::StateCache;
use crate::guard::gateway::HookGateway;
use crate::guard::ledger::SessionLedgers;
use crate::guard::remote::{Remote, RemoteError, checkout_of};
use crate::guard::report::StateTallies;
use crate::guard::upkeep::DetachedSync;
use crate::guard::worktrees::{ClientWorktreePool, OwnWorktreeGit};

/// The variable that names a git directory other than the checkout's own.
pub const GIT_DIR: &str = "GIT_DIR";

/// How long a checkout's `origin`, once read from git, is taken as it was.
pub const ORIGIN_FRESH: Duration = Duration::minutes(10);

/// Every checkout on this machine, found from the environment the binary received.
pub struct ClientWorkspaces {
    env: HashMap<String, String>,
    git: OwnWorktreeGit,
}

impl ClientWorkspaces {
    /// The checkouts, with the client's directories taken from `env`.
    pub fn new(env: HashMap<String, String>) -> Self {
        Self {
            env,
            git: OwnWorktreeGit,
        }
    }
}

impl Workspaces for ClientWorkspaces {
    fn open(&self, cwd: &Path) -> Result<Option<Box<dyn Workspace + '_>>, PortError> {
        let checkout = match checkout_of(cwd) {
            Ok(Some(checkout)) => checkout,
            Ok(None) => return Ok(None),
            Err(RemoteError::Config(text)) => return Err(PortError::Refused(text)),
            Err(RemoteError::Client(error)) => {
                return Err(PortError::Refused(sentence(&error.to_string())));
            }
        };
        let locations = Locations::from_env(&|name| self.env.get(name).cloned())
            .map_err(|error| PortError::Refused(sentence(&error.to_string())))?;
        let remote = Remote::new(&checkout, &locations, &self.env);
        let git_dir = self.env.get(GIT_DIR).map(PathBuf::from);
        Ok(Some(Box::new(ClientWorkspace::new(
            checkout,
            remote,
            git_dir,
            locations.state_dir().to_path_buf(),
        ))))
    }

    fn git(&self) -> &dyn HostGit {
        &self.git
    }
}

/// One checkout, its project and the ports over them.
pub struct ClientWorkspace {
    checkout: Checkout,
    git_dir: Option<PathBuf>,
    state: StateDir,
    cache: StateCache,
    gateway: HookGateway,
    upkeep: DetachedSync,
    ledger: SessionLedgers,
    tallies: StateTallies,
    remote: Remote,
    state_base: PathBuf,
    pool: OnceCell<Result<ClientWorktreePool, Option<String>>>,
}

impl ClientWorkspace {
    fn new(
        checkout: Checkout,
        remote: Remote,
        git_dir: Option<PathBuf>,
        state_base: PathBuf,
    ) -> Self {
        let state = remote.state.clone();
        Self {
            remote: remote.clone(),
            state_base,
            pool: OnceCell::new(),
            cache: StateCache::new(state.clone(), checkout.root()),
            upkeep: DetachedSync::new(state.clone(), checkout.root()),
            ledger: SessionLedgers::new(state.clone()),
            tallies: StateTallies::new(state.clone()),
            gateway: HookGateway::new(remote, checkout.clone()),
            checkout,
            git_dir,
            state,
        }
    }

    /// The pool the checkout lends or belongs to, located once: `None` as the error when it
    /// declares none, the sentence saying why when it declares one that cannot be used.
    fn located(&self) -> &Result<ClientWorktreePool, Option<String>> {
        self.pool
            .get_or_init(|| ClientWorktreePool::of(&self.checkout, &self.remote, &self.state_base))
    }

    /// The checkout's `origin`: as remembered when read from git within [`ORIGIN_FRESH`], read
    /// from git (and remembered) otherwise. Git is a process start a hook does not repeat on
    /// every call.
    fn origin(&self) -> Option<RemoteUrl> {
        let root = self.checkout.root();
        let file = self.state.file("origin");
        let now = OffsetDateTime::now_utc();
        if let Some(known) = read_json::<OriginFile>(&file)
            && known.root == root
            && is_fresh(known.at, now, ORIGIN_FRESH)
        {
            return known.origin;
        }
        let origin = Git::new(root).origin();
        let remembered = OriginFile {
            root: root.to_path_buf(),
            origin: origin.clone(),
            at: now,
        };
        if let Err(error) = write_json(&file, &remembered) {
            tracing::debug!(error = %error, "the origin could not be remembered");
        }
        origin
    }
}

/// A checkout's `origin` as git last named it.
#[derive(Serialize, Deserialize)]
struct OriginFile {
    root: PathBuf,
    origin: Option<RemoteUrl>,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

impl Workspace for ClientWorkspace {
    fn relative(&self, cwd: &Path, path: &Path) -> Option<RelPath> {
        self.checkout.relative_path(cwd, path).ok()
    }

    fn repo(&self, settings: &ProjectSettings) -> Result<RepoId, PortError> {
        let origin = self.origin();
        self.checkout
            .governing_repo(settings, origin.as_ref())
            .map(|repo| repo.id.clone())
            .map_err(|refused| PortError::Refused(sentence(&refused.to_string())))
    }

    fn git_facts(&self, cwd: &Path) -> Option<GitFacts> {
        Git::new(cwd).push_facts(head(cwd, self.git_dir.as_deref()))
    }

    fn contains_dir(&self, cwd: &Path, dir: &str) -> bool {
        let (Ok(dir), Ok(root)) = (
            std::fs::canonicalize(cwd.join(dir)),
            std::fs::canonicalize(self.checkout.root()),
        ) else {
            return false;
        };
        dir.starts_with(root)
    }

    fn branch(&self) -> Option<BranchName> {
        current_branch(self.checkout.root(), self.git_dir.as_deref())
    }

    fn cache(&self) -> &dyn LocalCache {
        &self.cache
    }

    fn gateway(&self) -> &dyn CoordinationGateway {
        &self.gateway
    }

    fn upkeep(&self) -> &dyn Upkeep {
        &self.upkeep
    }

    fn ledger(&self) -> &dyn DeliveryLedger {
        &self.ledger
    }

    fn tallies(&self) -> &dyn Tallies {
        &self.tallies
    }

    fn worktree_pool(&self) -> Option<&dyn WorktreePool> {
        self.located()
            .as_ref()
            .ok()
            .map(|pool| pool as &dyn WorktreePool)
    }

    fn worktree_pool_refused(&self) -> Option<String> {
        self.located().as_ref().err().cloned().flatten()
    }
}
