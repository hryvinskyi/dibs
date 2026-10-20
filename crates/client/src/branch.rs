//! The branch a checkout is on, read from git's own files rather than by starting git: a hook asks
//! for it before the edits it checks, and reading one small file costs nothing next to a process.
//!
//! The checkout's git directory is found as git finds it: the one `GIT_DIR` names when it is set,
//! else the nearest `.git` at or above the directory, which is the directory itself in an ordinary
//! clone and a file naming it (`gitdir: <path>`) in a linked worktree or a submodule. Its `HEAD`
//! names the branch (`ref: refs/heads/<name>`), or holds a commit when the head is detached, which
//! is on no branch. A repository that keeps its refs in a reftable has a `HEAD` naming
//! `refs/heads/.invalid`, which is no branch name, so it reports none.

use std::fs;
use std::path::{Path, PathBuf};

use dibs_domain::radar::BranchName;

/// The most bytes read from `HEAD` or from a `.git` file; either is one short line.
const MAX_READ: u64 = 4096;

/// The branch the checkout at or above `dir` is on, as `git symbolic-ref --short HEAD` names it;
/// `None` on a detached head, outside a git checkout, or when its files cannot be read. `git_dir`
/// is the environment's `GIT_DIR`, when set; a relative one is taken from `dir`.
pub fn current_branch(dir: &Path, git_dir: Option<&Path>) -> Option<BranchName> {
    let git_dir = match git_dir {
        Some(named) if named.is_absolute() => named.to_path_buf(),
        Some(named) => dir.join(named),
        None => self::git_dir(dir)?,
    };
    let head = read_small(&git_dir.join("HEAD"))?;
    branch_of_head(&head)
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

/// The text of a small file, `None` when it cannot be read, is not text or is too long.
fn read_small(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
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
