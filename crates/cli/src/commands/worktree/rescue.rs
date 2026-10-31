//! Keeping a worktree's work before it is reused: what git says of the worktree, and the rescue
//! branch that saves whatever nothing else holds.
//!
//! A worktree in the middle of a rebase, a merge, a cherry-pick, a revert or a bisect is left as it
//! is: its state cannot be committed whole. Otherwise, when its tree has changes (untracked files
//! included, ignored ones not) or its `HEAD` is a commit no branch and no remote ref contains, a
//! new branch `dibs/rescue/<name>-<UTC time>` is made at `HEAD` — so a branch the worktree is on is
//! never moved — and the changes are committed on it whole, without the repository's hooks, as the
//! member. Its own `.dibs.toml`, which dibs writes, is never part of what is saved. A rescue
//! answers the untracked files it committed, so that a stray dump or a credential is seen before
//! the branch is pushed. dibs never pushes and never deletes a rescue branch.
//!
//! The same git also says whether a file of the worktree is tracked, and whether it is ignored.

use std::path::{Path, PathBuf};
use std::process::Command;

use time::{OffsetDateTime, UtcOffset};

use dibs_client::api::Member;
use dibs_client::checkout::FILE_NAME;
use dibs_domain::worktree::WorktreeName;

/// The prefix of every rescue branch.
pub const RESCUE_PREFIX: &str = "dibs/rescue/";

/// Why git could not tell or do what was asked of a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFailed {
    /// What it was asked to do.
    pub action: String,
    /// The worktree.
    pub dir: String,
}

impl GitFailed {
    /// What happened, as the end of a sentence: `git could not stage the work to rescue in …`.
    pub fn reason(&self) -> String {
        format!("git could not {} in {}", self.action, self.dir)
    }
}

/// Who a rescue commit is made as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    /// The name.
    pub name: String,
    /// The address.
    pub email: String,
}

impl Author {
    /// The member as a rescue commit's author: their name, and an address made of their handle
    /// that reaches nobody, for a worktree whose git knows no address of its own.
    pub fn of(member: &Member) -> Self {
        Self {
            name: member.display_name.to_string(),
            email: format!("{}@users.dibs.invalid", member.handle),
        }
    }
}

/// The git of one worktree.
#[derive(Debug, Clone)]
pub struct WorktreeGit {
    dir: PathBuf,
}

/// What a rescue saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rescue {
    /// The rescue branch.
    pub branch: String,
    /// The untracked files committed onto it, as paths of the worktree, in git's order.
    pub untracked: Vec<String>,
}

impl Rescue {
    /// How many untracked files a rescue's notice names before it only counts the rest.
    pub const NAMED: usize = 5;

    /// What a notice says of the untracked files committed, when there were any: `It committed 7
    /// untracked files: a.sql, b.env, c, d, e and 2 more; look at them before you push the
    /// branch.`
    pub fn untracked_sentence(&self) -> Option<String> {
        let count = self.untracked.len();
        if count == 0 {
            return None;
        }
        let named = self.untracked[..count.min(Self::NAMED)].join(", ");
        let more = count.saturating_sub(Self::NAMED);
        let rest = if more == 0 {
            String::new()
        } else {
            format!(" and {more} more")
        };
        let (files, them) = if count == 1 {
            ("file", "it")
        } else {
            ("files", "them")
        };
        Some(format!(
            "It committed {count} untracked {files}: {named}{rest}; look at {them} before you push the branch."
        ))
    }
}

/// The files of a git directory that say an operation is under way, and its name.
const IN_PROGRESS: [(&str, &str); 6] = [
    ("rebase-merge", "a rebase"),
    ("rebase-apply", "a rebase"),
    ("MERGE_HEAD", "a merge"),
    ("CHERRY_PICK_HEAD", "a cherry-pick"),
    ("REVERT_HEAD", "a revert"),
    ("BISECT_LOG", "a bisect"),
];

