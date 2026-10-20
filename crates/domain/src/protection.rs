//! Protected refs: branches of a repository that only the members a project names may push to.
//! Everyone else's way to such a branch is a pull request that one of those members merges.

use std::collections::BTreeSet;

use crate::handle::Handle;
use crate::slot::RepoRef;

/// A branch only `pushers` may push to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedRef {
    /// The branch.
    pub target: RepoRef,
    /// The members who may push to it, by handle; none means nobody pushes to it directly.
    pub pushers: BTreeSet<Handle>,
}

impl ProtectedRef {
    /// Whether the member may push to the branch.
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

    #[test]
    fn only_a_listed_pusher_may_push_and_an_empty_list_lets_nobody() {
        let main = RepoRef::parse("api:refs/heads/main").unwrap();
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
}
