//! Every change of a pool: a pure function of the pool, a request and the time, answering the new
//! pool and the events it records.
//!
//! The rules every transition keeps: a worktree is held by one holder at most, and a holder holds
//! one worktree at most; a holder that asks again gets the worktree it holds; a hold's last sight
//! only moves forward; a hold ends when it is released, when its session ends, when its member
//! leaves the project, or when it has been silent for longer than its kind of holder may be.

use thiserror::Error;
use time::OffsetDateTime;

use crate::event::{EventKind, WorktreeReleaseCause, WorktreeReleased, WorktreeTaken};
use crate::ids::{ItemId, MemberId, SessionId};
use crate::session::HostName;
use crate::slot::Purpose;
use crate::worktree::name::WorktreeName;
use crate::worktree::state::{Holder, Silence, WorktreeHold, WorktreeHolder, WorktreePool};

/// The pool after a transition, the events it records, and its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition<A> {
    /// The new state.
    pub pool: WorktreePool,
    /// One event per change, in the order the changes were made.
    pub events: Vec<EventKind>,
    /// What the transition answers its caller.
    pub answer: A,
}

/// Why a worktree request is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorktreeRefused {
    /// A take named no worktree.
    #[error("name at least one worktree to take")]
    NoNames,
    /// A take named one worktree twice.
    #[error("the worktree {worktree} is named twice")]
    NamedTwice {
        /// The worktree.
        worktree: WorktreeName,
    },
    /// Nothing the release names is held.
    #[error("that worktree is not held")]
    NotHeld,
}

/// A holder asking for a worktree of a pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeWorktree {
    /// The worktrees that may be handed out, best first.
    pub names: Vec<WorktreeName>,
    /// Who asks.
    pub holder: Holder,
    /// The host of the holder's session.
    pub host: HostName,
    /// Why.
    pub purpose: Purpose,
    /// The work item, when one is named.
    pub item: Option<ItemId>,
}

/// What a take came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeAnswer {
    /// The holder holds this worktree; `again` when it held it before asking.
    Took {
        /// The worktree.
        worktree: WorktreeName,
        /// Whether the holder held it already.
        again: bool,
    },
    /// Every worktree named is held by someone else.
    Busy {
        /// Who holds each, in the order the take named them.
        held: Vec<WorktreeHolder>,
    },
}

/// What a release names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    /// Whatever worktree this holder holds.
    Holder(Holder),
    /// This worktree, whoever holds it.
    Name(WorktreeName),
}

impl Release {
    /// Whether it names this hold.
    pub fn names(&self, hold: &WorktreeHold) -> bool {
        match self {
            Self::Holder(holder) => hold.holder == *holder,
            Self::Name(worktree) => hold.worktree == *worktree,
        }
    }
}

/// One sign of life of a holder in a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeBeat {
    /// The worktree.
    pub worktree: WorktreeName,
    /// The holder seen in it.
    pub holder: Holder,
    /// When it was seen.
    pub seen_at: OffsetDateTime,
}

/// What a batch of beats came to: each named worktree and holder once, in the order first named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Beaten {
    /// The pairs that hold: their holds were seen.
    pub kept: Vec<WorktreeHolder>,
    /// The pairs that do not: the worktree is free or another holder has it.
    pub unknown: Vec<WorktreeHolder>,
}

/// Decides a take: the holder's own hold is answered again, seen now and otherwise unchanged;
/// otherwise the first name in the order given that nobody holds becomes the holder's; otherwise
/// the pool is busy.
///
/// # Errors
/// [`WorktreeRefused::NoNames`] for an empty list; [`WorktreeRefused::NamedTwice`] when a name
/// repeats.
pub fn decide_take(
    pool: &WorktreePool,
    request: &TakeWorktree,
    now: OffsetDateTime,
) -> Result<Transition<TakeAnswer>, WorktreeRefused> {
    if request.names.is_empty() {
        return Err(WorktreeRefused::NoNames);
    }
    for (index, name) in request.names.iter().enumerate() {
        if request.names[..index].contains(name) {
            return Err(WorktreeRefused::NamedTwice {
                worktree: name.clone(),
            });
        }
    }
    let mut next = pool.clone();
    if let Some(held) = pool.hold_of(&request.holder) {
        if let Some(hold) = next.hold_mut(&held.worktree) {
            hold.last_seen = hold.last_seen.max(now);
        }
        return Ok(Transition {
            pool: next,
            events: Vec::new(),
            answer: TakeAnswer::Took {
                worktree: held.worktree.clone(),
                again: true,
            },
        });
    }
    let Some(free) = request.names.iter().find(|name| pool.hold(name).is_none()) else {
        let held = request
            .names
            .iter()
            .filter_map(|name| pool.hold(name).map(WorktreeHold::held))
            .collect();
        return Ok(Transition {
            pool: next,
            events: Vec::new(),
            answer: TakeAnswer::Busy { held },
        });
    };
    let hold = WorktreeHold {
        worktree: free.clone(),
        holder: request.holder.clone(),
        host: request.host.clone(),
        purpose: request.purpose.clone(),
        item: request.item,
        taken_at: now,
        last_seen: now,
    };
    let event = EventKind::WorktreeTaken(WorktreeTaken {
        member: pool.key().member,
        host: hold.host.clone(),
        worktree: hold.worktree.clone(),
        holder: hold.holder.kind(),
        purpose: hold.purpose.clone(),
    });
    next.insert(hold);
    Ok(Transition {
        pool: next,
        events: vec![event],
        answer: TakeAnswer::Took {
            worktree: free.clone(),
            again: false,
        },
    })
}

