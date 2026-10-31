//! Which worktrees of a pool a take offers the server, best first.
//!
//! A worktree set aside as broken is never offered, nor one somebody still works in by this
//! machine's beats — a sign of life newer than its last release, younger than an agent's silence —
//! whatever the server says of it, nor one being prepared right now. Of the rest, a prepared one
//! whose environment runs comes first, then a prepared one whose environment was stopped, then,
//! where the take may prepare one, one not prepared yet; within each, the pool's own order.
//!
//! A preparation in the background builds a whole environment, so one runs at a time: none is
//! started while any worktree of the pool is being prepared, reset or stopped.

use dibs_domain::worktree::WorktreeName;

/// What this machine knows of one worktree when a take chooses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The worktree.
    pub name: WorktreeName,
    /// Whether it was prepared and is ready to be reset.
    pub prepared: bool,
    /// Whether its environment was stopped for being left idle.
    pub stopped: bool,
    /// Whether it is set aside as broken.
    pub broken: bool,
    /// Whether somebody other than the taker still works in it by this machine's beats.
    pub in_use: bool,
    /// Whether another run holds its lock: it is being prepared, reset or stopped.
    pub locked: bool,
}

/// The rank of a worktree a take may offer; `None` for one it may not.
fn rank(facts: &Facts, may_prepare: bool) -> Option<u8> {
    if facts.broken || facts.in_use {
        return None;
    }
    match (facts.prepared, facts.stopped) {
        (true, false) => Some(0),
        (true, true) => Some(1),
        (false, _) if may_prepare && !facts.locked => Some(2),
        (false, _) => None,
    }
}

/// The worktrees a take offers the server, best first; unprepared ones only when `may_prepare`.
pub fn candidates(facts: &[Facts], may_prepare: bool) -> Vec<WorktreeName> {
    let mut ranked: Vec<(u8, usize, &WorktreeName)> = facts
        .iter()
        .enumerate()
        .filter_map(|(at, facts)| rank(facts, may_prepare).map(|rank| (rank, at, &facts.name)))
        .collect();
    ranked.sort();
    ranked
        .into_iter()
        .map(|(_, _, name)| name.clone())
        .collect()
}

/// The first worktree a take under a deadline may have prepared in the background: not prepared,
/// not broken and not in use; none while any worktree of `facts` is locked.
pub fn to_prepare(facts: &[Facts]) -> Option<WorktreeName> {
    if facts.iter().any(|facts| facts.locked) {
        return None;
    }
    facts
        .iter()
        .find(|facts| !facts.prepared && !facts.broken && !facts.in_use && !facts.locked)
        .map(|facts| facts.name.clone())
}

/// The worktrees not prepared yet that a take under a deadline is waiting on rather than refused
/// by: neither broken nor in use, whether a preparation of them runs already or not.
pub fn unprepared(facts: &[Facts]) -> Vec<&Facts> {
    facts
        .iter()
        .filter(|facts| !facts.prepared && !facts.broken && !facts.in_use)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree(name: &str) -> Facts {
        Facts {
            name: WorktreeName::parse(name).unwrap(),
            prepared: true,
            stopped: false,
            broken: false,
            in_use: false,
            locked: false,
        }
    }

    fn names(list: &[WorktreeName]) -> Vec<&str> {
        list.iter().map(WorktreeName::as_str).collect()
    }

    #[test]
    fn running_come_before_stopped_and_both_before_unprepared() {
        let facts = [
            Facts {
                prepared: false,
                ..worktree("worktree1")
            },
            Facts {
                stopped: true,
                ..worktree("worktree2")
            },
            worktree("worktree3"),
            Facts {
                stopped: true,
                ..worktree("worktree4")
            },
            worktree("worktree5"),
        ];
        assert_eq!(
            names(&candidates(&facts, true)),
            [
                "worktree3",
                "worktree5",
                "worktree2",
                "worktree4",
                "worktree1"
            ]
        );
        assert_eq!(
            names(&candidates(&facts, false)),
            ["worktree3", "worktree5", "worktree2", "worktree4"],
            "a take that may not prepare offers prepared worktrees only"
        );
    }

    #[test]
    fn broken_in_use_and_ones_being_prepared_are_never_offered() {
        let facts = [
            Facts {
                broken: true,
                ..worktree("worktree1")
            },
            Facts {
                in_use: true,
                ..worktree("worktree2")
            },
            Facts {
                prepared: false,
                locked: true,
                ..worktree("worktree3")
            },
            Facts {
                locked: true,
                ..worktree("worktree4")
            },
        ];
        assert_eq!(
            names(&candidates(&facts, true)),
            ["worktree4"],
            "a prepared worktree being stopped is offered; the take waits for its lock"
        );
    }

    #[test]
    fn the_worktree_to_prepare_is_the_first_unprepared_one_that_is_neither_broken_nor_in_use() {
        let facts = [
            worktree("worktree1"),
            Facts {
                prepared: false,
                in_use: true,
                ..worktree("worktree2")
            },
            Facts {
                prepared: false,
                broken: true,
                ..worktree("worktree3")
            },
            Facts {
                prepared: false,
                ..worktree("worktree4")
            },
        ];
        assert_eq!(
            to_prepare(&facts).as_ref().map(WorktreeName::as_str),
            Some("worktree4")
        );
        assert_eq!(to_prepare(&facts[..3]), None);
    }

    #[test]
    fn no_preparation_starts_while_any_worktree_of_the_pool_is_locked() {
        let preparing = [
            Facts {
                prepared: false,
                locked: true,
                ..worktree("worktree1")
            },
            Facts {
                prepared: false,
                ..worktree("worktree2")
            },
        ];
        assert_eq!(to_prepare(&preparing), None, "one preparation at a time");
        let resetting = [
            Facts {
                locked: true,
                ..worktree("worktree1")
            },
            Facts {
                prepared: false,
                ..worktree("worktree2")
            },
        ];
        assert_eq!(to_prepare(&resetting), None);
    }

    #[test]
    fn unprepared_worktrees_are_those_neither_broken_nor_in_use_being_prepared_or_not() {
        let facts = [
            worktree("worktree1"),
            Facts {
                prepared: false,
                locked: true,
                ..worktree("worktree2")
            },
            Facts {
                prepared: false,
                broken: true,
                ..worktree("worktree3")
            },
            Facts {
                prepared: false,
                in_use: true,
                ..worktree("worktree4")
            },
            Facts {
                prepared: false,
                ..worktree("worktree5")
            },
        ];
        let names: Vec<&str> = unprepared(&facts)
            .into_iter()
            .map(|facts| facts.name.as_str())
            .collect();
        assert_eq!(names, ["worktree2", "worktree5"]);
        assert!(
            unprepared(&facts[..1]).is_empty(),
            "a pool whose every worktree is prepared has none left to wait on"
        );
    }
}