impl WorktreeGit {
    /// Git run in the worktree at `dir`.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// The operation under way in the worktree that keeps it from being committed whole, when
    /// one is.
    ///
    /// # Errors
    /// [`GitFailed`] when git cannot name the worktree's git directory.
    pub fn in_progress(&self) -> Result<Option<&'static str>, GitFailed> {
        let git_dir = self.text(
            &["rev-parse", "--absolute-git-dir"],
            "find its git directory",
        )?;
        let git_dir = PathBuf::from(git_dir.trim());
        Ok(IN_PROGRESS
            .iter()
            .find(|(file, _)| git_dir.join(file).exists())
            .map(|(_, what)| *what))
    }

    /// Whether the worktree holds work nothing else keeps: changes in its tree, or a `HEAD` that
    /// no branch and no remote ref contains.
    ///
    /// # Errors
    /// [`GitFailed`] when git cannot tell.
    pub fn needs_rescue(&self) -> Result<bool, GitFailed> {
        if self.is_dirty()? {
            return Ok(true);
        }
        let holders = self.text(
            &[
                "for-each-ref",
                "--count=1",
                "--contains",
                "HEAD",
                "--format=%(refname)",
                "refs/heads",
                "refs/remotes",
            ],
            "tell which refs contain HEAD",
        )?;
        Ok(holders.trim().is_empty())
    }

    /// Saves the worktree's work on a new rescue branch made at `HEAD`, its changes committed as
    /// `author`, and answers the branch and the untracked files committed.
    ///
    /// # Errors
    /// [`GitFailed`] when the branch or the commit cannot be made.
    pub fn rescue(
        &self,
        name: &WorktreeName,
        at: OffsetDateTime,
        author: &Author,
    ) -> Result<Rescue, GitFailed> {
        let dirty = self.is_dirty()?;
        let untracked = if dirty { self.untracked()? } else { Vec::new() };
        let branch = self.free_branch(&rescue_branch(name, at))?;
        self.text(&["switch", "-q", "-c", &branch], "make a rescue branch")?;
        if dirty {
            self.text(&["add", "-A", "--", "."], "stage the work to rescue")?;
            self.text(
                &["reset", "-q", "--", &own_file()],
                "leave its own .dibs.toml out of the rescue",
            )?;
            let message = format!("dibs: rescued from {name} before reuse");
            self.text(
                &[
                    "-c",
                    &format!("user.name={}", author.name),
                    "-c",
                    &format!("user.email={}", author.email),
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-q",
                    "--no-verify",
                    "-m",
                    &message,
                ],
                "commit the work to rescue",
            )?;
        }
        Ok(Rescue { branch, untracked })
    }

    /// Whether git tracks the file at `path`, relative to the worktree: whether its index holds it.
    ///
    /// # Errors
    /// [`GitFailed`] when git cannot tell.
    pub fn tracks(&self, path: &str) -> Result<bool, GitFailed> {
        let listed = self.text(
            &["ls-files", "-z", "--", &format!(":(literal){path}")],
            "tell whether it tracks a file",
        )?;
        Ok(!listed.is_empty())
    }

    /// Whether git ignores the path `path`, relative to the worktree, by the repository's ignore
    /// rules; a tracked file is never ignored. The path need not exist.
    ///
    /// # Errors
    /// [`GitFailed`] when git cannot tell.
    pub fn ignores(&self, path: &str) -> Result<bool, GitFailed> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(["check-ignore", "-q", "--", path])
            .output()
            .map_err(|error| {
                tracing::debug!(error = %error, "git could not be started");
                self.failed("tell whether it ignores a file")
            })?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => {
                tracing::debug!(
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "git refused in a worktree"
                );
                Err(self.failed("tell whether it ignores a file"))
            }
        }
    }

    /// The address git is set up with here, when it has one.
    pub fn email(&self) -> Option<String> {
        self.text(&["config", "user.email"], "read the address")
            .ok()
            .map(|email| email.trim().to_owned())
            .filter(|email| !email.is_empty())
    }

    /// The rescue branches made from the worktree `name`, by name.
    pub fn rescue_branches(&self, name: &WorktreeName) -> Vec<String> {
        let pattern = format!("refs/heads/{RESCUE_PREFIX}{name}-*");
        self.text(
            &["for-each-ref", "--format=%(refname:short)", &pattern],
            "list the rescue branches",
        )
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
    }

    fn is_dirty(&self) -> Result<bool, GitFailed> {
        let status = self.text(
            &[
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--",
                ".",
                &exclude_own_file(),
            ],
            "read the worktree's status",
        )?;
        Ok(!status.trim().is_empty())
    }

    /// The untracked files of the tree that git does not ignore, its own `.dibs.toml` left out.
    fn untracked(&self) -> Result<Vec<String>, GitFailed> {
        let listed = self.text(
            &[
                "ls-files",
                "-z",
                "--others",
                "--exclude-standard",
                "--",
                ".",
                &exclude_own_file(),
            ],
            "list the untracked files to rescue",
        )?;
        Ok(listed
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// `wanted`, or `wanted-2`, `wanted-3`, … when a branch of that name exists.
    fn free_branch(&self, wanted: &str) -> Result<String, GitFailed> {
        for n in 1..100 {
            let branch = if n == 1 {
                wanted.to_owned()
            } else {
                format!("{wanted}-{n}")
            };
            let exists = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(["rev-parse", "--verify", "-q"])
                .arg(format!("refs/heads/{branch}"))
                .output()
                .map_err(|_| self.failed("look for a branch"))?;
            if !exists.status.success() {
                return Ok(branch);
            }
        }
        Err(self.failed("name a rescue branch"))
    }

    fn text(&self, args: &[&str], action: &str) -> Result<String, GitFailed> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .map_err(|error| {
                tracing::debug!(error = %error, "git could not be started");
                self.failed(action)
            })?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        tracing::debug!(
            stderr = %String::from_utf8_lossy(&output.stderr),
            "git refused in a worktree"
        );
        Err(self.failed(action))
    }

    fn failed(&self, action: &str) -> GitFailed {
        GitFailed {
            action: action.to_owned(),
            dir: self.dir.display().to_string(),
        }
    }
}

