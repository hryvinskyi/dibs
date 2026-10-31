//! A checkout's configuration: the `.dibs.toml` nearest to a directory, the repository of the
//! project it governs, and paths turned into the repository-relative form the server reads.
//!
//! The nearest file wins, and the search never leaves the git repository it starts in: it stops at
//! the first directory holding a `.git` entry (the directory of a clone, or the file of a worktree
//! or a submodule). A checkout nested inside another (a clone the outer one ignores) therefore
//! answers for its own repository, and when it has no file of its own it is governed by none —
//! never by the outer checkout's. A file names the server, the project and, in a project with
//! several repositories, which one this checkout is; that repository's remote must be the
//! checkout's `origin`, so a file copied into another clone governs nothing.
//!
//! A file may also hold one of two tables about the pool of worktrees a checkout lends to its
//! agents, never both: `[worktrees]`, in the checkout that lends them, says where they live, how
//! many there are and which commands of the project prepare, reset and stop one; `[pool_worktree]`,
//! in the file dibs writes into each worktree of a pool, names the checkout that lends it and the
//! worktree's name, so a worktree is a governed checkout of its own and never a pool of its own.
//! Parsing a file reads either table from it as written; neither may be honoured from a file git
//! tracks or does not ignore. The `[worktrees]` table names commands that run on the machine of
//! whoever reads it, and `[pool_worktree]` names the checkout whose commands those are: from a
//! committed file they would run on every member's machine that checks the branch out.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::Duration;

use dibs_domain::paths::RelPath;
use dibs_domain::project::ProjectSlug;
use dibs_domain::repo::{RemoteUrl, RepoId};
use dibs_domain::settings::{CheckoutRefused, ProjectSettings, RepoSpec, parse_duration};
use dibs_domain::worktree::{PoolId, PoolIdError, WorktreeName};

use crate::api::Me;
use crate::server_url::ServerUrl;

/// The name of a checkout's configuration file.
pub const FILE_NAME: &str = ".dibs.toml";

/// What a `.dibs.toml` says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutConfig {
    /// The server.
    pub server: ServerUrl,
    /// The project.
    pub project: ProjectSlug,
    /// Which of the project's repositories this checkout is; absent for a project with one.
    pub repo: Option<RepoId>,
    /// The pool of worktrees this checkout lends, when its `[worktrees]` table declares one.
    pub worktrees: Option<WorktreeSettings>,
    /// The checkout this one is a pool worktree of, when its `[pool_worktree]` table says so.
    pub pool_worktree: Option<PoolWorktree>,
}

/// The pool of worktrees a checkout lends: `<prefix>1` to `<prefix><count>` under `root`, and the
/// project's commands that prepare a missing one, reset a reused one and stop the environment of
/// one left idle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSettings {
    /// The directory the worktrees live in, absolute: by default beside the checkout,
    /// `<checkout>-worktrees`.
    pub root: PathBuf,
    /// How many worktrees there are, 1 to [`WorktreeSettings::MAX_COUNT`].
    pub count: u8,
    /// What each worktree's name starts with; `worktree` by default.
    pub prefix: String,
    /// What a worktree is reset to; `origin/main` by default.
    pub base: String,
    /// The command that makes a worktree missing on disk ready to work in.
    pub prepare: String,
    /// The command that brings a reused worktree back to the base.
    pub reset: String,
    /// The command that stops the environment of a worktree left free, when the project has one.
    pub idle: Option<String>,
    /// How long a worktree stays free before its environment is stopped; 30 minutes by default.
    pub idle_after: Duration,
}

impl WorktreeSettings {
    /// The most worktrees one pool has.
    pub const MAX_COUNT: u8 = 32;

    /// The names of the worktrees, in order: `<prefix>1` to `<prefix><count>`.
    pub fn names(&self) -> Vec<WorktreeName> {
        (1..=self.count)
            .filter_map(|n| WorktreeName::parse(&format!("{}{n}", self.prefix)).ok())
            .collect()
    }

    /// The directory of the worktree `name`.
    pub fn path_of(&self, name: &WorktreeName) -> PathBuf {
        self.root.join(name.as_str())
    }
}

