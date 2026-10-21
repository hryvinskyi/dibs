//! Protected refs: branches and tags of a repository that only the members a project names may push
//! to. Everyone else's way to such a branch is a pull request that one of those members merges; a
//! protected tag is pushed by one of them.

use std::collections::BTreeSet;
use std::fmt;

use thiserror::Error;

use crate::handle::Handle;
use crate::repo::RepoId;
use crate::slot::{
    BRANCHES, BranchPattern, BranchPatternError, MAX_REF_NAME, StarFault, TAGS, Written,
    ref_name_is_clean, written,
};

/// The kind of ref a protection covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// A branch, `refs/heads/<branch>`.
    Branch,
    /// A tag, `refs/tags/<name>`.
    Tag,
}

/// The refs one protection covers in one repository: branches, `refs/heads/<branch>` exactly or
/// `refs/heads/<prefix>*` for every branch whose name starts with the prefix; or tags,
/// `refs/tags/<name>` exactly or `refs/tags/<prefix>*` for every tag whose name starts with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedTarget(Covered);

/// What a protection covers, by kind.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Covered {
    /// A branch, or the branches of a pattern.
    Branches(BranchPattern),
    /// A tag, or the tags of a pattern, written in full.
    Tags { repo: RepoId, name: String },
}

/// Why a text names no ref a protection may cover.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtectedTargetError {
    /// Neither a branch ref nor a tag ref.
    #[error(
        "it is written 'refs/heads/<branch>', 'refs/heads/<prefix>*', 'refs/tags/<name>' or 'refs/tags/<prefix>*'"
    )]
    Unknown,
    /// A branch ref, or a branch pattern, that breaks its rules.
    #[error(transparent)]
    Branch(BranchPatternError),
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

impl ProtectedTarget {
    /// The refs `text` names in `repo`. A branch pattern has a prefix before its `*`; a tag
    /// pattern's prefix may be empty, so `refs/tags/*` covers every tag.
    ///
    /// # Errors
    /// [`ProtectedTargetError`] for a text that names no branches and no tags as above.
    pub fn parse(repo: RepoId, text: &str) -> Result<Self, ProtectedTargetError> {
        if text.starts_with(BRANCHES) {
            return BranchPattern::named(repo, text)
                .map(|branches| Self(Covered::Branches(branches)))
                .map_err(ProtectedTargetError::Branch);
        }
        let tag = text
            .strip_prefix(TAGS)
            .ok_or(ProtectedTargetError::Unknown)?;
        let (literal, pattern) = match written(tag) {
            Err(StarFault::Empty) => return Err(ProtectedTargetError::EmptyTag),
            Err(StarFault::Double) => return Err(ProtectedTargetError::DoubleStar),
            Err(StarFault::NotAtEnd) => return Err(ProtectedTargetError::StarNotAtEnd),
            Ok(Written::Exact(name)) => (name, false),
            Ok(Written::Prefix(prefix)) => (prefix, true),
        };
        if text.len() > MAX_REF_NAME || !ref_name_is_clean(literal, pattern) {
            return Err(ProtectedTargetError::Tag);
        }
        Ok(Self(Covered::Tags {
            repo,
            name: text.to_owned(),
        }))
    }

    /// The repository.
    pub fn repo(&self) -> &RepoId {
        match &self.0 {
            Covered::Branches(branches) => branches.repo(),
            Covered::Tags { repo, .. } => repo,
        }
    }

    /// The ref or the pattern as written: `refs/heads/main`, `refs/heads/release/*`,
    /// `refs/tags/v1.0`, `refs/tags/v*`.
    pub fn name(&self) -> &str {
        match &self.0 {
            Covered::Branches(branches) => branches.name(),
            Covered::Tags { name, .. } => name,
        }
    }

    /// The name after `refs/heads/` or `refs/tags/`: `main`, `release/*`, `v1.0`, `v*`.
    pub fn short_name(&self) -> &str {
        let name = self.name();
        name.strip_prefix(BRANCHES)
            .or_else(|| name.strip_prefix(TAGS))
            .unwrap_or(name)
    }

