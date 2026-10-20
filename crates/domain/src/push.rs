//! Which branches a `git push` updates, judged against the protected branches of the repository it
//! pushes to. Unlike a slot's freeze, which is best effort, protection fails closed: a push whose
//! branches cannot be read from the command line and the checkout's git settings is treated as one
//! that may update a protected branch.
//!
//! What a push updates comes from the line — the remote, the refspecs, `--all`, `--mirror`,
//! `--delete` — and, where the line leaves it to git, from the checkout's settings: the current
//! branch, its push remote and upstream, `push.default`, `remote.<name>.push`, the remotes' URLs
//! and `url.<base>.insteadOf`. Which repository it pushes to is the listed repository whose remote
//! is the URL of the remote pushed to; a remote no listed repository has — a fork — protects
//! nothing. A project that lists no repositories has one repository, which every remote is.

use std::collections::BTreeSet;

use crate::handle::Handle;
use crate::protection::ProtectedRef;
use crate::repo::RemoteUrl;
use crate::settings::{ProjectSettings, RepoSpec};
use crate::slot::{CommandVerdict, RepoRef, SimpleCommand};

/// The remote a push goes to when git is left to choose and nothing else names one.
const DEFAULT_REMOTE: &str = "origin";

/// What a push sends, as its options say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushScope {
    /// The refspecs named, or, with none, what git's settings push by default.
    Refspecs,
    /// Every local branch (`--all`, `--branches`).
    AllBranches,
    /// Every ref, and the deletion of those the checkout lacks (`--mirror`, `--prune`).
    Mirror,
    /// The tags only (`--tags` with no refspec).
    TagsOnly,
}

/// One `git push` of a command line, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPush {
    /// The directory of `git -C <dir>`, when one was given.
    pub dir: Option<String>,
    /// The remote named, by name or URL.
    pub remote: Option<String>,
    /// The refspecs named, in order.
    pub refspecs: Vec<String>,
    /// What it sends besides them.
    pub scope: PushScope,
    /// Whether the refspecs are refs to delete (`--delete`, `-d`).
    pub delete: bool,
    /// Whether something on the line makes its target unreadable: another git directory or work
    /// tree, a namespace, an option whose value is missing.
    pub unreadable: bool,
}

/// The `git push` commands among `commands`, in order.
pub fn git_pushes(commands: &[&SimpleCommand]) -> Vec<GitPush> {
    commands
        .iter()
        .filter_map(|command| git_push(&command.words))
        .collect()
}

fn program_name(word: &str) -> &str {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// The push `words` run, when they run `git … push …`.
fn git_push(words: &[String]) -> Option<GitPush> {
    let mut words = words.iter().map(String::as_str);
    if words.next().map(program_name) != Some("git") {
        return None;
    }
    let mut push = GitPush {
        dir: None,
        remote: None,
        refspecs: Vec::new(),
        scope: PushScope::Refspecs,
        delete: false,
        unreadable: false,
    };
    loop {
        match words.next()? {
            "push" => break,
            "-C" => push.dir = Some(words.next()?.to_owned()),
            "-c" => {
                words.next()?;
            }
            "--git-dir" | "--work-tree" | "--namespace" => {
                words.next();
                push.unreadable = true;
            }
            option
                if option.starts_with("--git-dir=")
                    || option.starts_with("--work-tree=")
                    || option.starts_with("--namespace=") =>
            {
                push.unreadable = true;
            }
            option if option.starts_with('-') => {}
            _ => return None,
        }
    }
    let mut tags = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut options_end = false;
    while let Some(word) = words.next() {
        if options_end || !word.starts_with('-') || word == "-" {
            positional.push(word);
            continue;
        }
        match word {
            "--" => options_end = true,
            "--all" | "--branches" => push.scope = widest(push.scope, PushScope::AllBranches),
            "--mirror" | "--prune" => push.scope = PushScope::Mirror,
            "--tags" => tags = true,
            "--delete" => push.delete = true,
            "--repo" => match words.next() {
                Some(remote) => push.remote = Some(remote.to_owned()),
                None => push.unreadable = true,
            },
            "-o" | "--push-option" | "--receive-pack" | "--exec" => {
                if words.next().is_none() {
                    push.unreadable = true;
                }
            }
            long if long.starts_with("--repo=") => {
                push.remote = Some(long.trim_start_matches("--repo=").to_owned());
            }
            long if long.starts_with("--") => {}
            short => {
                if short.contains('d') {
                    push.delete = true;
                }
            }
        }
    }
    let mut positional = positional.into_iter();
    if push.remote.is_none() {
        push.remote = positional.next().map(str::to_owned);
    }
    push.refspecs = positional.map(str::to_owned).collect();
    if tags && push.refspecs.is_empty() && push.scope == PushScope::Refspecs {
        push.scope = PushScope::TagsOnly;
    }
    Some(push)
}

fn widest(current: PushScope, asked: PushScope) -> PushScope {
    if current == PushScope::Mirror {
        current
    } else {
        asked
    }
}

/// A checkout's git settings, as `git config --list` and its current branch tell them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitFacts {
    /// The current branch's short name; `None` on a detached head.
    pub branch: Option<String>,
    /// Every setting in force, `key` as git writes it (section and name in lower case, a
    /// subsection as written), in the order git lists them; the last of a key wins.
    pub config: Vec<(String, String)>,
}