/// What the file of a pool worktree says about it: the checkout that lends it and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolWorktree {
    /// The checkout whose pool the worktree belongs to, absolute.
    pub checkout: PathBuf,
    /// The worktree's name in that pool.
    pub name: WorktreeName,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    server: String,
    project: String,
    repo: Option<String>,
    worktrees: Option<WorktreesTable>,
    pool_worktree: Option<PoolWorktreeTable>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreesTable {
    root: Option<String>,
    count: Option<i64>,
    prefix: Option<String>,
    base: Option<String>,
    prepare: Option<String>,
    reset: Option<String>,
    idle: Option<String>,
    idle_after: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoolWorktreeTable {
    checkout: String,
    name: String,
}

/// A checkout: the directory holding its `.dibs.toml`, and what the file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    root: PathBuf,
    config: CheckoutConfig,
}

/// Why a checkout's configuration cannot be used.
#[derive(Debug, Error)]
pub enum CheckoutError {
    /// The file exists and cannot be read.
    #[error("{path} cannot be read")]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// The file is not valid.
    #[error("{path} is not valid: {reason}")]
    Invalid {
        /// The file.
        path: PathBuf,
        /// The rule broken.
        reason: String,
    },
}

/// Why a path cannot be checked in this checkout.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PathRefused {
    /// The path lies outside the checkout.
    #[error("{path} is outside the checkout at {root}")]
    Outside {
        /// The path as given.
        path: String,
        /// The checkout's root.
        root: String,
    },
    /// The path is the checkout's root itself or breaks the rules of a repository path.
    #[error("{path} is not a file path inside the checkout: {reason}")]
    Invalid {
        /// The path as given.
        path: String,
        /// The rule broken.
        reason: String,
    },
}

/// Why a checkout is governed by no repository of its project's settings.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GovernRefused {
    /// The project lists repositories and the file names none.
    #[error(
        "{file} names no repo, and this project has several; add repo = \"<id>\" naming one of: {known}"
    )]
    RepoRequired {
        /// The configuration file.
        file: String,
        /// The repository ids the project lists.
        known: String,
    },
    /// The file names a repository the project does not list.
    #[error("{file} names the repo '{repo}', which this project does not list; it lists: {known}")]
    UnknownRepo {
        /// The configuration file.
        file: String,
        /// The id named.
        repo: RepoId,
        /// The ids listed.
        known: String,
    },
    /// The checkout has no `origin` to compare with the repository's remote.
    #[error(
        "{file} says this checkout is '{repo}' ({expected}), but the checkout has no origin remote to confirm it"
    )]
    NoOrigin {
        /// The configuration file.
        file: String,
        /// The id named.
        repo: RepoId,
        /// The repository's remote.
        expected: RemoteUrl,
    },
    /// The checkout's `origin` is another repository.
    #[error(
        "{file} says this checkout is '{repo}' ({expected}), but its origin is {origin}; the file belongs to another clone"
    )]
    RemoteMismatch {
        /// The configuration file.
        file: String,
        /// The id named.
        repo: RepoId,
        /// The repository's remote.
        expected: RemoteUrl,
        /// The checkout's origin.
        origin: RemoteUrl,
    },
}

/// The token belongs to another project than the checkout names.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "this checkout belongs to the project '{configured}', but the token signs in to the project '{signed_in}'; sign in to '{configured}' with dibs login"
)]
pub struct ProjectMismatch {
    /// The project the checkout names.
    pub configured: ProjectSlug,
    /// The project the token belongs to.
    pub signed_in: ProjectSlug,
}

impl Checkout {
    /// The checkout whose `.dibs.toml` is nearest to `start`: in `start` itself or the closest
    /// directory above it within the same git repository. The search stops at the first directory
    /// holding a `.git` entry, whether a directory or a file; `None` when no directory up to there
    /// (or up to the root, outside any repository) holds a configuration file.
    ///
    /// # Errors
    /// [`CheckoutError`] when the nearest file cannot be read or is not valid.
    pub fn discover(start: &Path) -> Result<Option<Self>, CheckoutError> {
        for directory in start.ancestors() {
            let path = directory.join(FILE_NAME);
            if !path.is_file() {
                if directory.join(".git").exists() {
                    return Ok(None);
                }
                continue;
            }
            let text = fs::read_to_string(&path).map_err(|source| CheckoutError::Unreadable {
                path: path.clone(),
                source,
            })?;
            let config = Self::parse(&path, &text)?;
            return Ok(Some(Self {
                root: directory.to_path_buf(),
                config,
            }));
        }
        Ok(None)
    }

    /// A checkout at `root` with this configuration.
    pub fn new(root: PathBuf, config: CheckoutConfig) -> Self {
        Self { root, config }
    }

