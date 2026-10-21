//! Which branches a `git push` updates, judged against the protected branches of the repository it
//! pushes to. Unlike a slot's freeze, which is best effort, protection fails closed: a push whose
//! branches cannot be read from the command line and the checkout's git settings is treated as one
//! that may update a protected branch.
//!
//! Protected tags are judged the same way, failing closed too: `--tags`, `--follow-tags` (or
//! `push.followTags`), `--mirror` and a `remote.<name>.push` setting may push any tag; a refspec
//! under `refs/tags/` names its tag; and a short name such as `v1.0`, which git may resolve to a
//! tag, is judged as that tag as well as that branch. The exact refs git hands its pre-push hook
//! are judged against both, a deletion counting as a push of the ref it deletes.
//!
//! What a push updates comes from the line — the remote, the refspecs, `--all`, `--mirror`,
//! `--delete` — and, where the line leaves it to git, from the checkout's settings: the current
//! branch, its push remote and upstream, `push.default`, `remote.<name>.push`, the remotes' URLs
//! and `url.<base>.insteadOf`. Which repository it pushes to is the listed repository whose remote
//! is the URL of the remote pushed to; a remote no listed repository has — a fork — protects
//! nothing. A project that lists no repositories has one repository, which every remote is.

use std::collections::BTreeSet;

use crate::handle::Handle;
use crate::protection::{ProtectedRef, ProtectedTarget, RefKind};
use crate::repo::RemoteUrl;
use crate::settings::{ProjectSettings, RepoSpec};
use crate::slot::{CommandVerdict, SimpleCommand};

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
    /// Whether it pushes tags besides what the refspecs name (`--tags`, `--follow-tags`).
    pub tags: bool,
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
        tags: false,
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
    let mut all_tags = false;
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
            "--tags" => {
                all_tags = true;
                push.tags = true;
            }
            "--follow-tags" => push.tags = true,
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
    if all_tags && push.refspecs.is_empty() && push.scope == PushScope::Refspecs {
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
    /// This ref, exactly: a branch `refs/heads/<name>` or a tag `refs/tags/<name>`.
    Ref(String),
    /// Any branch or tag matching this one-`*` pattern, a full ref.
    Pattern(String),
    /// Any branch: every branch is pushed, or which ones cannot be read.
    AnyBranch,
    /// Any tag: every tag is pushed, or which ones cannot be read.
    AnyTag,
}

/// The branches and tags a push updates in the repository it goes to; empty when it updates
/// none.
fn reach(push: &GitPush, remote: &str, facts: &GitFacts) -> Vec<Reach> {
    let mut reached = match push.scope {
        PushScope::AllBranches => vec![Reach::AnyBranch],
        PushScope::Mirror => vec![Reach::AnyBranch, Reach::AnyTag],
        PushScope::TagsOnly => vec![Reach::AnyTag],
        PushScope::Refspecs if push.refspecs.is_empty() => default_reach(remote, facts),
        PushScope::Refspecs => push
            .refspecs
            .iter()
            .flat_map(|spec| refspec_reach(spec, push.delete, facts))
            .collect(),
    };
    let follows_tags = facts.get("push.followtags") == Some("true");
    if push.tags || follows_tags {
        reached.push(Reach::AnyTag);
    }
    reached
}

/// What a push with no refspec updates, as `remote.<name>.push` and `push.default` say.
fn default_reach(remote: &str, facts: &GitFacts) -> Vec<Reach> {
    if facts.has(&format!("remote.{remote}.push")) {
        return vec![Reach::AnyBranch, Reach::AnyTag];
    }
    let mode = facts.get("push.default").unwrap_or("simple");
    if mode == "nothing" {
        return Vec::new();
    }
    if mode == "matching" {
        return vec![Reach::AnyBranch];
    }
    let Some(branch) = facts.branch.as_deref() else {
        return Vec::new();
    };
    let mut reached = vec![Reach::Ref(format!("refs/heads/{branch}"))];
    let upstream_remote = facts.get(&format!("branch.{branch}.remote"));
    if mode != "current"
        && upstream_remote == Some(remote)
        && let Some(merge) = facts.get(&format!("branch.{branch}.merge"))
    {
        reached.extend(destination_reach(merge));
    }
    reached
}

/// What one refspec updates; empty for a ref that is neither a branch nor a tag.
fn refspec_reach(spec: &str, delete: bool, facts: &GitFacts) -> Vec<Reach> {
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    if spec == ":" {
        return vec![Reach::AnyBranch];
    }
    let destination = match spec.split_once(':') {
        Some((_, "")) => return vec![Reach::AnyBranch, Reach::AnyTag],
        Some((_, destination)) => destination,
        None if !delete && (spec == "HEAD" || spec == "@") => {
            return vec![match &facts.branch {
                Some(branch) => Reach::Ref(format!("refs/heads/{branch}")),
                None => Reach::AnyBranch,
            }];
        }
        None => spec,
    };
    if destination == "HEAD" || destination == "@" {
        return vec![Reach::AnyBranch];
    }
    destination_reach(destination)
}