impl GitFacts {
    /// The value of `key` in force.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.config
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Whether `key` is set at all.
    fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// The remote a push without one goes to, from the current branch.
    fn default_remote(&self) -> String {
        let branch = self.branch.as_deref().unwrap_or_default();
        self.get(&format!("branch.{branch}.pushremote"))
            .or_else(|| self.get("remote.pushdefault"))
            .or_else(|| self.get(&format!("branch.{branch}.remote")))
            .unwrap_or(DEFAULT_REMOTE)
            .to_owned()
    }

    /// The URL a push to `remote` (a remote's name, or a URL) identifies its repository by: a named
    /// remote's fetch URL, else its push URL, else the text itself; rewritten by the longest
    /// `url.<base>.insteadOf` prefix that matches.
    fn url_of(&self, remote: &str) -> String {
        let named = self
            .get(&format!("remote.{remote}.url"))
            .or_else(|| self.get(&format!("remote.{remote}.pushurl")))
            .unwrap_or(remote);
        let rewrite = self
            .config
            .iter()
            .filter_map(|(key, prefix)| {
                let base = key
                    .strip_prefix("url.")?
                    .strip_suffix(".insteadof")
                    .or_else(|| key.strip_prefix("url.")?.strip_suffix(".pushinsteadof"))?;
                named.starts_with(prefix.as_str()).then_some((base, prefix))
            })
            .max_by_key(|(_, prefix)| prefix.len());
        match rewrite {
            Some((base, prefix)) => format!("{base}{}", &named[prefix.len()..]),
            None => named.to_owned(),
        }
    }
}

/// What a push may update, as far as it can be read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reach {
    /// This branch, `refs/heads/<name>`.
    Branch(String),
    /// Any branch matching this one-`*` pattern.
    Pattern(String),
    /// Any branch: every branch is pushed, or which ones cannot be read.
    Any,
}

/// The branches a push updates in the repository it goes to; empty when it updates none.
fn reach(push: &GitPush, remote: &str, facts: &GitFacts) -> Vec<Reach> {
    match push.scope {
        PushScope::AllBranches | PushScope::Mirror => return vec![Reach::Any],
        PushScope::TagsOnly => return Vec::new(),
        PushScope::Refspecs => {}
    }
    if push.refspecs.is_empty() {
        return default_reach(remote, facts);
    }
    push.refspecs
        .iter()
        .filter_map(|spec| refspec_reach(spec, push.delete, facts))
        .collect()
}

/// What a push with no refspec updates, as `remote.<name>.push` and `push.default` say.
fn default_reach(remote: &str, facts: &GitFacts) -> Vec<Reach> {
    if facts.has(&format!("remote.{remote}.push")) {
        return vec![Reach::Any];
    }
    let mode = facts.get("push.default").unwrap_or("simple");
    if mode == "nothing" {
        return Vec::new();
    }
    if mode == "matching" {
        return vec![Reach::Any];
    }
    let Some(branch) = facts.branch.as_deref() else {
        return Vec::new();
    };
    let mut reached = vec![Reach::Branch(format!("refs/heads/{branch}"))];
    let upstream_remote = facts.get(&format!("branch.{branch}.remote"));
    if mode != "current"
        && upstream_remote == Some(remote)
        && let Some(merge) = facts.get(&format!("branch.{branch}.merge"))
    {
        reached.push(branch_reach(merge));
    }
    reached
}

/// What one refspec updates; `None` for a ref that is no branch, such as a tag.
fn refspec_reach(spec: &str, delete: bool, facts: &GitFacts) -> Option<Reach> {
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    if spec == ":" {
        return Some(Reach::Any);
    }
    let destination = match spec.split_once(':') {
        Some((_, "")) => return Some(Reach::Any),
        Some((_, destination)) => destination,
        None if !delete && (spec == "HEAD" || spec == "@") => {
            return Some(match &facts.branch {
                Some(branch) => Reach::Branch(format!("refs/heads/{branch}")),
                None => Reach::Any,
            });
        }
        None => spec,
    };
    if destination == "HEAD" || destination == "@" {
        return Some(Reach::Any);
    }
    if !destination.starts_with("refs/heads/") && destination.starts_with("refs/") {
        return None;
    }
    Some(branch_reach(destination))
}

/// A destination as a branch reach: a full ref kept, a short name under `refs/heads/`.
fn branch_reach(name: &str) -> Reach {
    let full = if name.starts_with("refs/") {
        name.to_owned()
    } else {
        format!("refs/heads/{name}")
    };
    if full.contains('*') {
        Reach::Pattern(full)
    } else {
        Reach::Branch(full)
    }
}

/// Whether a one-`*` refspec pattern matches `name`.
fn pattern_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        Some((prefix, suffix)) => {
            name.len() >= prefix.len() + suffix.len()
                && name.starts_with(prefix)
                && name.ends_with(suffix)
        }
        None => pattern == name,
    }
}