    /// Parses the text of the configuration file at `path`.
    ///
    /// # Errors
    /// [`CheckoutError::Invalid`] naming the file and the first rule broken.
    pub fn parse(path: &Path, text: &str) -> Result<CheckoutConfig, CheckoutError> {
        let invalid = |reason: String| CheckoutError::Invalid {
            path: path.to_path_buf(),
            reason,
        };
        let file: ConfigFile =
            toml::from_str(text).map_err(|error| invalid(error.message().to_owned()))?;
        let server = ServerUrl::parse(&file.server).map_err(|error| invalid(error.to_string()))?;
        let project = ProjectSlug::parse(&file.project)
            .map_err(|error| invalid(format!("project: {error}")))?;
        let repo = file
            .repo
            .as_deref()
            .map(RepoId::parse)
            .transpose()
            .map_err(|error| invalid(format!("repo: {error}")))?;
        if file.worktrees.is_some() && file.pool_worktree.is_some() {
            return Err(invalid(
                "a file holds either a [worktrees] table or a [pool_worktree] table, never both"
                    .to_owned(),
            ));
        }
        let directory = path.parent().unwrap_or_else(|| Path::new(""));
        let worktrees = file
            .worktrees
            .map(|table| worktree_settings(directory, table))
            .transpose()
            .map_err(invalid)?;
        let pool_worktree = file
            .pool_worktree
            .map(pool_worktree)
            .transpose()
            .map_err(invalid)?;
        Ok(CheckoutConfig {
            server,
            project,
            repo,
            worktrees,
            pool_worktree,
        })
    }

