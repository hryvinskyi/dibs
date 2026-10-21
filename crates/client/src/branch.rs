//! The branch a checkout is on, read from git's own files rather than by starting git: a hook asks
//! for it before the edits it checks, and reading one small file costs nothing next to a process.
//!
//! The checkout's git directory is found as git finds it: the one `GIT_DIR` names when it is set,
//! else, from the directory upwards, the first that has a `.git` — a directory in an ordinary
//! clone, a file naming one (`gitdir: <path>`) in a linked worktree or a submodule — or is a git
//! directory itself, as a bare repository and the inside of `.git` are. Its `HEAD` names the branch
//! (`ref: refs/heads/<name>`), or holds a commit when the head is detached, which is on no branch.
//! A `HEAD` that is a symbolic link is not read: git follows it, so only git can say what it
//! names. A repository that keeps its refs in a reftable has a `HEAD` naming
//! `refs/heads/.invalid`, which is no branch name, so it reports none.

use std::fs;
use std::path::{Path, PathBuf};

use dibs_domain::radar::BranchName;

/// The most bytes read from `HEAD` or from a `.git` file; either is one short line.
const MAX_READ: u64 = 4096;

/// The branch the checkout at or above `dir` is on: the name its `HEAD` file gives after
/// `refs/heads/`; `None` on a detached head, outside a git checkout, for a `HEAD` that names no
/// branch by name (a reftable's, a symbolic link), or when its files cannot be read. `git_dir` is
/// the environment's `GIT_DIR`, when set; a relative one is taken from `dir`.
pub fn current_branch(dir: &Path, git_dir: Option<&Path>) -> Option<BranchName> {
    branch_of_head(&head_text(dir, git_dir)?)
}

/// What the `HEAD` of a checkout says, as far as its file tells it for certain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// It names this branch.
    Branch(BranchName),
    /// It holds a commit: the head is detached, on no branch.
    Detached,
    /// The file cannot be read, or says something else: a ref outside `refs/heads/`, a name that
    /// is no branch name (as a reftable's `HEAD` has). Only git itself can tell.
    Unknown,
}

/// What the `HEAD` of the checkout at or above `dir` says, read as [`current_branch`] reads it.
pub fn head(dir: &Path, git_dir: Option<&Path>) -> Head {
    let Some(text) = head_text(dir, git_dir) else {
        return Head::Unknown;
    };
    if let Some(branch) = branch_of_head(&text) {
        return Head::Branch(branch);
    }
    let commit = text.trim();
    let hex = commit.chars().all(|c| c.is_ascii_hexdigit());
    if hex && matches!(commit.len(), 40 | 64) {
        Head::Detached
    } else {
        Head::Unknown
    }
}

/// The text of the `HEAD` of the checkout at or above `dir`, `git_dir` being the environment's
/// `GIT_DIR` when set.
fn head_text(dir: &Path, git_dir: Option<&Path>) -> Option<String> {
    let git_dir = match git_dir {
        Some(named) if named.is_absolute() => named.to_path_buf(),
        Some(named) => dir.join(named),
        None => self::git_dir(dir)?,
    };
    read_small(&git_dir.join("HEAD"))
}

/// The branch a `HEAD` file's text names; `None` for a detached head or a ref outside
/// `refs/heads/`.
pub fn branch_of_head(text: &str) -> Option<BranchName> {
    let target = text.trim().strip_prefix("ref:")?.trim();
    let name = target.strip_prefix("refs/heads/")?;
    BranchName::parse(name).ok()
}

/// The git directory of the checkout at or above `dir`.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    for ancestor in dir.ancestors() {
        let dot_git = ancestor.join(".git");
        let Ok(meta) = fs::metadata(&dot_git) else {
            if is_git_dir(ancestor) {
                return Some(ancestor.to_path_buf());
            }
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git);
        }
        let text = read_small(&dot_git)?;
        let named = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        return Some(if named.is_absolute() {
            named
        } else {
            ancestor.join(named)
        });
    }
    None
}

