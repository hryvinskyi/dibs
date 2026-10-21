//! Protected refs: branches and tags of a repository that only the members a project names may push
//! to. Everyone else's way to such a branch is a pull request that one of those members merges; a
//! protected tag is pushed by one of them.

use std::collections::BTreeSet;
use std::fmt;

use thiserror::Error;

use crate::handle::Handle;
use crate::repo::RepoId;
use crate::slot::{MAX_REF_NAME, RepoRef, ref_part_is_clean};

/// Where the branches of a repository are.
const BRANCHES: &str = "refs/heads/";

/// Where the tags of a repository are.
const TAGS: &str = "refs/tags/";

/// The kind of ref a protection covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// A branch, `refs/heads/<branch>`.
    Branch,
    /// A tag, `refs/tags/<name>`.
    Tag,
}

/// The refs one protection covers in one repository: a branch, `refs/heads/<branch>`, exactly; or
/// tags, `refs/tags/<name>` exactly, or `refs/tags/<prefix>*` for every tag whose name starts with
/// the prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedTarget {
    repo: RepoId,
    name: String,
}

/// Why a text names no ref a protection may cover.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtectedTargetError {
    /// Neither a branch ref nor a tag ref.
    #[error("it is written 'refs/heads/<branch>', 'refs/tags/<name>' or 'refs/tags/<prefix>*'")]
    Unknown,
    /// A branch ref git would not accept, a pattern included: a branch is named exactly.
    #[error("'refs/heads/<branch>' names one branch exactly, without a pattern")]
    Branch,
    /// Nothing after `refs/tags/`.
    #[error("a tag ref names a tag, or a prefix followed by '*', after 'refs/tags/'")]
    EmptyTag,
    /// `**` in a tag pattern.
    #[error("a tag pattern has one '*', at its end; '**' is not accepted")]
    DoubleStar,
    /// A `*` before the end of a tag pattern.
    #[error("a tag pattern has its '*' at the end only, as in 'refs/tags/v*'")]
    StarNotAtEnd,
    /// A tag name, or the prefix of a pattern, git would not accept.
    #[error(
        "the tag name is not one git accepts: no space, control character or any of ~^:?[\\, no '..', '@{{' or '//', no part that starts with '.' or ends with '.lock', and no '.' or '/' at its end"
    )]
    Tag,
}

/// Whether `tag`, a tag's name after `refs/tags/`, or with `pattern` the prefix before a pattern's
/// `*`, follows git's rules for a ref name: [`ref_part_is_clean`], no part between slashes that is
/// empty, starts with `.` or ends with `.lock`, and no `.` or `/` at the end. A prefix may be empty,
/// and its last part is unfinished: it may end with `/`, `.` or `.lock`, or be empty, since what
/// the `*` adds completes it.
fn tag_is_clean(tag: &str, pattern: bool) -> bool {
    if !ref_part_is_clean(tag) {
        return false;
    }
    let mut parts: Vec<&str> = tag.split('/').collect();
    let last = parts.pop().unwrap_or_default();
    let whole =
        |part: &&str| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock");
    if !parts.iter().all(whole) || last.starts_with('.') {
        return false;
    }
    pattern || (!last.is_empty() && !last.ends_with('.') && !last.ends_with(".lock"))
}

impl ProtectedTarget {
    /// The refs `text` names in `repo`.
    ///
    /// # Errors
    /// [`ProtectedTargetError`] for a text that names no branch exactly and no tags as above.
    pub fn parse(repo: RepoId, text: &str) -> Result<Self, ProtectedTargetError> {
        if text.starts_with(BRANCHES) {
            let branch = RepoRef::of(repo, text).ok_or(ProtectedTargetError::Branch)?;
            return Ok(Self {
                repo: branch.repo,
                name: branch.name,
            });
        }
        let tag = text
            .strip_prefix(TAGS)
            .ok_or(ProtectedTargetError::Unknown)?;
        if tag.is_empty() {
            return Err(ProtectedTargetError::EmptyTag);
        }
        if tag.contains("**") {
            return Err(ProtectedTargetError::DoubleStar);
        }
        let (literal, pattern) = match tag.strip_suffix('*') {
            Some(prefix) => (prefix, true),
            None => (tag, false),
        };
        if literal.contains('*') {
            return Err(ProtectedTargetError::StarNotAtEnd);
        }
        if text.len() > MAX_REF_NAME || !tag_is_clean(literal, pattern) {
            return Err(ProtectedTargetError::Tag);
        }
        Ok(Self {
            repo,
            name: text.to_owned(),
        })
    }

    /// The repository.
    pub fn repo(&self) -> &RepoId {
        &self.repo
    }

    /// The ref or the pattern as written: `refs/heads/main`, `refs/tags/v1.0`, `refs/tags/v*`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name after `refs/heads/` or `refs/tags/`: `main`, `v1.0`, `v*`.
    pub fn short_name(&self) -> &str {
        self.name
            .strip_prefix(BRANCHES)
            .or_else(|| self.name.strip_prefix(TAGS))
            .unwrap_or(&self.name)
    }

    /// What precedes the `*` of a pattern, the full ref (`refs/tags/v`); `None` for a ref named
    /// exactly.
    pub fn prefix(&self) -> Option<&str> {
        self.name.strip_suffix('*')
    }

    /// Whether it covers branches or tags.
    pub fn kind(&self) -> RefKind {
        if self.name.starts_with(BRANCHES) {
            RefKind::Branch
        } else {
            RefKind::Tag
        }
    }

    /// Whether the full ref `name` of this repository is one it covers: the same ref, or for a
    /// pattern, a ref that starts with what precedes its `*`.
    pub fn covers(&self, name: &str) -> bool {
        match self.prefix() {
            Some(prefix) => name.starts_with(prefix),
            None => self.name == name,
        }
    }
}