    /// The directory holding the configuration file.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The configuration file.
    pub fn file(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// What the file says.
    pub fn config(&self) -> &CheckoutConfig {
        &self.config
    }

    /// Refuses a token of another project than the file names.
    ///
    /// # Errors
    /// [`ProjectMismatch`] naming both projects.
    pub fn verify_project(&self, me: &Me) -> Result<(), ProjectMismatch> {
        verify_project(&self.config.project, me)
    }

    /// The repository of the settings this checkout is, its remote confirmed against the
    /// checkout's `origin` (`None` when it has none). The one repository of a project that lists
    /// none has no remote to confirm.
    ///
    /// # Errors
    /// [`GovernRefused`] naming the file and what does not match.
    pub fn governing_repo<'s>(
        &self,
        settings: &'s ProjectSettings,
        origin: Option<&RemoteUrl>,
    ) -> Result<&'s RepoSpec, GovernRefused> {
        let file = self.file().display().to_string();
        let known = || {
            settings
                .repos()
                .iter()
                .map(|repo| repo.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let repo = settings
            .checkout_repo(self.config.repo.as_ref())
            .map_err(|refused| match refused {
                CheckoutRefused::RepoRequired => GovernRefused::RepoRequired {
                    file: file.clone(),
                    known: known(),
                },
                CheckoutRefused::UnknownRepo { repo } => GovernRefused::UnknownRepo {
                    file: file.clone(),
                    repo,
                    known: known(),
                },
            })?;
        let Some(expected) = &repo.remote else {
            return Ok(repo);
        };
        match origin {
            Some(origin) if origin == expected => Ok(repo),
            Some(origin) => Err(GovernRefused::RemoteMismatch {
                file,
                repo: repo.id.clone(),
                expected: expected.clone(),
                origin: origin.clone(),
            }),
            None => Err(GovernRefused::NoOrigin {
                file,
                repo: repo.id.clone(),
                expected: expected.clone(),
            }),
        }
    }

    /// A path given relative to `cwd`, or absolute, as a path relative to the checkout's root.
    /// The two are compared by their components after `.` and `..` are resolved, so either
    /// separator works; on Windows the drive and the directories down to the root compare without
    /// regard to case, the path inside the checkout with it. When the spelling differs from the
    /// root's (a link in one of them), both are compared as the file system resolves them.
    ///
    /// # Errors
    /// [`PathRefused`] when the path lies outside the checkout or is not a file path.
    pub fn relative_path(&self, cwd: &Path, path: &Path) -> Result<RelPath, PathRefused> {
        let shown = path.display().to_string();
        let absolute = lexical(&cwd.join(path));
        let inside = strip_root(&lexical(&self.root), &absolute).or_else(|| {
            let root = fs::canonicalize(&self.root).ok()?;
            strip_root(&root, &resolve_existing(&absolute)?)
        });
        let Some(segments) = inside else {
            return Err(PathRefused::Outside {
                path: shown,
                root: self.root.display().to_string(),
            });
        };
        RelPath::parse(&segments.join("/")).map_err(|error| PathRefused::Invalid {
            path: shown,
            reason: error.to_string(),
        })
    }
}

/// The `[worktrees]` table of the file in `directory`, its defaults filled in.
fn worktree_settings(directory: &Path, table: WorktreesTable) -> Result<WorktreeSettings, String> {
    let count = table
        .count
        .ok_or("worktrees: count is required: how many worktrees the pool has")?;
    let count = u8::try_from(count)
        .ok()
        .filter(|count| (1..=WorktreeSettings::MAX_COUNT).contains(count))
        .ok_or_else(|| format!("worktrees: count is 1 to {}", WorktreeSettings::MAX_COUNT))?;
    let prefix = table.prefix.unwrap_or_else(|| "worktree".to_owned());
    if WorktreeName::parse(&format!("{prefix}{count}")).is_err() {
        return Err(format!(
            "worktrees: prefix '{prefix}' does not make worktree names: a name is 1 to 64 lowercase \
             letters, digits, '.', '_' or '-', starting with a letter or a digit"
        ));
    }
    let root = match table.root {
        Some(root) if root.trim().is_empty() => {
            return Err("worktrees: root is a directory".to_owned());
        }
        Some(root) => lexical(&directory.join(root)),
        None => default_root(directory)?,
    };
    if root.starts_with(lexical(directory)) {
        return Err(format!(
            "worktrees: root {} is the checkout or lies inside it; the worktrees live outside it",
            root.display()
        ));
    }
    let base = table.base.unwrap_or_else(|| "origin/main".to_owned());
    if base.is_empty() || base.starts_with('-') || base.chars().any(char::is_whitespace) {
        return Err(format!(
            "worktrees: base '{base}' is no ref a worktree can be reset to"
        ));
    }
    let command = |name: &str, text: Option<String>| -> Result<Option<String>, String> {
        match text {
            Some(text) if text.trim().is_empty() => {
                Err(format!("worktrees: {name} is a command, not empty"))
            }
            other => Ok(other),
        }
    };
    let prepare = command("prepare", table.prepare)?.ok_or(
        "worktrees: prepare is required: the command that makes a worktree ready to work in",
    )?;
    let reset = command("reset", table.reset)?.ok_or(
        "worktrees: reset is required: the command that brings a reused worktree back to the base",
    )?;
    let idle = command("idle", table.idle)?;
    let idle_after = match table.idle_after {
        Some(text) => parse_duration(&text)
            .ok()
            .filter(|duration| duration.is_positive())
            .ok_or_else(|| format!("worktrees: idle_after '{text}' is no duration like '30m'"))?,
        None => Duration::minutes(30),
    };
    Ok(WorktreeSettings {
        root,
        count,
        prefix,
        base,
        prepare,
        reset,
        idle,
        idle_after,
    })
}

/// `<checkout>-worktrees`, beside the checkout.
fn default_root(directory: &Path) -> Result<PathBuf, String> {
    let directory = lexical(directory);
    let name = directory
        .file_name()
        .ok_or("worktrees: the checkout has no directory name to put its pool beside; set root")?;
    let mut sibling = name.to_os_string();
    sibling.push("-worktrees");
    Ok(directory.with_file_name(sibling))
}

/// The `[pool_worktree]` table.
fn pool_worktree(table: PoolWorktreeTable) -> Result<PoolWorktree, String> {
    let checkout = PathBuf::from(&table.checkout);
    if !checkout.is_absolute() {
        return Err("pool_worktree: checkout is the absolute path of a checkout".to_owned());
    }
    let name = WorktreeName::parse(&table.name)
        .map_err(|error| format!("pool_worktree: name: {error}"))?;
    Ok(PoolWorktree { checkout, name })
}

/// The id of the pool whose worktrees live in `root`: a digest of the directory as the file
/// system resolves it, as far as it exists, so every spelling of one directory — through a link,
/// with `..` — names one pool, and two checkouts of one project on one machine name two.
///
/// # Errors
/// None in practice: the digest is always written as a valid id.
pub fn pool_id(root: &Path) -> Result<PoolId, PoolIdError> {
    let lexical_root = lexical(root);
    let resolved = resolve_existing(&lexical_root).unwrap_or(lexical_root);
    let digest = Sha256::digest(resolved.to_string_lossy().as_bytes());
    let hex: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    PoolId::parse(&hex)
}

/// Refuses a token of another project than `configured`.
///
/// # Errors
/// [`ProjectMismatch`] naming both projects.
pub fn verify_project(configured: &ProjectSlug, me: &Me) -> Result<(), ProjectMismatch> {
    if me.project.slug == *configured {
        return Ok(());
    }
    Err(ProjectMismatch {
        configured: configured.clone(),
        signed_in: me.project.slug.clone(),
    })
}

/// The path with `.` dropped and each `..` taking off the component before it.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The path resolved by the file system as far as it exists, with the rest appended as written.
fn resolve_existing(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut existing = path;
    loop {
        if let Ok(resolved) = fs::canonicalize(existing) {
            let mut out = resolved;
            out.extend(rest.iter().rev());
            return Some(out);
        }
        rest.push(existing.file_name()?.to_owned());
        existing = existing.parent()?;
    }
}

/// The components of `path` below `root`, as text, when `path` lies under it.
fn strip_root(root: &Path, path: &Path) -> Option<Vec<String>> {
    let mut root_parts = root.components();
    let mut path_parts = path.components();
    for expected in root_parts.by_ref() {
        let actual = path_parts.next()?;
        if !same_root_component(expected, actual) {
            return None;
        }
    }
    path_parts
        .map(|component| match component {
            Component::Normal(part) => part.to_str().map(str::to_owned),
            _ => None,
        })
        .collect()
}

#[cfg(windows)]
fn same_root_component(a: Component<'_>, b: Component<'_>) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .to_lowercase()
        .replace('/', "\\")
        == b.as_os_str()
            .to_string_lossy()
            .to_lowercase()
            .replace('/', "\\")
}