/// What a destination names: a full branch or tag ref as written; a short name both as the branch
/// and as the tag of that name, since git may resolve it to either; nothing for another ref.
fn destination_reach(name: &str) -> Vec<Reach> {
    if name.starts_with("refs/heads/") || name.starts_with("refs/tags/") {
        return vec![full_reach(name.to_owned())];
    }
    if name.starts_with("refs/") {
        return Vec::new();
    }
    vec![
        full_reach(format!("refs/heads/{name}")),
        full_reach(format!("refs/tags/{name}")),
    ]
}

/// A full ref as a reach: a pattern when it holds a `*`.
fn full_reach(full: String) -> Reach {
    if full.contains('*') {
        Reach::Pattern(full)
    } else {
        Reach::Ref(full)
    }
}

/// Whether a ref a one-`*` refspec pattern matches may be one `target` covers. For a target that
/// is a pattern itself, any overlap of the two counts, so that a push that might reach a protected
/// tag is judged as one that does.
fn pattern_reaches(pattern: &str, target: &ProtectedTarget) -> bool {
    match target.prefix() {
        Some(prefix) => {
            let head = pattern.split_once('*').map_or(pattern, |(head, _)| head);
            head.starts_with(prefix) || prefix.starts_with(head)
        }
        None => pattern_matches(pattern, target.name()),
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
    /// Whether any repository protects a branch or a tag: only then are the refs git names worth
    /// judging.
    pub fn applies(&self) -> bool {
        self.settings
            .repos()
            .iter()
            .any(|repo| !repo.protected_refs.is_empty())
    }

    /// [`CommandVerdict::NeedsPermission`] for the first push among `pushes` that may update a
    /// branch or a tag the member may not push to.
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
    /// hook, or `None` when it was not handed) whose remote refs are `refs`, exactly: a branch or
    /// a tag the member may not push to, a deletion included. Without a remote, the push is judged
    /// as one to `fallback`, the repository the checkout governs; with neither, it may go anywhere,
    /// and any protected branch the member may not push to refuses it, as does a protected tag of
    /// any repository that covers a tag pushed.
    pub fn refs_refusal(
        &self,
        remote: Option<&str>,
        url: Option<&str>,
        refs: &[String],
        fallback: Option<&RepoSpec>,
    ) -> Option<CommandVerdict> {
        let reached: Vec<Reach> = refs.iter().map(|name| Reach::Ref(name.clone())).collect();
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
                None => {
                    return self
                        .any_repo_refusal_of(RefKind::Branch)
                        .or_else(|| self.any_repo_tag_refusal(refs));
                }
            },
        }?;
        self.repo_refusal(repo, &reached)
    }

    /// The protected refs of `repo` the member may not push to.
    fn blocked<'r>(&self, repo: &'r RepoSpec) -> Vec<&'r ProtectedRef> {
        repo.protected_refs
            .iter()
            .filter(|rule| !rule.allows(self.pusher))
            .collect()
    }

    fn repo_refusal(&self, repo: &RepoSpec, reached: &[Reach]) -> Option<CommandVerdict> {
        let blocked = self.blocked(repo);
        let first_of = |kind: RefKind| {
            blocked
                .iter()
                .find(|rule| rule.target.kind() == kind)
                .copied()
        };
        reached.iter().find_map(|reach| {
            let (rule, certain) = match reach {
                Reach::Ref(name) => (*blocked.iter().find(|rule| rule.target.covers(name))?, true),
                Reach::Pattern(pattern) => (
                    *blocked
                        .iter()
                        .find(|rule| pattern_reaches(pattern, &rule.target))?,
                    true,
                ),
                Reach::AnyBranch => (first_of(RefKind::Branch)?, false),
                Reach::AnyTag => (first_of(RefKind::Tag)?, false),
            };
            Some(needs_permission(rule, certain))
        })
    }

    /// The refusal of a command line's push whose repository cannot be read: any protected branch
    /// of the project the member may not push to, or else any protected tag.
    fn any_repo_refusal(&self) -> Option<CommandVerdict> {
        self.any_repo_refusal_of(RefKind::Branch)
            .or_else(|| self.any_repo_refusal_of(RefKind::Tag))
    }

    /// The first protected ref of `kind`, of any repository, the member may not push to.
    fn any_repo_refusal_of(&self, kind: RefKind) -> Option<CommandVerdict> {
        self.settings
            .repos()
            .iter()
            .flat_map(|repo| self.blocked(repo))
            .find(|rule| rule.target.kind() == kind)
            .map(|rule| needs_permission(rule, false))
    }

    /// The refusal of a push of `refs` whose repository cannot be read, for its tags: a protected
    /// tag of any repository that covers one of them and that the member may not push to.
    fn any_repo_tag_refusal(&self, refs: &[String]) -> Option<CommandVerdict> {
        self.settings
            .repos()
            .iter()
            .flat_map(|repo| self.blocked(repo))
            .find(|rule| {
                rule.target.kind() == RefKind::Tag
                    && refs.iter().any(|name| rule.target.covers(name))
            })
            .map(|rule| needs_permission(rule, false))
    }
}

fn needs_permission(rule: &ProtectedRef, certain: bool) -> CommandVerdict {
    CommandVerdict::NeedsPermission {
        target: rule.target.clone(),
        pushers: BTreeSet::clone(&rule.pushers),
        certain,
    }
}

#[cfg(test)]
mod tests;