    /// What precedes the `*` of a pattern, the full ref (`refs/heads/release/`, `refs/tags/v`);
    /// `None` for a ref named exactly.
    pub fn prefix(&self) -> Option<&str> {
        match &self.0 {
            Covered::Branches(branches) => branches.prefix(),
            Covered::Tags { name, .. } => name.strip_suffix('*'),
        }
    }

    /// Whether it covers branches or tags.
    pub fn kind(&self) -> RefKind {
        match self.0 {
            Covered::Branches(_) => RefKind::Branch,
            Covered::Tags { .. } => RefKind::Tag,
        }
    }

    /// Whether the full ref `name` of this repository is one it covers: the same ref, or for a
    /// pattern, a ref that starts with what precedes its `*`.
    pub fn covers(&self, name: &str) -> bool {
        match &self.0 {
            Covered::Branches(branches) => branches.covers(name),
            Covered::Tags { name: tags, .. } => match tags.strip_suffix('*') {
                Some(prefix) => name.starts_with(prefix),
                None => tags == name,
            },
        }
    }
}

impl fmt::Display for ProtectedTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.repo(), self.name())
    }
}

/// A branch, branches or tags only `pushers` may push to.
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
    fn branches_are_covered_by_a_prefix_that_ends_in_a_star() {
        let releases = target("refs/heads/release/*").unwrap();
        assert_eq!(releases.kind(), RefKind::Branch);
        assert_eq!(releases.to_string(), "api:refs/heads/release/*");
        assert_eq!(releases.short_name(), "release/*");
        assert_eq!(releases.prefix(), Some("refs/heads/release/"));
        for name in [
            "refs/heads/release/2026.10.3",
            "refs/heads/release/",
            "refs/heads/release/2026.10/hotfix",
        ] {
            assert!(releases.covers(name), "{name}");
        }
        for name in [
            "refs/heads/release",
            "refs/heads/release-2026.10.3",
            "refs/heads/main",
            "refs/tags/release/2026.10.3",
        ] {
            assert!(!releases.covers(name), "{name}");
        }
        let dashed = target("refs/heads/release-*").unwrap();
        assert!(dashed.covers("refs/heads/release-2026.10.3"));
        assert!(!dashed.covers("refs/heads/release/2026.10.3"));
        for text in [
            "refs/heads/rel*",
            "refs/heads/release/2026.*",
            "refs/heads/x.lock*",
            "refs/heads/hotfix/x.*",
        ] {
            assert!(target(text).is_ok(), "{text}");
        }
    }

    #[test]
    fn a_branch_pattern_has_one_star_at_its_end_after_a_prefix_git_accepts() {
        let branch = |text: &str| match target(text) {
            Err(ProtectedTargetError::Branch(error)) => error,
            other => panic!("{text}: {other:?}"),
        };
        assert_eq!(branch("refs/heads/"), BranchPatternError::Empty);
        assert_eq!(branch("refs/heads/*"), BranchPatternError::EmptyPrefix);
        assert_eq!(branch("refs/heads/**"), BranchPatternError::DoubleStar);
        assert_eq!(
            branch("refs/heads/release/**"),
            BranchPatternError::DoubleStar
        );
        for text in [
            "refs/heads/*/release",
            "refs/heads/release/*/x",
            "refs/heads/rel*ease",
            "refs/heads/release/*.x",
        ] {
            assert_eq!(branch(text), BranchPatternError::StarNotAtEnd, "{text}");
        }
        for text in [
            "refs/heads/a..b",
            "refs/heads/a..*",
            "refs/heads/release//*",
            "refs/heads/.release/*",
            "refs/heads/release/.*",
            "refs/heads/x.lock/*",
            "refs/heads/re lease/*",
            "refs/heads/release?/*",
            "refs/heads/release@{/*",
            "refs/heads/main.lock",
            "refs/heads/main/",
        ] {
            assert_eq!(branch(text), BranchPatternError::Name, "{text}");
        }
        let long = format!("refs/heads/{}*", "r".repeat(MAX_REF_NAME));
        assert_eq!(branch(&long), BranchPatternError::Name);
        let said = ProtectedTargetError::Branch(BranchPatternError::EmptyPrefix).to_string();
        assert!(said.contains("would cover every branch"), "{said}");
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