#[cfg(not(windows))]
fn same_root_component(a: Component<'_>, b: Component<'_>) -> bool {
    a == b
}

#[cfg(test)]
mod tests {
    use dibs_domain::hot_rule::HotRuleRegistry;
    use dibs_domain::settings::SettingsDocument;

    use super::*;
    use crate::api::{Member, ProjectRef};

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    const API: &str = "server = \"https://dibs.example.com\"\nproject = \"acme\"\nrepo = \"api\"\n";

    #[test]
    fn the_nearest_file_wins_for_a_nested_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("work");
        let inner = outer.join("vendor").join("web");
        write(&outer.join(FILE_NAME), API);
        write(
            &inner.join(FILE_NAME),
            "server = \"https://dibs.example.com/\"\nproject = \"acme\"\nrepo = \"web\"\n",
        );
        fs::create_dir_all(inner.join("src/deep")).unwrap();
        fs::create_dir_all(outer.join("src")).unwrap();

        let found = Checkout::discover(&inner.join("src/deep"))
            .unwrap()
            .unwrap();
        assert_eq!(found.root(), inner);
        assert_eq!(found.config().repo.as_ref().unwrap().as_str(), "web");
        assert_eq!(found.config().server.as_str(), "https://dibs.example.com");

        let outer_found = Checkout::discover(&outer.join("src")).unwrap().unwrap();
        assert_eq!(outer_found.root(), outer);
        assert_eq!(outer_found.config().repo.as_ref().unwrap().as_str(), "api");
    }

    #[test]
    fn the_search_stops_at_the_boundary_of_a_git_repository() {
        let dir = tempfile::tempdir().unwrap();
        let outer = dir.path().join("work");
        write(&outer.join(FILE_NAME), API);
        fs::create_dir_all(outer.join(".git")).unwrap();
        fs::create_dir_all(outer.join("src/deep")).unwrap();

        let plain = Checkout::discover(&outer.join("src/deep"))
            .unwrap()
            .unwrap();
        assert_eq!(plain.root(), outer);

        let bare_clone = outer.join("vendor").join("search");
        fs::create_dir_all(bare_clone.join(".git")).unwrap();
        fs::create_dir_all(bare_clone.join("lib")).unwrap();
        assert!(
            Checkout::discover(&bare_clone.join("lib"))
                .unwrap()
                .is_none()
        );
        assert!(Checkout::discover(&bare_clone).unwrap().is_none());

        let worktree = outer.join("worktrees").join("feature");
        write(
            &worktree.join(".git"),
            "gitdir: ../../.git/worktrees/feature\n",
        );
        fs::create_dir_all(worktree.join("src")).unwrap();
        assert!(Checkout::discover(&worktree.join("src")).unwrap().is_none());

        let configured = outer.join("vendor").join("web");
        fs::create_dir_all(configured.join(".git")).unwrap();
        write(
            &configured.join(FILE_NAME),
            "server = \"https://dibs.example.com\"\nproject = \"acme\"\nrepo = \"web\"\n",
        );
        fs::create_dir_all(configured.join("app")).unwrap();
        let own = Checkout::discover(&configured.join("app"))
            .unwrap()
            .unwrap();
        assert_eq!(own.root(), configured);
        assert_eq!(own.config().repo.as_ref().unwrap().as_str(), "web");
    }

    #[test]
    fn no_file_up_to_the_root_is_no_checkout() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Checkout::discover(dir.path()).unwrap().is_none());
    }

    #[test]
    fn an_invalid_file_names_itself_and_the_rule() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        for (text, needle) in [
            ("server = \"https://x.example.com\"\n", "project"),
            (
                "server = \"dibs.example.com\"\nproject = \"acme\"\n",
                "not a server address",
            ),
            (
                "server = \"https://x.example.com\"\nproject = \"Acme Co\"\n",
                "project:",
            ),
            (
                "server = \"https://x.example.com\"\nproject = \"acme\"\nrepo = \"a:b\"\n",
                "repo:",
            ),
            (
                "server = \"https://x.example.com\"\nproject = \"acme\"\ntoken = \"x\"\n",
                "token",
            ),
        ] {
            write(&path, text);
            let error = Checkout::discover(dir.path()).unwrap_err().to_string();
            assert!(error.contains(FILE_NAME), "{error}");
            assert!(error.contains(needle), "{text:?}: {error}");
        }
    }

    const POOL: &str = "server = \"https://dibs.example.com\"\nproject = \"acme\"\n\n[worktrees]\ncount = 3\nprepare = \"/opt/kit/prepare.sh\"\nreset = \"/opt/kit/reset.sh\"\n";

    #[test]
    fn a_worktrees_table_fills_in_its_defaults_beside_the_checkout() {
        let path = Path::new("/home/alice/widgets/.dibs.toml");
        let config = Checkout::parse(path, POOL).unwrap();
        let pool = config.worktrees.unwrap();
        assert_eq!(pool.root, PathBuf::from("/home/alice/widgets-worktrees"));
        assert_eq!(pool.count, 3);
        assert_eq!(pool.base, "origin/main");
        assert_eq!(pool.prepare, "/opt/kit/prepare.sh");
        assert_eq!(pool.reset, "/opt/kit/reset.sh");
        assert_eq!(pool.idle, None);
        assert_eq!(pool.idle_after, Duration::minutes(30));
        let names: Vec<String> = pool.names().iter().map(ToString::to_string).collect();
        assert_eq!(names, ["worktree1", "worktree2", "worktree3"]);
        assert_eq!(
            pool.path_of(&pool.names()[1]),
            PathBuf::from("/home/alice/widgets-worktrees/worktree2")
        );
        assert!(config.pool_worktree.is_none());

        let custom = format!(
            "{POOL}root = \"../pools/widgets\"\nprefix = \"wt-\"\nbase = \"main\"\nidle = \"stop.sh\"\nidle_after = \"1h\"\n"
        );
        let pool = Checkout::parse(path, &custom).unwrap().worktrees.unwrap();
        assert_eq!(pool.root, PathBuf::from("/home/alice/pools/widgets"));
        assert_eq!(pool.names()[0].as_str(), "wt-1");
        assert_eq!(pool.base, "main");
        assert_eq!(pool.idle.as_deref(), Some("stop.sh"));
        assert_eq!(pool.idle_after, Duration::hours(1));
    }

    #[test]
    fn a_pool_worktree_table_names_its_checkout_and_its_name() {
        let text = "server = \"https://dibs.example.com\"\nproject = \"acme\"\n\n[pool_worktree]\ncheckout = \"/home/alice/widgets\"\nname = \"worktree2\"\n";
        let config = Checkout::parse(
            Path::new("/home/alice/widgets-worktrees/worktree2/.dibs.toml"),
            text,
        )
        .unwrap();
        assert_eq!(
            config.pool_worktree,
            Some(PoolWorktree {
                checkout: PathBuf::from("/home/alice/widgets"),
                name: WorktreeName::parse("worktree2").unwrap(),
            })
        );
        assert!(config.worktrees.is_none());
    }

    #[test]
    fn both_tables_in_one_file_and_unknown_keys_are_refused_naming_the_file() {
        let path = Path::new("/home/alice/widgets/.dibs.toml");
        let both = format!(
            "{POOL}\n[pool_worktree]\ncheckout = \"/home/alice/widgets\"\nname = \"worktree1\"\n"
        );
        for (text, needle) in [
            (both, "never both"),
            (format!("{POOL}size = 4\n"), "size"),
            (
                "server = \"https://dibs.example.com\"\nproject = \"acme\"\n[pool_worktree]\ncheckout = \"/a\"\nname = \"w1\"\nextra = 1\n".to_owned(),
                "extra",
            ),
            (POOL.replace("count = 3", "count = 0"), "count is 1 to 32"),
            (POOL.replace("count = 3", "count = 33"), "count is 1 to 32"),
            (POOL.replace("count = 3\n", ""), "count is required"),
            (POOL.replace("reset = \"/opt/kit/reset.sh\"\n", ""), "reset is required"),
            (format!("{POOL}prefix = \"Work Tree\"\n"), "prefix"),
            (format!("{POOL}root = \"pool\"\n"), "inside it"),
            (format!("{POOL}base = \"--force\"\n"), "base"),
            (format!("{POOL}idle_after = \"soon\"\n"), "idle_after"),
            (
                "server = \"https://dibs.example.com\"\nproject = \"acme\"\n[pool_worktree]\ncheckout = \"widgets\"\nname = \"w1\"\n".to_owned(),
                "absolute",
            ),
        ] {
            let error = Checkout::parse(path, &text).unwrap_err().to_string();
            assert!(error.contains(".dibs.toml"), "{error}");
            assert!(error.contains(needle), "{text:?}: {error}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_pool_is_named_by_its_directory_however_it_is_spelled() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("widgets-worktrees");
        let before = pool_id(&root).unwrap();
        fs::create_dir_all(&root).unwrap();
        assert_eq!(
            pool_id(&root).unwrap(),
            before,
            "made or not, one directory"
        );
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert_eq!(pool_id(&link).unwrap(), before);
        assert_eq!(
            pool_id(&dir.path().join("other/../widgets-worktrees")).unwrap(),
            before
        );
        assert_ne!(
            pool_id(&dir.path().join("gadgets-worktrees")).unwrap(),
            before
        );
        assert!(pool_id(&root).unwrap().as_str().len() <= PoolId::MAX_CHARS);
    }

    fn settings(document: serde_json::Value) -> ProjectSettings {
        let document: SettingsDocument = serde_json::from_value(document).unwrap();
        ProjectSettings::parse(&document, &HotRuleRegistry::standard()).unwrap()
    }

    fn two_repositories() -> ProjectSettings {
        settings(serde_json::json!({"repos": [
            {"id": "api", "remote": "https://example.com/acme/api.git",
             "areas": [{"name": "billing", "paths": ["src/billing/**"]}]},
            {"id": "web", "remote": "git@example.com:acme/web.git"}
        ]}))
    }

    fn checkout(repo: Option<&str>) -> Checkout {
        Checkout::new(
            PathBuf::from("/work/api"),
            CheckoutConfig {
                server: ServerUrl::parse("https://dibs.example.com").unwrap(),
                project: ProjectSlug::parse("acme").unwrap(),
                repo: repo.map(|id| RepoId::parse(id).unwrap()),
                worktrees: None,
                pool_worktree: None,
            },
        )
    }

    fn remote(text: &str) -> RemoteUrl {
        RemoteUrl::parse(text).unwrap()
    }

    #[test]
    fn the_repository_is_confirmed_by_the_checkouts_origin() {
        let settings = two_repositories();
        let api = checkout(Some("api"));
        let origin = remote("git@example.com:acme/api.git");
        assert_eq!(
            api.governing_repo(&settings, Some(&origin))
                .unwrap()
                .id
                .as_str(),
            "api"
        );
        let mismatch = api
            .governing_repo(&settings, Some(&remote("https://example.com/acme/web")))
            .unwrap_err();
        assert!(matches!(mismatch, GovernRefused::RemoteMismatch { .. }));
        let sentence = mismatch.to_string();
        assert!(sentence.contains("example.com/acme/api"), "{sentence}");
        assert!(sentence.contains("example.com/acme/web"), "{sentence}");
        assert!(matches!(
            api.governing_repo(&settings, None),
            Err(GovernRefused::NoOrigin { .. })
        ));
    }

    #[test]
    fn a_missing_or_unknown_repo_is_refused_naming_the_known_ones() {
        let settings = two_repositories();
        let missing = checkout(None).governing_repo(&settings, None).unwrap_err();
        assert!(matches!(missing, GovernRefused::RepoRequired { .. }));
        assert!(missing.to_string().contains("api, web"));
        let unknown = checkout(Some("search"))
            .governing_repo(&settings, None)
            .unwrap_err();
        assert!(matches!(unknown, GovernRefused::UnknownRepo { .. }));
    }

    #[test]
    fn a_project_with_one_repository_needs_no_repo_and_no_origin() {
        let one =
            settings(serde_json::json!({"areas": [{"name": "billing", "paths": ["billing/**"]}]}));
        assert_eq!(
            checkout(None)
                .governing_repo(&one, None)
                .unwrap()
                .id
                .as_str(),
            "default"
        );
    }

    fn me(project: &str) -> Me {
        Me {
            member: Member {
                handle: "alice".parse().unwrap(),
                display_name: "Alice".parse().unwrap(),
                external_login: None,
                role: dibs_domain::member::Role::Member,
            },
            project: ProjectRef {
                slug: ProjectSlug::parse(project).unwrap(),
            },
            repos: Vec::new(),
            settings: SettingsDocument::default(),
        }
    }

    #[test]
    fn a_token_of_another_project_is_refused_naming_both() {
        let api = checkout(Some("api"));
        assert!(api.verify_project(&me("acme")).is_ok());
        let refused = api.verify_project(&me("globex")).unwrap_err();
        let sentence = refused.to_string();
        assert!(
            sentence.contains("'acme'") && sentence.contains("'globex'"),
            "{sentence}"
        );
    }

    #[test]
    fn paths_become_relative_to_the_root_by_components() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("api");
        fs::create_dir_all(root.join("src/billing")).unwrap();
        let checkout = Checkout::new(root.clone(), checkout(Some("api")).config().clone());
        let cwd = root.join("src");
        for (given, expected) in [
            (PathBuf::from("billing/a.rs"), "src/billing/a.rs"),
            (
                PathBuf::from("./billing/../billing/new.rs"),
                "src/billing/new.rs",
            ),
            (PathBuf::from("../Cargo.toml"), "Cargo.toml"),
            (root.join("src/billing/a.rs"), "src/billing/a.rs"),
        ] {
            assert_eq!(
                checkout.relative_path(&cwd, &given).unwrap().as_str(),
                expected,
                "{given:?}"
            );
        }
        assert!(matches!(
            checkout.relative_path(&cwd, Path::new("../../elsewhere.rs")),
            Err(PathRefused::Outside { .. })
        ));
        assert!(matches!(
            checkout.relative_path(&cwd, Path::new("..")),
            Err(PathRefused::Invalid { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_path_through_a_link_to_the_checkout_is_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("api");
        fs::create_dir_all(root.join("src")).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let checkout = Checkout::new(root, checkout(Some("api")).config().clone());
        assert_eq!(
            checkout
                .relative_path(dir.path(), &link.join("src/new.rs"))
                .unwrap()
                .as_str(),
            "src/new.rs"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_and_its_directories_compare_without_case_and_either_separator() {
        let checkout = Checkout::new(
            PathBuf::from(r"C:\work\api"),
            checkout(Some("api")).config().clone(),
        );
        let cwd = PathBuf::from(r"C:\");
        for given in [
            r"C:\work\api\src\a.rs",
            "c:/work/api/src/a.rs",
            r"c:\WORK\Api\src/a.rs",
        ] {
            assert_eq!(
                checkout
                    .relative_path(&cwd, Path::new(given))
                    .unwrap()
                    .as_str(),
                "src/a.rs",
                "{given}"
            );
        }
        assert_eq!(
            checkout
                .relative_path(&cwd, Path::new(r"C:\work\api\SRC\A.rs"))
                .unwrap()
                .as_str(),
            "SRC/A.rs"
        );
        assert!(
            checkout
                .relative_path(&cwd, Path::new(r"D:\work\api\src\a.rs"))
                .is_err()
        );
    }
}
