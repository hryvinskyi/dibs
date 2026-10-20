//! Every change of the slots' state: a pure function of the board, the declared slots, a request
//! and the time, answering the new board and the events it records.
//!
//! The rules every transition keeps: one hold per group at a time; a member waits in a group's
//! queue at most once; when a group's hold ends in any way and nobody is on offer, the first waiter
//! is offered the slot they asked for; while a group is on offer only the offered member takes it;
//! a renewal never moves an expiry past `max_hold` from now, nor, while somebody waits, past twice
//! `max_hold` from when the hold was taken; a stalled hold offers nothing and does not expire.

use thiserror::Error;
use time::OffsetDateTime;

use crate::event::{
    EventKind, ForceCause, SlotForceReleased, SlotMember, SlotQueued, SlotReleased,
    SlotStallTakenOver, SlotTaken,
};
use crate::ids::RunId;
use crate::ids::{ItemId, MemberId, SessionId, WaiterId};
use crate::session::HostName;
use crate::slot::spec::{GroupKey, Purpose, SlotCatalog, SlotName, SlotSpec};
use crate::slot::state::{
    CommitSha, EndWord, Liveness, RunExit, STALL_AFTER, SlotBoard, SlotHold, SlotOffer, SlotWaiter,
    WorkflowName,
};

/// The board after a transition, the events it records, and its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition<A> {
    /// The new state.
    pub board: SlotBoard,
    /// One event per change, in the order the changes were made.
    pub events: Vec<EventKind>,
    /// What the transition answers its caller.
    pub answer: A,
}

/// Why a slot request is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SlotRefused {
    /// The settings declare no such slot.
    #[error("the project declares no slot {slot}")]
    UnknownSlot {
        /// The slot named.
        slot: SlotName,
    },
    /// The member holds another slot of the same group.
    #[error("you hold {held}, which excludes this slot; release it first")]
    HoldsOther {
        /// The slot they hold.
        held: SlotName,
    },
    /// Nobody holds the slot.
    #[error("nobody holds this slot")]
    NotHeld,
    /// Another member holds it.
    #[error("only the member holding this slot may do that")]
    NotHolder {
        /// The holder.
        member: MemberId,
    },
    /// The member holds it, but not through the run that asked: the run's hold ended.
    #[error("this run no longer holds the slot")]
    RunEnded,
    /// The member is neither waiting for the slot's group nor offered it.
    #[error("you are not waiting for this slot")]
    NotWaiting,
    /// The stalled hold is the member's own; taking the slot again renews it.
    #[error("this hold is yours; take the slot again to renew it")]
    OwnHold,
    /// The hold is not stalled.
    #[error("the hold is not stalled; message its holder instead")]
    NotStalled {
        /// The holder.
        member: MemberId,
        /// When it was last seen.
        last_seen: OffsetDateTime,
    },
}

/// What a run's heartbeat learns when its hold is gone.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BeatRefused {
    /// Another member holds the slot now.
    #[error("another member took the slot over")]
    TakenOver {
        /// The member holding it now.
        by: MemberId,
    },
    /// The hold the run had ended: released, expired, ended by an admin or renewed by another run.
    #[error("the hold this run had has ended")]
    Ended,
}

/// A member asking for a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeRequest {
    /// The slot.
    pub slot: SlotName,
    /// The member asking.
    pub member: MemberId,
    /// Their session.
    pub session: SessionId,
    /// That session's host.
    pub host: HostName,
    /// Why.
    pub purpose: Purpose,
    /// The work item, when one is named.
    pub item: Option<ItemId>,
    /// How the hold would stay alive.
    pub liveness: Liveness,
    /// The place the member gets if they must wait.
    pub waiter: WaiterId,
}

/// What a take came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeAnswer {
    /// The member holds the slot now.
    Taken {
        /// The hold.
        hold: SlotHold,
    },
    /// The member held it already; its expiry moved.
    Renewed {
        /// The hold.
        hold: SlotHold,
    },
    /// The member waits.
    Queued {
        /// Their place, from 1.
        position: usize,
        /// The group's hold they wait behind, when there is one.
        hold: Option<SlotHold>,
        /// The group's offer to another member, when there is one.
        offer: Option<SlotOffer>,
    },
}

