//! A work tree's git directories, read from git's own files rather than by starting git: a hook
//! asks for them on every tool call, and reading two small files costs nothing next to a process.
//!
//! A work tree's top holds a `.git` entry. In an ordinary clone it is the git directory itself, and
//! the repository's common directory is that directory. In a linked worktree it is a file naming
//! the worktree's own git directory (`gitdir: <path>`, relative to the work tree when not
//! absolute), and that directory's `commondir` file names the repository's common directory
//! (relative to the worktree's git directory when not absolute). A `.git` file whose directory has
//! no `commondir` — a submodule's, or a clone made with a separate git directory — is the git
//! directory of a work tree that is no linked worktree.
//!
//! The main work tree of a linked worktree is the directory its common directory lies in, when that
//! directory's own `.git` is the common directory and the repository is not bare; a bare repository
//! has no main work tree.

use std::fs;
use std::path::{Path, PathBuf};

use crate::checkout::lexical;
use crate::paths::same_dir;

/// The most bytes read from a `.git` file, a `commondir` file or a repository's `config`.
const MAX_READ: u64 = 64 * 1024;

/// The git directories of one work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDirs {
    work_tree: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
    linked: bool,
}

impl GitDirs {
    /// The git directories of the work tree whose top is `work_tree`; `None` when it holds no
    /// `.git` entry, or one that names no git directory.
    pub fn at(work_tree: &Path) -> Option<Self> {
        let dot_git = work_tree.join(".git");
        let meta = fs::metadata(&dot_git).ok()?;
        let git_dir = if meta.is_dir() {
            dot_git
        } else {
            let text = read_small(&dot_git)?;
            let named = text.lines().next()?.trim().strip_prefix("gitdir:")?.trim();
            if named.is_empty() {
                return None;
            }
            lexical(&work_tree.join(named))
        };
        let (common_dir, linked) = match read_small(&git_dir.join("commondir")) {
            Some(text) => {
                let named = text.lines().next()?.trim();
                if named.is_empty() {
                    return None;
                }
                (lexical(&git_dir.join(named)), true)
            }
            None => (git_dir.clone(), false),
        };
        Some(Self {
            work_tree: work_tree.to_path_buf(),
            git_dir,
            common_dir,
            linked,
        })
    }

    /// The git directories of the work tree at or above `dir`: the nearest directory holding a
    /// `.git` entry; `None` when none does, or the nearest names no git directory.
    pub fn around(dir: &Path) -> Option<Self> {
        let top = dir
            .ancestors()
            .find(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())?;
        Self::at(top)
    }

    /// The top of the work tree.
    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    /// The work tree's own git directory: `.git` in an ordinary clone, `.git/worktrees/<id>` of
    /// the repository for a linked worktree.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The directory the repository's worktrees share: its objects, refs and configuration.
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    /// Whether the work tree is a linked worktree of a repository whose main work tree is
    /// elsewhere.
    pub fn is_linked(&self) -> bool {
        self.linked
    }

    /// The main work tree of the repository: this work tree when it is no linked worktree; for a
    /// linked one, the directory its common directory lies in, when that directory's `.git` is the
    /// common directory. `None` for a bare repository, and when the common directory lies in no
    /// work tree of its own.
    pub fn main_work_tree(&self) -> Option<PathBuf> {
        if !self.linked {
            return Some(self.work_tree.clone());
        }
        if is_bare(&self.common_dir) {
            return None;
        }
        let candidate = self.common_dir.parent()?;
        let main = Self::at(candidate)?;
        (!main.linked && same_dir(&main.git_dir, &self.common_dir)).then(|| candidate.to_path_buf())
    }

    /// Whether `other` is a work tree of the same repository: the two share their common
    /// directory.
    pub fn same_repository(&self, other: &Self) -> bool {
        same_dir(&self.common_dir, &other.common_dir)
    }
}

/// Whether the repository whose common directory is `common` is bare, as its `config` says in
/// `core.bare`.
fn is_bare(common: &Path) -> bool {
    let Some(text) = read_small(&common.join("config")) else {
        return false;
    };
    let mut in_core = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_core = line
                .trim_start_matches('[')
                .split([']', ' ', '"'])
                .next()
                .is_some_and(|section| section.eq_ignore_ascii_case("core"));
            continue;
        }
        if !in_core {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("bare") {
            let value = value.split(['#', ';']).next().unwrap_or_default().trim();
            return ["true", "yes", "on", "1"]
                .iter()
                .any(|truth| value.eq_ignore_ascii_case(truth));
        }
    }
    false
}