/// The repository of the project a remote's URL is; `None` for a remote no listed repository has.
fn listed_repo<'s>(url: &str, settings: &'s ProjectSettings) -> Option<&'s RepoSpec> {
    if !settings.lists_repositories() {
        return settings.repos().first();
    }
    RemoteUrl::parse(url)
        .ok()
        .and_then(|remote| settings.repo_by_remote(&remote))
}

/// Who pushes, from where, under which settings.
#[derive(Clone, Copy)]
pub struct PushProtection<'a> {
    /// The member pushing.
    pub pusher: &'a Handle,
    /// The project's settings.
    pub settings: &'a ProjectSettings,
    /// The git settings of the checkout the push runs in; `None` when they could not be read,
    /// which leaves every push unreadable.
    pub facts: Option<&'a GitFacts>,
    /// Whether a `git -C <dir>` directory lies in the checkout the push runs in.
    pub here: &'a dyn Fn(&str) -> bool,
}

impl PushProtection<'_> {
    /// Whether any repository protects a branch: only then is a push worth reading.
    pub fn applies(&self) -> bool {
        self.settings
            .repos()
            .iter()
            .any(|repo| !repo.protected_refs.is_empty())
    }

    /// [`CommandVerdict::NeedsPermission`] for the first push among `pushes` that may update a
    /// branch the member may not push to.
    pub fn refusal(&self, pushes: &[GitPush]) -> Option<CommandVerdict> {
        pushes.iter().find_map(|push| self.push_refusal(push))
    }

    fn push_refusal(&self, push: &GitPush) -> Option<CommandVerdict> {
        let readable = !push.unreadable && push.dir.as_deref().is_none_or(|dir| (self.here)(dir));
        let Some(facts) = self.facts.filter(|_| readable) else {
            return self.any_repo_refusal();
        };
        let remote = push
            .remote
            .clone()
            .unwrap_or_else(|| facts.default_remote());
        let repo = listed_repo(&facts.url_of(&remote), self.settings)?;
        self.repo_refusal(repo, &reach(push, &remote, facts))
    }

    /// The refusal of a push to `remote` (a remote's name or URL, as git hands it to its pre-push
    /// hook, or `None` when it was not handed) whose remote refs are `refs`, exactly. Without a
    /// remote, the push is judged as one to `fallback`, the repository the checkout governs; with
    /// neither, it may go anywhere, and any protected branch the member may not push to refuses
    /// it.
    pub fn refs_refusal(
        &self,
        remote: Option<&str>,
        url: Option<&str>,
        refs: &[String],
        fallback: Option<&RepoSpec>,
    ) -> Option<CommandVerdict> {
        let reached: Vec<Reach> = refs
            .iter()
            .filter(|name| name.starts_with("refs/heads/"))
            .map(|name| Reach::Branch(name.clone()))
            .collect();
        let repo = match (remote, self.facts) {
            (Some(remote), Some(facts)) if facts.has(&format!("remote.{remote}.url")) => {
                listed_repo(&facts.url_of(remote), self.settings)
            }
            (Some(remote), facts) => {
                let url = url.unwrap_or(remote);
                let url = facts.map_or_else(|| url.to_owned(), |facts| facts.url_of(url));
                listed_repo(&url, self.settings)
            }
            (None, _) => match fallback {
                Some(repo) => Some(repo),
                None => return self.any_repo_refusal(),
            },
        }?;
        self.repo_refusal(repo, &reached)
    }

    /// The protected branches of `repo` the member may not push to.
    fn blocked<'r>(&self, repo: &'r RepoSpec) -> Vec<&'r ProtectedRef> {
        repo.protected_refs
            .iter()
            .filter(|rule| !rule.allows(self.pusher))
            .collect()
    }

    fn repo_refusal(&self, repo: &RepoSpec, reached: &[Reach]) -> Option<CommandVerdict> {
        let blocked = self.blocked(repo);
        reached.iter().find_map(|reach| {
            let (rule, certain) = match reach {
                Reach::Branch(name) => {
                    (blocked.iter().find(|rule| rule.target.name == *name)?, true)
                }
                Reach::Pattern(pattern) => (
                    blocked
                        .iter()
                        .find(|rule| pattern_matches(pattern, &rule.target.name))?,
                    true,
                ),
                Reach::Any => (blocked.first()?, false),
            };
            Some(needs_permission(rule, certain))
        })
    }

    /// The refusal of a push whose repository cannot be read: any protected branch of the
    /// project the member may not push to.
    fn any_repo_refusal(&self) -> Option<CommandVerdict> {
        self.settings
            .repos()
            .iter()
            .find_map(|repo| self.blocked(repo).first().copied())
            .map(|rule| needs_permission(rule, false))
    }
}

fn needs_permission(rule: &ProtectedRef, certain: bool) -> CommandVerdict {
    CommandVerdict::NeedsPermission {
        target: RepoRef::clone(&rule.target),
        pushers: BTreeSet::clone(&rule.pushers),
        certain,
    }
}

#[cfg(test)]
mod tests;