/// Applies what time alone changes: a run's hold without heartbeats stalls, a hold past its expiry
/// ends, an offer past its deadline lapses and its member leaves the queue, a waiter silent for
/// longer than `max_wait` loses their place; then every free group is offered to its first waiter.
/// Rows of slots the settings no longer declare are left as they are.
pub fn decide_tick(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    now: OffsetDateTime,
) -> Transition<()> {
    let mut next = board.clone();
    let mut events = Vec::new();
    next.holds.retain_mut(|hold| {
        if catalog.get(&hold.slot).is_none() {
            return true;
        }
        let silent_run = matches!(hold.liveness, Liveness::Run { .. })
            && !hold.is_stalled()
            && now - hold.last_seen > STALL_AFTER;
        if silent_run {
            hold.stalled_at = Some(now);
            events.push(EventKind::SlotStalled(slot_member(
                hold.slot.clone(),
                hold.member,
            )));
            return true;
        }
        if !hold.is_stalled() && now >= hold.expires_at {
            events.push(EventKind::SlotExpired(slot_member(
                hold.slot.clone(),
                hold.member,
            )));
            return false;
        }
        true
    });
    next.offers.retain(|offer| {
        if catalog.get(&offer.slot).is_none() || now < offer.until {
            return true;
        }
        events.push(EventKind::SlotOfferLapsed(slot_member(
            offer.slot.clone(),
            offer.member,
        )));
        false
    });
    next.waiters.retain(|waiter| {
        let Some(spec) = catalog.get(&waiter.slot) else {
            return true;
        };
        if now - waiter.last_seen <= spec.max_wait {
            return true;
        }
        events.push(EventKind::SlotLeft(slot_member(
            waiter.slot.clone(),
            waiter.member,
        )));
        false
    });
    settle(&mut next, catalog, now, &mut events);
    Transition {
        board: next,
        events,
        answer: (),
    }
}

/// Decides a take: the free slot, or the slot on offer to the member, is theirs; their own hold is
/// renewed; otherwise they wait. Taking again while waiting keeps the place, refreshes it and
/// records the slot and purpose asked for now.
///
/// # Errors
/// [`SlotRefused::UnknownSlot`]; [`SlotRefused::HoldsOther`] when the member holds another slot of
/// the group.
pub fn decide_take(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    request: &TakeRequest,
    now: OffsetDateTime,
) -> Result<Transition<TakeAnswer>, SlotRefused> {
    let spec = declared(catalog, &request.slot)?;
    let key = spec.group_key();
    let mut next = board.clone();
    let mut events = Vec::new();
    if let Some(index) = next
        .holds
        .iter()
        .position(|hold| hold.slot == request.slot && hold.member == request.member)
    {
        let waiting = !board.group_queue(catalog, &key).is_empty();
        let hold = &mut next.holds[index];
        let mut expires_at = now + spec.max_hold;
        if waiting {
            expires_at = expires_at.min(hold.taken_at + spec.max_hold * 2);
        }
        if hold.is_stalled() {
            events.push(EventKind::SlotResumed(slot_member(
                hold.slot.clone(),
                hold.member,
            )));
        }
        hold.expires_at = hold.expires_at.max(expires_at);
        hold.liveness = hold.liveness.renewed(&request.liveness);
        hold.session = request.session;
        hold.host = request.host.clone();
        hold.purpose = request.purpose.clone();
        hold.item = request.item.or(hold.item);
        hold.last_seen = now;
        hold.stalled_at = None;
        let hold = hold.clone();
        events.push(EventKind::SlotRenewed(slot_member(
            hold.slot.clone(),
            hold.member,
        )));
        return Ok(Transition {
            board: next,
            events,
            answer: TakeAnswer::Renewed { hold },
        });
    }
    let group_hold = board.group_holds(catalog, &key).next();
    if let Some(mine) = board
        .group_holds(catalog, &key)
        .find(|hold| hold.member == request.member)
    {
        return Err(SlotRefused::HoldsOther {
            held: mine.slot.clone(),
        });
    }
    let offer = board.group_offer(catalog, &key);
    let offered_to_me = offer.is_some_and(|offer| offer.member == request.member);
    if group_hold.is_none() && (offer.is_none() || offered_to_me) {
        next.offers
            .retain(|offer| !catalog.in_group(&offer.slot, &key));
        next.waiters.retain(|waiter| {
            !(waiter.member == request.member && catalog.in_group(&waiter.slot, &key))
        });
        let hold = new_hold(request, spec, now);
        next.holds.push(hold.clone());
        events.push(EventKind::SlotTaken(SlotTaken {
            slot: hold.slot.clone(),
            member: hold.member,
            purpose: hold.purpose.clone(),
        }));
        return Ok(Transition {
            board: next,
            events,
            answer: TakeAnswer::Taken { hold },
        });
    }
    match next
        .waiters
        .iter_mut()
        .find(|waiter| waiter.member == request.member && catalog.in_group(&waiter.slot, &key))
    {
        Some(waiter) => {
            waiter.slot = request.slot.clone();
            waiter.purpose = request.purpose.clone();
            waiter.last_seen = now;
        }
        None => {
            next.waiters.push(SlotWaiter {
                id: request.waiter,
                slot: request.slot.clone(),
                member: request.member,
                purpose: request.purpose.clone(),
                since: now,
                last_seen: now,
            });
            events.push(EventKind::SlotQueued(SlotQueued {
                slot: request.slot.clone(),
                member: request.member,
                purpose: request.purpose.clone(),
            }));
        }
    }
    let position = next
        .group_queue(catalog, &key)
        .iter()
        .position(|waiter| waiter.member == request.member)
        .map_or(1, |index| index + 1);
    Ok(Transition {
        answer: TakeAnswer::Queued {
            position,
            hold: group_hold.cloned(),
            offer: offer.cloned(),
        },
        board: next,
        events,
    })
}