/// The text of a small file, `None` when it cannot be read, is a symbolic link, is not text or is
/// too long.
fn read_small(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_READ {
        return None;
    }
    fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// A main work tree at `<dir>/main` and its linked worktree `second` at `<dir>/second`, as
    /// `git worktree add` lays them out.
    fn linked(dir: &Path, gitdir_line: &str, commondir_line: &str) -> (PathBuf, PathBuf) {
        let main = dir.join("main");
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        let admin = main.join(".git/worktrees/second");
        write(&admin.join("HEAD"), "ref: refs/heads/topic\n");
        write(&admin.join("commondir"), commondir_line);
        let second = dir.join("second");
        write(&second.join(".git"), gitdir_line);
        (main, second)
    }

    #[test]
    fn an_ordinary_clone_is_its_own_main_work_tree() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        write(&main.join(".git/HEAD"), "ref: refs/heads/main\n");
        fs::create_dir_all(main.join("src/deep")).unwrap();
        let dirs = GitDirs::around(&main.join("src/deep")).unwrap();
        assert_eq!(dirs.work_tree(), main);
        assert_eq!(dirs.git_dir(), main.join(".git"));
        assert_eq!(dirs.common_dir(), main.join(".git"));
        assert!(!dirs.is_linked());
        assert_eq!(dirs.main_work_tree(), Some(main));
    }

    #[test]
    fn a_linked_worktree_leads_to_its_main_work_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let admin = root.join("main/.git/worktrees/second");
        let (main, second) = linked(&root, &format!("gitdir: {}\n", admin.display()), "../..\n");
        let dirs = GitDirs::at(&second).unwrap();
        assert!(dirs.is_linked());
        assert_eq!(dirs.git_dir(), admin);
        assert_eq!(dirs.common_dir(), main.join(".git"));
        assert_eq!(dirs.main_work_tree(), Some(main.clone()));
        assert!(dirs.same_repository(&GitDirs::at(&main).unwrap()));
    }

    #[test]
    fn relative_gitdir_and_commondir_lines_are_read_from_their_own_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let (main, second) = linked(&root, "gitdir: ../main/.git/worktrees/second\n", "../..\n");
        let dirs = GitDirs::at(&second).unwrap();
        assert_eq!(dirs.git_dir(), root.join("main/.git/worktrees/second"));
        assert_eq!(dirs.common_dir(), main.join(".git"));
        assert_eq!(dirs.main_work_tree(), Some(main));
    }

    #[test]
    fn a_submodules_git_file_is_a_work_tree_of_its_own_and_no_linked_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let outer = root.join("outer");
        write(&outer.join(".git/HEAD"), "ref: refs/heads/main\n");
        write(
            &outer.join(".git/modules/lib/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        );
        let lib = outer.join("lib");
        write(&lib.join(".git"), "gitdir: ../.git/modules/lib\n");
        let dirs = GitDirs::at(&lib).unwrap();
        assert!(!dirs.is_linked());
        assert_eq!(dirs.common_dir(), outer.join(".git/modules/lib"));
        assert_eq!(dirs.main_work_tree(), Some(lib), "never the outer checkout");
    }

    #[test]
    fn a_bare_common_directory_has_no_main_work_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let bare = root.join("widgets.git");
        write(&bare.join("HEAD"), "ref: refs/heads/main\n");
        write(
            &bare.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = true\n",
        );
        let admin = bare.join("worktrees/second");
        write(&admin.join("commondir"), "../..\n");
        let second = root.join("second");
        write(
            &second.join(".git"),
            &format!("gitdir: {}\n", admin.display()),
        );
        let dirs = GitDirs::at(&second).unwrap();
        assert!(dirs.is_linked());
        assert_eq!(dirs.main_work_tree(), None);

        let (_, linked_to_bare_dot_git) = linked(
            &root.join("x"),
            "gitdir: ../main/.git/worktrees/second\n",
            "../..\n",
        );
        write(
            &root.join("x/main/.git/config"),
            "[core]\n\tbare = TRUE ; a clone made with --bare into .git\n",
        );
        assert_eq!(
            GitDirs::at(&linked_to_bare_dot_git)
                .unwrap()
                .main_work_tree(),
            None
        );
    }

    #[test]
    fn a_common_directory_in_no_work_tree_of_its_own_has_no_main_work_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let separate = root.join("store/widgets");
        write(&separate.join("HEAD"), "ref: refs/heads/main\n");
        let admin = separate.join("worktrees/second");
        write(&admin.join("commondir"), "../..\n");
        let second = root.join("second");
        write(
            &second.join(".git"),
            &format!("gitdir: {}\n", admin.display()),
        );
        assert_eq!(GitDirs::at(&second).unwrap().main_work_tree(), None);
    }

    #[test]
    fn no_git_entry_or_an_unreadable_one_names_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(GitDirs::at(dir.path()), None);
        write(&dir.path().join("plain/.git"), "not a gitdir line\n");
        assert_eq!(GitDirs::at(&dir.path().join("plain")), None);
        write(&dir.path().join("empty/.git"), "gitdir:\n");
        assert_eq!(GitDirs::at(&dir.path().join("empty")), None);
    }
}