/// Decides a release by `by`: the pool's member, or an admin ending another member's hold. Who may
/// release is the caller's to check; the event names `by` only when it is not the pool's member.
///
/// # Errors
/// [`WorktreeRefused::NotHeld`] when the worktree, or the holder, holds nothing.
pub fn decide_release(
    pool: &WorktreePool,
    release: &Release,
    by: MemberId,
) -> Result<Transition<WorktreeHold>, WorktreeRefused> {
    let mut next = pool.clone();
    let mut ended = next.remove_where(|hold| release.names(hold));
    let Some(hold) = ended.pop() else {
        return Err(WorktreeRefused::NotHeld);
    };
    let by = (by != pool.key().member).then_some(by);
    let events = vec![released(pool, &hold, WorktreeReleaseCause::Released, by)];
    Ok(Transition {
        pool: next,
        events,
        answer: hold,
    })
}

/// Decides a batch of beats: a hold is seen at a beat's time when the beat names its worktree and
/// its holder and the time is later than its last sight. A beat for a free worktree, or for a
/// worktree another holder has, changes nothing and is answered as unknown, so the machine that
/// sent it can take the worktree again.
pub fn decide_beat(pool: &WorktreePool, beats: &[WorktreeBeat]) -> Transition<Beaten> {
    let mut next = pool.clone();
    let mut answer = Beaten::default();
    for beat in beats {
        let pair = WorktreeHolder {
            worktree: beat.worktree.clone(),
            holder: beat.holder.clone(),
        };
        let held = next
            .hold_mut(&beat.worktree)
            .filter(|hold| hold.holder == beat.holder);
        let list = match held {
            Some(hold) => {
                hold.last_seen = hold.last_seen.max(beat.seen_at);
                &mut answer.kept
            }
            None => &mut answer.unknown,
        };
        if !list.contains(&pair) {
            list.push(pair);
        }
    }
    Transition {
        pool: next,
        events: Vec::new(),
        answer,
    }
}

/// Ends every hold silent for longer than `silence` allows its kind of holder at `now`; a hold
/// silent exactly that long stays. Answers the holds that ended.
pub fn decide_tick(
    pool: &WorktreePool,
    now: OffsetDateTime,
    silence: Silence,
) -> Transition<Vec<WorktreeHold>> {
    end_where(
        pool,
        |hold| hold.is_silent(now, silence),
        WorktreeReleaseCause::Silent,
        None,
    )
}

/// Ends every hold of `session`: its own and its agents'. Answers the holds that ended.
pub fn decide_session_ended(
    pool: &WorktreePool,
    session: SessionId,
) -> Transition<Vec<WorktreeHold>> {
    end_where(
        pool,
        |hold| hold.holder.session() == session,
        WorktreeReleaseCause::SessionEnded,
        None,
    )
}

/// Ends every hold of the pool, its member having been removed from the project by `by` (`None`
/// for the service's operator). Answers the holds that ended.
pub fn decide_member_removed(
    pool: &WorktreePool,
    by: Option<MemberId>,
) -> Transition<Vec<WorktreeHold>> {
    end_where(pool, |_| true, WorktreeReleaseCause::MemberRemoved, by)
}

fn end_where(
    pool: &WorktreePool,
    ends: impl FnMut(&WorktreeHold) -> bool,
    cause: WorktreeReleaseCause,
    by: Option<MemberId>,
) -> Transition<Vec<WorktreeHold>> {
    let mut next = pool.clone();
    let ended = next.remove_where(ends);
    let events = ended
        .iter()
        .map(|hold| released(pool, hold, cause, by))
        .collect();
    Transition {
        pool: next,
        events,
        answer: ended,
    }
}

fn released(
    pool: &WorktreePool,
    hold: &WorktreeHold,
    cause: WorktreeReleaseCause,
    by: Option<MemberId>,
) -> EventKind {
    EventKind::WorktreeReleased(WorktreeReleased {
        member: pool.key().member,
        host: hold.host.clone(),
        worktree: hold.worktree.clone(),
        holder: hold.holder.kind(),
        purpose: hold.purpose.clone(),
        cause,
        by,
    })
}