/// Decides the holder's release, by the run that holds the slot when `run` names one; the slot is
/// offered to the group's first waiter.
///
/// # Errors
/// [`SlotRefused::NotHeld`], [`SlotRefused::NotHolder`], or [`SlotRefused::RunEnded`] when `run`
/// no longer holds it.
pub fn decide_release(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    slot: &SlotName,
    member: MemberId,
    run: Option<RunId>,
    exit: Option<RunExit>,
    now: OffsetDateTime,
) -> Result<Transition<SlotHold>, SlotRefused> {
    let hold = board.hold(slot).ok_or(SlotRefused::NotHeld)?;
    if hold.member != member {
        return Err(SlotRefused::NotHolder {
            member: hold.member,
        });
    }
    if run.is_some_and(|run| !hold.is_run(run)) {
        return Err(SlotRefused::RunEnded);
    }
    let event = EventKind::SlotReleased(SlotReleased {
        slot: slot.clone(),
        member,
        exit,
    });
    Ok(end_hold(board, catalog, hold, event, now))
}

/// Decides a waiter leaving their group's queue, or an offered member giving up the offer, which
/// passes to the next waiter.
///
/// # Errors
/// [`SlotRefused::UnknownSlot`]; [`SlotRefused::NotWaiting`].
pub fn decide_leave(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    slot: &SlotName,
    member: MemberId,
    now: OffsetDateTime,
) -> Result<Transition<()>, SlotRefused> {
    let key = declared(catalog, slot)?.group_key();
    let mut next = board.clone();
    let mut left = None;
    next.waiters.retain(|waiter| {
        let mine = waiter.member == member && catalog.in_group(&waiter.slot, &key);
        if mine {
            left = Some(waiter.slot.clone());
        }
        !mine
    });
    next.offers.retain(|offer| {
        let mine = offer.member == member && catalog.in_group(&offer.slot, &key);
        if mine {
            left = Some(offer.slot.clone());
        }
        !mine
    });
    let Some(left) = left else {
        return Err(SlotRefused::NotWaiting);
    };
    let mut events = vec![EventKind::SlotLeft(slot_member(left, member))];
    settle(&mut next, catalog, now, &mut events);
    Ok(Transition {
        board: next,
        events,
        answer: (),
    })
}

/// Decides an admin ending a hold; the slot is offered to the group's first waiter.
///
/// # Errors
/// [`SlotRefused::NotHeld`].
pub fn decide_force_release(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    slot: &SlotName,
    by: MemberId,
    now: OffsetDateTime,
) -> Result<Transition<SlotHold>, SlotRefused> {
    let hold = board.hold(slot).ok_or(SlotRefused::NotHeld)?;
    let event = EventKind::SlotForceReleased(SlotForceReleased {
        slot: slot.clone(),
        member: hold.member,
        by: Some(by),
        cause: ForceCause::Admin,
    });
    Ok(end_hold(board, catalog, hold, event, now))
}

