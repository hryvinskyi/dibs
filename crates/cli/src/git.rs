//! What the command line asks git about a checkout: its `origin`, where the checkout sits in its
//! repository, the paths a range of commits changed, and the settings a push reads.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use dibs_domain::push::GitFacts;
use dibs_domain::repo::RemoteUrl;

use crate::failure::Failure;

/// The git repository a checkout lives in.
#[derive(Debug, Clone)]
pub struct Git {
    dir: PathBuf,
}

impl Git {
    /// Git run in `dir`.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// The checkout's `origin`, normalised; `None` when it has none git can name or it is no
    /// remote form a repository has.
    pub fn origin(&self) -> Option<RemoteUrl> {
        let url = self.run(&["remote", "get-url", "origin"]).ok()?;
        RemoteUrl::parse(String::from_utf8_lossy(&url).trim()).ok()
    }

    /// What a push from this directory reads: the current branch (`None` on a detached head) and
    /// every setting in force. `None` when git cannot list the settings. Two git runs.
    pub fn push_facts(&self) -> Option<GitFacts> {
        let config = self.run(&["config", "--list", "-z"]).ok()?;
        let branch = self
            .run(&["symbolic-ref", "-q", "--short", "HEAD"])
            .ok()
            .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
            .filter(|branch| !branch.is_empty());
        Some(GitFacts {
            branch,
            config: parse_config(&config),
        })
    }

    /// The paths the non-merge commits of `range` changed, both sides of every rename, relative
    /// to this directory; paths outside it are left out. With `author`, only that author's
    /// commits; `me` is the author git is set up as here.
    ///
    /// # Errors
    /// A [`Failure`] naming the range when git refuses it or cannot run.
    pub fn changed_paths(&self, range: &str, author: Option<&str>) -> Result<Vec<String>, Failure> {
        let prefix = self.run(&["rev-parse", "--show-prefix"])?;
        let prefix = String::from_utf8_lossy(&prefix).trim().to_owned();
        let mut args = vec![
            "log".to_owned(),
            "--no-merges".to_owned(),
            "--name-status".to_owned(),
            "-M".to_owned(),
            "-z".to_owned(),
            "--format=".to_owned(),
        ];
        if let Some(author) = author {
            let who = if author == "me" {
                let email = self.run(&["config", "user.email"])?;
                String::from_utf8_lossy(&email).trim().to_owned()
            } else {
                author.to_owned()
            };
            args.push(format!("--author={who}"));
        }
        args.push(range.to_owned());
        args.push("--".to_owned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.run(&refs)?;
        Ok(parse_name_status(&output)
            .into_iter()
            .filter_map(|path| path.strip_prefix(&prefix).map(str::to_owned))
            .collect())
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>, Failure> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .map_err(|error| {
                tracing::debug!(error = %error, "git could not be started");
                Failure::config(
                    "git",
                    "git could not be run; is it installed and on the PATH?",
                )
            })?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        tracing::debug!(stderr = %String::from_utf8_lossy(&output.stderr), "git refused");
        Err(Failure::config(
            "git",
            format!(
                "git refused 'git {}' in {}",
                args.join(" "),
                self.dir.display()
            ),
        ))
    }
}

/// The settings of `git config --list -z` output, in order: each entry is the key, a line break and
/// the value, entries ending in NUL; a key without a value is a boolean set to true.
pub fn parse_config(output: &[u8]) -> Vec<(String, String)> {
    output
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            match entry.split_once('\n') {
                Some((key, value)) => (key.to_owned(), value.to_owned()),
                None => (entry.into_owned(), "true".to_owned()),
            }
        })
        .collect()
}

/// The paths of `git log --name-status -z --format=` output, in order of first appearance: one
/// path per entry, two for a rename or a copy.
pub fn parse_name_status(output: &[u8]) -> Vec<String> {
    let mut tokens = output
        .split(|byte| *byte == 0)
        .map(|token| String::from_utf8_lossy(token).into_owned());
    let mut seen = BTreeSet::new();
    let mut paths = Vec::new();
    while let Some(token) = tokens.next() {
        let status = token.trim_start_matches('\n');
        let Some(kind) = status.chars().next() else {
            continue;
        };
        let count = if matches!(kind, 'R' | 'C') { 2 } else { 1 };
        for _ in 0..count {
            if let Some(path) = tokens.next()
                && seen.insert(path.clone())
            {
                paths.push(path);
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_read_key_by_key_with_a_bare_key_as_true() {
        let output: &[u8] = &[
            &b"remote.origin.url\ngit@example.com:acme/api.git\0core.bare\nfalse\0"[..],
            &b"branch.Main.merge\nrefs/heads/main\0push.autosetupremote\0"[..],
        ]
        .concat();
        assert_eq!(
            parse_config(output),
            [
                (
                    "remote.origin.url".to_owned(),
                    "git@example.com:acme/api.git".to_owned()
                ),
                ("core.bare".to_owned(), "false".to_owned()),
                ("branch.Main.merge".to_owned(), "refs/heads/main".to_owned()),
                ("push.autosetupremote".to_owned(), "true".to_owned()),
            ]
        );
        assert!(parse_config(b"").is_empty());
    }

    #[test]
    fn renames_give_both_sides_and_each_path_appears_once() {
        let output =
            b"M\0b \xc3\xa9.txt\0R100\0a.txt\0c.txt\0\nA\0a.txt\0D\0gone.rs\0C75\0x.rs\0y.rs\0";
        assert_eq!(
            parse_name_status(output),
            ["b é.txt", "a.txt", "c.txt", "gone.rs", "x.rs", "y.rs"]
        );
        assert!(parse_name_status(b"").is_empty());
    }
}