/// Whether `dir` is a git directory itself: it has a `HEAD`, an `objects` directory and a `refs`
/// directory, as git's own test asks.
fn is_git_dir(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join("HEAD")).is_ok()
        && dir.join("objects").is_dir()
        && dir.join("refs").is_dir()
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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    #[test]
    fn a_head_names_its_branch_and_a_detached_one_none() {
        assert_eq!(
            branch_of_head("ref: refs/heads/bob/42-refunds\n").map(|b| b.to_string()),
            Some("bob/42-refunds".to_owned())
        );
        assert_eq!(
            branch_of_head("ref:refs/heads/main").map(|b| b.to_string()),
            Some("main".to_owned())
        );
        assert_eq!(
            branch_of_head("4f2a91c0e2d1b7a9f00b3c4d5e6f708192a3b4c5\n"),
            None
        );
        assert_eq!(branch_of_head("ref: refs/remotes/origin/main\n"), None);
        assert_eq!(branch_of_head("ref: refs/heads/bad name\n"), None);
        assert_eq!(branch_of_head(""), None);
    }

    #[test]
    fn a_head_is_a_branch_a_detached_commit_or_left_to_git() {
        let root = tempfile::tempdir().unwrap();
        let said = |text: &str| {
            write(&root.path().join(".git/HEAD"), text);
            head(root.path(), None)
        };
        assert_eq!(
            said("ref: refs/heads/release/2026.10.3\n"),
            Head::Branch(BranchName::parse("release/2026.10.3").unwrap())
        );
        assert_eq!(
            said("4f2a91c0e2d1b7a9f00b3c4d5e6f708192a3b4c5\n"),
            Head::Detached
        );
        assert_eq!(said(&format!("{}\n", "a".repeat(64))), Head::Detached);
        for text in [
            "ref: refs/heads/.invalid\n",
            "ref: refs/remotes/origin/main\n",
            "4f2a91c\n",
            "",
        ] {
            assert_eq!(said(text), Head::Unknown, "{text:?}");
        }
        let outside = tempfile::tempdir().unwrap();
        assert_eq!(head(outside.path(), None), Head::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn a_head_that_is_a_symbolic_link_is_left_to_git() {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join("elsewhere"), "ref: refs/heads/main\n");
        fs::create_dir_all(root.path().join(".git")).unwrap();
        std::os::unix::fs::symlink(root.path().join("elsewhere"), root.path().join(".git/HEAD"))
            .unwrap();
        assert_eq!(head(root.path(), None), Head::Unknown);
        assert_eq!(current_branch(root.path(), None), None);
    }

    #[test]
    fn a_bare_repository_or_the_inside_of_a_git_directory_is_its_own_git_directory() {
        let outer = tempfile::tempdir().unwrap();
        write(&outer.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        let bare = outer.path().join("mirror.git");
        write(&bare.join("HEAD"), "ref: refs/heads/release/2026.10.3\n");
        fs::create_dir_all(bare.join("objects")).unwrap();
        fs::create_dir_all(bare.join("refs")).unwrap();
        assert_eq!(
            head(&bare, None),
            Head::Branch(BranchName::parse("release/2026.10.3").unwrap()),
            "not the HEAD of the checkout around it"
        );
        let inside = outer.path().join(".git");
        fs::create_dir_all(inside.join("objects")).unwrap();
        fs::create_dir_all(inside.join("refs")).unwrap();
        assert_eq!(
            head(&inside.join("refs"), None),
            Head::Branch(BranchName::parse("main").unwrap())
        );
    }

    #[test]
    fn an_ordinary_clone_is_read_from_its_git_directory_from_any_directory_below_it() {
        let root = tempfile::tempdir().unwrap();
        write(
            &root.path().join(".git/HEAD"),
            "ref: refs/heads/alice/7-rates\n",
        );
        let deep = root.path().join("packages/billing/src");
        fs::create_dir_all(&deep).unwrap();
        for dir in [root.path(), deep.as_path()] {
            assert_eq!(
                current_branch(dir, None).map(|b| b.to_string()),
                Some("alice/7-rates".to_owned())
            );
        }
        write(
            &root.path().join(".git/HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        );
        assert_eq!(current_branch(root.path(), None), None, "a detached head");
    }

    #[test]
    fn a_linked_worktree_is_read_from_the_git_directory_its_git_file_names() {
        let main = tempfile::tempdir().unwrap();
        write(&main.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        let worktree_git = main.path().join(".git/worktrees/second");
        write(&worktree_git.join("HEAD"), "ref: refs/heads/bob/9-search\n");
        let linked = tempfile::tempdir().unwrap();
        write(
            &linked.path().join(".git"),
            &format!("gitdir: {}\n", worktree_git.display()),
        );
        assert_eq!(
            current_branch(linked.path(), None).map(|b| b.to_string()),
            Some("bob/9-search".to_owned())
        );
        let relative = tempfile::tempdir().unwrap();
        write(
            &relative.path().join("modules/lib/HEAD"),
            "ref: refs/heads/topic\n",
        );
        write(
            &relative.path().join("checkout/.git"),
            "gitdir: ../modules/lib\n",
        );
        assert_eq!(
            current_branch(&relative.path().join("checkout"), None).map(|b| b.to_string()),
            Some("topic".to_owned()),
            "a relative gitdir is read from the directory of the file"
        );
    }

    #[test]
    fn a_git_dir_from_the_environment_wins_and_a_reftable_head_names_no_branch() {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join(".git/HEAD"), "ref: refs/heads/main\n");
        let elsewhere = tempfile::tempdir().unwrap();
        write(
            &elsewhere.path().join("HEAD"),
            "ref: refs/heads/carol/8-export\n",
        );
        assert_eq!(
            current_branch(root.path(), Some(elsewhere.path())).map(|b| b.to_string()),
            Some("carol/8-export".to_owned())
        );
        write(&root.path().join("custom/HEAD"), "ref: refs/heads/topic\n");
        assert_eq!(
            current_branch(root.path(), Some(Path::new("custom"))).map(|b| b.to_string()),
            Some("topic".to_owned()),
            "a relative GIT_DIR is taken from the directory"
        );
        write(&root.path().join("big/HEAD"), &"x".repeat(5000));
        assert_eq!(current_branch(root.path(), Some(Path::new("big"))), None);
        let not_file = root.path().join("dir");
        fs::create_dir_all(not_file.join("HEAD")).unwrap();
        assert_eq!(current_branch(root.path(), Some(&not_file)), None);
        write(&root.path().join(".git/HEAD"), "ref: refs/heads/.invalid\n");
        assert_eq!(
            current_branch(root.path(), None),
            None,
            "a reftable repository"
        );
    }

    #[test]
    fn outside_a_checkout_or_with_unreadable_files_there_is_no_branch() {
        let bare = tempfile::tempdir().unwrap();
        let inside = bare.path().join("no-git-here");
        fs::create_dir_all(&inside).unwrap();
        write(&inside.join(".git"), "not a gitdir line\n");
        assert_eq!(current_branch(&inside, None), None);
        let huge = tempfile::tempdir().unwrap();
        write(&huge.path().join(".git/HEAD"), &"x".repeat(5000));
        assert_eq!(current_branch(huge.path(), None), None);
    }
}