/// Decides a heartbeat of the run holding a slot: it is seen now, and a stalled hold is live again.
///
/// # Errors
/// [`BeatRefused`] when the run's hold is gone.
pub fn decide_run_beat(
    board: &SlotBoard,
    slot: &SlotName,
    member: MemberId,
    run: RunId,
    now: OffsetDateTime,
) -> Result<Transition<SlotHold>, BeatRefused> {
    let mut next = board.clone();
    let Some(hold) = next.holds.iter_mut().find(|hold| hold.slot == *slot) else {
        return Err(BeatRefused::Ended);
    };
    if hold.member != member {
        return Err(BeatRefused::TakenOver { by: hold.member });
    }
    if !hold.is_run(run) {
        return Err(BeatRefused::Ended);
    }
    let mut events = Vec::new();
    if hold.is_stalled() {
        hold.stalled_at = None;
        events.push(EventKind::SlotResumed(slot_member(slot.clone(), member)));
    }
    hold.last_seen = now;
    let hold = hold.clone();
    Ok(Transition {
        board: next,
        events,
        answer: hold,
    })
}

/// Decides another member taking over a stalled hold. The new hold is the requester's; the queue
/// keeps its order.
///
/// # Errors
/// [`SlotRefused::UnknownSlot`], [`SlotRefused::NotHeld`], [`SlotRefused::OwnHold`], or
/// [`SlotRefused::NotStalled`] for a live hold.
pub fn decide_stall_takeover(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    request: &TakeRequest,
    now: OffsetDateTime,
) -> Result<Transition<SlotHold>, SlotRefused> {
    let spec = declared(catalog, &request.slot)?;
    let previous = board.hold(&request.slot).ok_or(SlotRefused::NotHeld)?;
    if previous.member == request.member {
        return Err(SlotRefused::OwnHold);
    }
    if !previous.is_stalled() {
        return Err(SlotRefused::NotStalled {
            member: previous.member,
            last_seen: previous.last_seen,
        });
    }
    let mut next = board.clone();
    next.holds.retain(|hold| hold.slot != request.slot);
    next.waiters.retain(|waiter| {
        !(waiter.member == request.member && catalog.in_group(&waiter.slot, &spec.group_key()))
    });
    let hold = new_hold(request, spec, now);
    next.holds.push(hold);
    Ok(Transition {
        board: next,
        events: vec![EventKind::SlotStallTakenOver(SlotStallTakenOver {
            slot: request.slot.clone(),
            member: request.member,
            previous: previous.member,
            purpose: request.purpose.clone(),
        })],
        answer: previous.clone(),
    })
}

/// Decides the forge's report that a workflow run finished: the hold tied to exactly that workflow
/// and commit ends, with the conclusion as its exit. Any other report changes nothing, so a repeat
/// answers the same as the first: `None`.
pub fn decide_run_finished(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    slot: &SlotName,
    workflow: &WorkflowName,
    sha: &CommitSha,
    conclusion: &EndWord,
    now: OffsetDateTime,
) -> Transition<Option<SlotHold>> {
    let tied = board.hold(slot).filter(|hold| {
        hold.liveness
            == Liveness::UntilRun {
                workflow: workflow.clone(),
                sha: sha.clone(),
            }
    });
    match tied {
        Some(hold) => {
            let event = EventKind::SlotReleased(SlotReleased {
                slot: slot.clone(),
                member: hold.member,
                exit: Some(RunExit::Conclusion(conclusion.clone())),
            });
            let ended = end_hold(board, catalog, hold, event, now);
            Transition {
                board: ended.board,
                events: ended.events,
                answer: Some(ended.answer),
            }
        }
        None => Transition {
            board: board.clone(),
            events: Vec::new(),
            answer: None,
        },
    }
}

/// Ends every hold, place and offer of a slot that `catalog` no longer declares, each with an event
/// naming `by`, then offers every free group to its first waiter.
pub fn decide_slots_removed(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    by: MemberId,
    now: OffsetDateTime,
) -> Transition<()> {
    let mut next = board.clone();
    let mut events = Vec::new();
    let mut removed = |slot: &SlotName, member: MemberId| {
        let gone = catalog.get(slot).is_none();
        if gone {
            events.push(EventKind::SlotForceReleased(SlotForceReleased {
                slot: slot.clone(),
                member,
                by: Some(by),
                cause: ForceCause::SlotRemoved,
            }));
        }
        !gone
    };
    next.holds.retain(|hold| removed(&hold.slot, hold.member));
    next.offers
        .retain(|offer| removed(&offer.slot, offer.member));
    next.waiters
        .retain(|waiter| removed(&waiter.slot, waiter.member));
    settle(&mut next, catalog, now, &mut events);
    Transition {
        board: next,
        events,
        answer: (),
    }
}

/// What removing a member from a project took from them among the slots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemovedFromSlots {
    /// The slots they held.
    pub held: Vec<SlotName>,
    /// The slots they waited for.
    pub waited: Vec<SlotName>,
    /// The slots on offer to them.
    pub offered: Vec<SlotName>,
}