/// The pathspec that leaves the worktree's own `.dibs.toml` out. A status takes it as it is; a
/// staging that names an ignored file even to leave it out is refused by git, so a rescue stages
/// everything and then puts the file back as `HEAD` has it ([`own_file`]).
fn exclude_own_file() -> String {
    format!(":(exclude,top){FILE_NAME}")
}

/// The pathspec of the worktree's own `.dibs.toml`.
fn own_file() -> String {
    format!(":(top){FILE_NAME}")
}

/// `dibs/rescue/<name>-<yyyymmddThhmmss>`, the time in UTC.
pub fn rescue_branch(name: &WorktreeName, at: OffsetDateTime) -> String {
    let at = at.to_offset(UtcOffset::UTC);
    format!(
        "{RESCUE_PREFIX}{name}-{:04}{:02}{:02}T{:02}{:02}{:02}",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rescue(untracked: &[&str]) -> Rescue {
        Rescue {
            branch: "dibs/rescue/worktree1-20260302T090507".to_owned(),
            untracked: untracked.iter().map(|path| (*path).to_owned()).collect(),
        }
    }

    #[test]
    fn a_rescue_names_the_first_untracked_files_it_committed_and_counts_the_rest() {
        assert_eq!(rescue(&[]).untracked_sentence(), None);
        assert_eq!(
            rescue(&["dump.sql"]).untracked_sentence().unwrap(),
            "It committed 1 untracked file: dump.sql; look at it before you push the branch."
        );
        assert_eq!(
            rescue(&["a", "b", "c", "d", "e", "f", "g"])
                .untracked_sentence()
                .unwrap(),
            "It committed 7 untracked files: a, b, c, d, e and 2 more; look at them before you push the branch."
        );
    }

    #[test]
    fn a_rescue_branch_names_the_worktree_and_the_utc_second() {
        let name = WorktreeName::parse("worktree2").unwrap();
        let at = OffsetDateTime::from_unix_timestamp(1_772_442_307)
            .unwrap()
            .to_offset(UtcOffset::from_hms(2, 0, 0).unwrap());
        assert_eq!(
            rescue_branch(&name, at),
            "dibs/rescue/worktree2-20260302T090507"
        );
    }
}