impl fmt::Display for ProtectedTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.repo, self.name)
    }
}

/// A branch or tags only `pushers` may push to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedRef {
    /// The refs it covers.
    pub target: ProtectedTarget,
    /// The members who may push to them, by handle; none means nobody pushes to them directly.
    pub pushers: BTreeSet<Handle>,
}

impl ProtectedRef {
    /// Whether the member may push to the refs it covers.
    pub fn allows(&self, pusher: &Handle) -> bool {
        self.pushers.contains(pusher)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(text: &str) -> Handle {
        Handle::parse(text).unwrap()
    }

    fn api() -> RepoId {
        RepoId::parse("api").unwrap()
    }

    fn target(text: &str) -> Result<ProtectedTarget, ProtectedTargetError> {
        ProtectedTarget::parse(api(), text)
    }

    #[test]
    fn only_a_listed_pusher_may_push_and_an_empty_list_lets_nobody() {
        let main = target("refs/heads/main").unwrap();
        let protected = ProtectedRef {
            target: main.clone(),
            pushers: BTreeSet::from([handle("alice")]),
        };
        assert!(protected.allows(&handle("alice")));
        assert!(!protected.allows(&handle("bob")));
        let nobody = ProtectedRef {
            target: main,
            pushers: BTreeSet::new(),
        };
        assert!(!nobody.allows(&handle("alice")));
    }

    #[test]
    fn a_branch_is_covered_exactly() {
        let main = target("refs/heads/main").unwrap();
        assert_eq!(main.kind(), RefKind::Branch);
        assert_eq!(main.to_string(), "api:refs/heads/main");
        assert_eq!(main.name(), "refs/heads/main");
        assert_eq!(main.repo(), &api());
        assert!(main.covers("refs/heads/main"));
        assert!(!main.covers("refs/heads/main2"));
        assert!(!main.covers("refs/tags/main"));
    }

    #[test]
    fn a_branch_takes_no_pattern() {
        for text in [
            "refs/heads/release/*",
            "refs/heads/*",
            "refs/heads/",
            "refs/heads/a..b",
        ] {
            assert_eq!(target(text), Err(ProtectedTargetError::Branch), "{text}");
        }
    }

    #[test]
    fn a_tag_is_covered_by_its_name_or_by_a_prefix_that_ends_in_a_star() {
        let exact = target("refs/tags/v1.0").unwrap();
        assert_eq!(exact.kind(), RefKind::Tag);
        assert!(exact.covers("refs/tags/v1.0"));
        assert!(!exact.covers("refs/tags/v1.0.1"));

        let releases = target("refs/tags/v*").unwrap();
        assert_eq!(releases.kind(), RefKind::Tag);
        assert_eq!(releases.to_string(), "api:refs/tags/v*");
        for name in ["refs/tags/v1.2.0", "refs/tags/v", "refs/tags/v2/rc1"] {
            assert!(releases.covers(name), "{name}");
        }
        for name in ["refs/tags/1.2.0", "refs/heads/v1", "refs/tags/xv1"] {
            assert!(!releases.covers(name), "{name}");
        }

        assert_eq!(exact.short_name(), "v1.0");
        assert_eq!(exact.prefix(), None);
        assert_eq!(releases.short_name(), "v*");
        assert_eq!(releases.prefix(), Some("refs/tags/v"));
        let every = target("refs/tags/*").unwrap();
        assert!(every.covers("refs/tags/anything"));
        assert!(!every.covers("refs/heads/main"));
        let nested = target("refs/tags/release/*").unwrap();
        assert!(nested.covers("refs/tags/release/2.4"));
        assert!(!nested.covers("refs/tags/release-2.4"));
    }

    #[test]
    fn a_tag_pattern_has_one_star_at_its_end_and_something_to_cover() {
        assert_eq!(target("refs/tags/"), Err(ProtectedTargetError::EmptyTag));
        assert_eq!(
            target("refs/tags/v**"),
            Err(ProtectedTargetError::DoubleStar)
        );
        assert_eq!(
            target("refs/tags/**"),
            Err(ProtectedTargetError::DoubleStar)
        );
        for text in ["refs/tags/*v", "refs/tags/v*.0", "refs/tags/v*/*"] {
            assert_eq!(
                target(text),
                Err(ProtectedTargetError::StarNotAtEnd),
                "{text}"
            );
        }
        for text in [
            "refs/tags/v 1",
            "refs/tags/a..b",
            "refs/tags//v",
            "refs/tags/v1/",
            "refs/tags/v1.lock",
            "refs/tags/v?",
            "refs/tags/v1.",
            "refs/tags/.hidden",
            "refs/tags/release/.v1",
            "refs/tags/release.lock/v1",
            "refs/tags/v@{1}",
            "refs/tags/v~1",
            "refs/tags/v^1",
            "refs/tags/v:1",
            "refs/tags/v[1]",
            "refs/tags/v\\1",
            "refs/tags/v\t1",
            "refs/tags/.*",
            "refs/tags/release/.*",
            "refs/tags/x.lock/*",
            "refs/tags/a..*",
        ] {
            assert_eq!(target(text), Err(ProtectedTargetError::Tag), "{text}");
        }
        for text in [
            "refs/tags/v1.*",
            "refs/tags/release/*",
            "refs/tags/v1.lock*",
            "refs/tags/release/2.4",
        ] {
            assert!(target(text).is_ok(), "{text}");
        }
        for text in ["main", "refs/notes/x", "refs/pull/1/head", "tags/v1"] {
            assert_eq!(target(text), Err(ProtectedTargetError::Unknown), "{text}");
        }
    }
}