impl RemovedFromSlots {
    /// Whether they had nothing among the slots.
    pub fn is_empty(&self) -> bool {
        self.held.is_empty() && self.waited.is_empty() && self.offered.is_empty()
    }
}

/// Ends every hold, place and offer of `member`, each with an event naming `by` and the member's
/// removal as the cause, then offers every freed group to its first waiter.
pub fn decide_member_removed(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    member: MemberId,
    by: Option<MemberId>,
    now: OffsetDateTime,
) -> Transition<RemovedFromSlots> {
    let mut next = board.clone();
    let mut events = Vec::new();
    let mut removed = RemovedFromSlots::default();
    let mut end = |slot: &SlotName, list: &mut Vec<SlotName>| {
        events.push(EventKind::SlotForceReleased(SlotForceReleased {
            slot: slot.clone(),
            member,
            by,
            cause: ForceCause::MemberRemoved,
        }));
        list.push(slot.clone());
    };
    next.holds.retain(|hold| {
        let theirs = hold.member == member;
        if theirs {
            end(&hold.slot, &mut removed.held);
        }
        !theirs
    });
    next.offers.retain(|offer| {
        let theirs = offer.member == member;
        if theirs {
            end(&offer.slot, &mut removed.offered);
        }
        !theirs
    });
    next.waiters.retain(|waiter| {
        let theirs = waiter.member == member;
        if theirs {
            end(&waiter.slot, &mut removed.waited);
        }
        !theirs
    });
    settle(&mut next, catalog, now, &mut events);
    Transition {
        board: next,
        events,
        answer: removed,
    }
}

fn declared<'a>(catalog: &'a SlotCatalog, slot: &SlotName) -> Result<&'a SlotSpec, SlotRefused> {
    catalog
        .get(slot)
        .ok_or_else(|| SlotRefused::UnknownSlot { slot: slot.clone() })
}

fn slot_member(slot: SlotName, member: MemberId) -> SlotMember {
    SlotMember { slot, member }
}

fn new_hold(request: &TakeRequest, spec: &SlotSpec, now: OffsetDateTime) -> SlotHold {
    SlotHold {
        slot: request.slot.clone(),
        member: request.member,
        session: request.session,
        host: request.host.clone(),
        purpose: request.purpose.clone(),
        item: request.item,
        liveness: request.liveness.clone(),
        taken_at: now,
        expires_at: now + spec.max_hold,
        last_seen: now,
        stalled_at: None,
    }
}

/// The board without `hold`, its end recorded by `event`, and the freed group offered on.
fn end_hold(
    board: &SlotBoard,
    catalog: &SlotCatalog,
    hold: &SlotHold,
    event: EventKind,
    now: OffsetDateTime,
) -> Transition<SlotHold> {
    let mut next = board.clone();
    next.holds.retain(|kept| kept.slot != hold.slot);
    let mut events = vec![event];
    settle(&mut next, catalog, now, &mut events);
    Transition {
        board: next,
        events,
        answer: hold.clone(),
    }
}

/// Offers every group that has no hold and no offer to its first waiter, in declaration order.
fn settle(
    board: &mut SlotBoard,
    catalog: &SlotCatalog,
    now: OffsetDateTime,
    events: &mut Vec<EventKind>,
) {
    for key in catalog.groups() {
        offer_first(board, catalog, &key, now, events);
    }
}

fn offer_first(
    board: &mut SlotBoard,
    catalog: &SlotCatalog,
    key: &GroupKey,
    now: OffsetDateTime,
    events: &mut Vec<EventKind>,
) {
    let busy = board.group_holds(catalog, key).next().is_some()
        || board.group_offer(catalog, key).is_some();
    if busy {
        return;
    }
    let Some(first) = board
        .group_queue(catalog, key)
        .first()
        .map(|waiter| waiter.id)
    else {
        return;
    };
    let Some(index) = board.waiters.iter().position(|waiter| waiter.id == first) else {
        return;
    };
    let waiter = board.waiters.remove(index);
    let Some(spec) = catalog.get(&waiter.slot) else {
        return;
    };
    events.push(EventKind::SlotOffered(slot_member(
        waiter.slot.clone(),
        waiter.member,
    )));
    board.offers.push(SlotOffer {
        until: now + spec.offer_for,
        slot: waiter.slot,
        member: waiter.member,
        purpose: waiter.purpose,
    });
}
