//! Taking an area from debt work: a feature claim that declares an area another member's debt claim
//! holds is not refused; the area waits on the feature claim, pending, and the debt holder is asked
//! to hand it over.
//!
//! A claim is of one of two kinds. Work on a feature takes precedence over paying off debt, so a
//! [`ClaimKind::Feature`] claim may take an area a [`ClaimKind::Debt`] claim holds, and nothing else
//! may take an area anyone else holds. The area is handed over as a transition, never as a hope:
//! the moment the debt claim no longer holds it — narrowed, ended, gone stale or taken over — it
//! moves to the feature claim. While the debt claim is in review its files sit in pull requests
//! under review, so it is not asked until it is worked on again. A request nobody answers within the
//! project's `yield_escalates_after` goes to the project's arbiters: an area is never taken
//! silently. An area moves only when no other member's live claim holds it any more: one member
//! holding it under several claims lets it go when the last of them does.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::area::QualifiedArea;
use crate::claim::Claim;
use crate::ids::{ItemId, ThreadId};

/// What a claim's work is: a feature, or paying off debt. Serialised as `feature` or `debt`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    /// Work on a feature: it may take an area from debt work.
    #[default]
    Feature,
    /// Debt work only: refactoring, clean-up, paying off what was left behind. It gives its areas
    /// up to feature work that needs them.
    Debt,
}

/// Why a text is not a claim kind.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClaimKindError {
    /// The text is neither `feature` nor `debt`.
    #[error("a claim is of the kind 'feature' or 'debt'")]
    Unknown,
}

impl ClaimKind {
    /// The stable name: `feature` or `debt`.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::Debt => "debt",
        }
    }

    /// The kind of this name.
    ///
    /// # Errors
    /// [`ClaimKindError::Unknown`] for any other text.
    pub fn parse(text: &str) -> Result<Self, ClaimKindError> {
        match text {
            "feature" => Ok(Self::Feature),
            "debt" => Ok(Self::Debt),
            _ => Err(ClaimKindError::Unknown),
        }
    }

    /// Whether a claim of this kind that declares an area a live claim of `holder`'s kind holds
    /// waits for that claim to hand it over, rather than being refused: only a feature waits, and
    /// only for debt work.
    pub const fn takes_from(self, holder: Self) -> bool {
        matches!((self, holder), (Self::Feature, Self::Debt))
    }
}

/// An area a claim declared and waits to receive from the debt claim that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingArea {
    /// The item of the debt claim it waits for.
    pub from: ItemId,
    /// When that claim's holder was asked to hand it over; `None` while they have not been, as
    /// while their claim is in review.
    pub asked_at: Option<OffsetDateTime>,
    /// When the unanswered request was escalated to the arbiters; `None` until it was.
    pub escalated_at: Option<OffsetDateTime>,
    /// The conversation that asks the debt claim's holder for it, shared by every area the claim
    /// waits for from that same debt claim; `None` until one was started.
    pub request: Option<ThreadId>,
}

impl PendingArea {
    /// An area that waits for the debt claim on `from`, its holder not asked yet.
    pub const fn waiting_for(from: ItemId) -> Self {
        Self {
            from,
            asked_at: None,
            escalated_at: None,
            request: None,
        }
    }
}

/// An area on its way from a debt claim to a feature claim.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AreaMove {
    /// The area.
    pub area: QualifiedArea,
    /// The item of the debt claim that holds it, or held it.
    pub from: ItemId,
    /// The item of the feature claim that waits for it.
    pub to: ItemId,
}

/// What one feature claim waits for from one debt claim, asked or escalated together: one request,
/// in one conversation, per pair of claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YieldPair {
    /// The item of the debt claim.
    pub from: ItemId,
    /// The item of the feature claim.
    pub to: ItemId,
    /// The areas concerned.
    pub areas: BTreeSet<QualifiedArea>,
    /// The conversation already asking for areas of this pair, when one was started.
    pub request: Option<ThreadId>,
}

impl YieldPair {
    /// Each area of the pair as a move.
    pub fn moves(&self) -> impl Iterator<Item = AreaMove> + '_ {
        self.areas.iter().map(|area| AreaMove {
            area: area.clone(),
            from: self.from,
            to: self.to,
        })
    }
}

/// Why a claim stopped waiting for an area it was not handed. Serialised as `stale`, `narrowed`,
/// `holder_changed` or `ended`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithdrawReason {
    /// The waiting claim went stale.
    Stale,
    /// Its holder gave the area up.
    Narrowed,
    /// The area is held now by a claim that a feature does not take from.
    HolderChanged,
    /// The waiting claim ended: released, completed, ended by an admin or with its member's
    /// removal, or completed by a merge.
    Ended,
}

impl WithdrawReason {
    /// The stable name.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::Narrowed => "narrowed",
            Self::HolderChanged => "holder_changed",
            Self::Ended => "ended",
        }
    }
}

/// A wait that ended without a handover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Withdrawal {
    /// The area and the two claims.
    pub moved: AreaMove,
    /// Why.
    pub reason: WithdrawReason,
    /// The conversation that asked for it, when one did.
    pub request: Option<ThreadId>,
}

/// What the pending areas of a project's claims call for now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settlement {
    /// The claims to store, each once, ordered by item, as the settlement leaves them.
    pub changed: Vec<Claim>,
    /// The areas handed over: each is held by its feature claim now, and no longer pending there
    /// or held by the debt claim.
    pub handed_over: Vec<AreaMove>,
    /// What to ask now, one request per pair of claims; each area is recorded as asked at the time
    /// of the settlement.
    pub asked: Vec<YieldPair>,
    /// The waits that ended without a handover.
    pub withdrawn: Vec<Withdrawal>,
}

impl Settlement {
    /// Whether nothing is due.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
    }
}

/// The live claims of members other than `waiting`'s that hold the area, lowest item first.
fn other_holders<'a>(
    area: &QualifiedArea,
    waiting: &Claim,
    claims: &'a [Claim],
    now: OffsetDateTime,
    stale_after: Duration,
) -> Vec<&'a Claim> {
    let mut holders: Vec<&Claim> = claims
        .iter()
        .filter(|claim| {
            claim.item != waiting.item
                && claim.member != waiting.member
                && claim.is_live(now, stale_after)
                && claim.holds(area)
        })
        .collect();
    holders.sort_by_key(|claim| claim.item);
    holders
}

/// Settles every pending area of `claims` at `now`:
///
/// - a feature claim gone stale stops waiting (`stale`): nothing is handed over to it or asked
///   for it;
/// - an area no live claim of another member holds any more — the debt claim narrowed it away,
///   ended, went stale or was taken over, and no other claim of any member holds it — is handed
///   over to the claim waiting for it, and the debt claim it waited for, when it still lists it,
///   drops it;
/// - an area another member's live claim of a kind the waiting claim does not take from holds
///   ends the wait (`holder_changed`);
/// - an area only debt claims hold waits for the one it waited for while that one still holds
///   it, and otherwise for the lowest of them, asked afresh;
/// - an area its debt claim holds and works on, not asked for yet, is asked for, together with the
///   other areas the same claim waits for from the same debt claim;
/// - an area whose debt claim went into review after it was asked for is no longer counted as
///   asked, so that it is asked again, with a fresh wait before an escalation, once that claim is
///   worked on again.
pub fn settle_pending(claims: &[Claim], now: OffsetDateTime, stale_after: Duration) -> Settlement {
    let mut working: BTreeMap<ItemId, Claim> = BTreeMap::new();
    let mut settlement = Settlement::default();
    let mut asks: BTreeMap<(ItemId, ItemId), BTreeSet<QualifiedArea>> = BTreeMap::new();
    for waiting in claims.iter().filter(|claim| !claim.pending.is_empty()) {
        for (area, pending) in &waiting.pending {
            let step = Step {
                area,
                from: pending.from,
                to: waiting.item,
            };
            if !waiting.is_live(now, stale_after) {
                step.withdraw(&mut working, claims);
                settlement
                    .withdrawn
                    .push(step.withdrawal(WithdrawReason::Stale, pending));
                continue;
            }
            let holders = other_holders(area, waiting, claims, now, stale_after);
            if holders.is_empty() {
                let from = claims.iter().find(|claim| claim.item == pending.from);
                step.hand_over(&mut working, claims, from);
                settlement.handed_over.push(step.into_move());
                continue;
            }
            if holders
                .iter()
                .any(|holder| !waiting.kind.takes_from(holder.kind))
            {
                step.withdraw(&mut working, claims);
                settlement
                    .withdrawn
                    .push(step.withdrawal(WithdrawReason::HolderChanged, pending));
                continue;
            }
            let holder = holders
                .iter()
                .find(|holder| holder.item == pending.from)
                .or_else(|| holders.first())
                .copied();
            let Some(holder) = holder else {
                continue;
            };
            let mut entry = if holder.item == pending.from {
                pending.clone()
            } else {
                PendingArea::waiting_for(holder.item)
            };
            if holder.is_working() {
                if entry.asked_at.is_none() {
                    entry.asked_at = Some(now);
                    asks.entry((holder.item, waiting.item))
                        .or_default()
                        .insert(area.clone());
                }
            } else {
                entry.asked_at = None;
                entry.escalated_at = None;
            }
            if entry != *pending {
                step.edit(&mut working, claims, |stored| *stored = entry);
            }
        }
    }
    settlement.asked = asks
        .into_iter()
        .map(|((from, to), areas)| YieldPair {
            request: working
                .get(&to)
                .or_else(|| claims.iter().find(|claim| claim.item == to))
                .and_then(|claim| claim.request_from(from)),
            from,
            to,
            areas,
        })
        .collect();
    settlement.changed = working.into_values().collect();
    settlement
}

/// The requests to hand an area over that waited `after` or longer at `now` without the area
/// being handed over, each escalated once, grouped per pair of claims: the debt claim still holds
/// the area and works on it, and the feature claim waiting for it is live. Answers the claims to
/// store, ordered by item, each pending area recorded as escalated at `now`, and what to escalate.
pub fn escalations_due(
    claims: &[Claim],
    now: OffsetDateTime,
    stale_after: Duration,
    after: Duration,
) -> (Vec<Claim>, Vec<YieldPair>) {
    let mut working: BTreeMap<ItemId, Claim> = BTreeMap::new();
    let mut due: BTreeMap<(ItemId, ItemId), YieldPair> = BTreeMap::new();
    for waiting in claims
        .iter()
        .filter(|claim| claim.is_live(now, stale_after))
    {
        for (area, pending) in &waiting.pending {
            let Some(asked_at) = pending.asked_at else {
                continue;
            };
            let holding = claims.iter().any(|claim| {
                claim.item == pending.from
                    && claim.member != waiting.member
                    && claim.is_live(now, stale_after)
                    && claim.is_working()
                    && claim.holds(area)
            });
            if pending.escalated_at.is_none() && holding && now - asked_at >= after {
                let step = Step {
                    area,
                    from: pending.from,
                    to: waiting.item,
                };
                step.edit(&mut working, claims, |entry| entry.escalated_at = Some(now));
                due.entry((pending.from, waiting.item))
                    .or_insert_with(|| YieldPair {
                        from: pending.from,
                        to: waiting.item,
                        areas: BTreeSet::new(),
                        request: waiting.request_from(pending.from),
                    })
                    .areas
                    .insert(area.clone());
            }
        }
    }
    (working.into_values().collect(), due.into_values().collect())
}

/// The claim, other than the one on `item`, that waits for the area — live or stale, since an area
/// is pending for one claim at a time and a stale claim stops waiting only when its pending areas
/// are settled.
pub fn pending_elsewhere<'a, I>(area: &QualifiedArea, item: ItemId, claims: I) -> Option<&'a Claim>
where
    I: IntoIterator<Item = &'a Claim>,
{
    claims
        .into_iter()
        .find(|claim| claim.item != item && claim.pending.contains_key(area))
}

/// One pending area being settled.
struct Step<'a> {
    area: &'a QualifiedArea,
    from: ItemId,
    to: ItemId,
}

impl Step<'_> {
    /// The area moves to the waiting claim; the claim it waited for drops it, when it still lists
    /// it.
    fn hand_over(
        &self,
        working: &mut BTreeMap<ItemId, Claim>,
        claims: &[Claim],
        from: Option<&Claim>,
    ) {
        if let Some(waiting) = claim_mut(working, claims, self.to) {
            waiting.pending.remove(self.area);
            waiting.areas.insert(self.area.clone());
        }
        if from.is_some_and(|from| from.holds(self.area))
            && let Some(giving) = claim_mut(working, claims, self.from)
        {
            giving.areas.remove(self.area);
        }
    }

    /// The waiting claim stops waiting for the area.
    fn withdraw(&self, working: &mut BTreeMap<ItemId, Claim>, claims: &[Claim]) {
        if let Some(waiting) = claim_mut(working, claims, self.to) {
            waiting.pending.remove(self.area);
        }
    }

    /// Changes the waiting claim's pending entry for the area.
    fn edit(
        &self,
        working: &mut BTreeMap<ItemId, Claim>,
        claims: &[Claim],
        change: impl FnOnce(&mut PendingArea),
    ) {
        if let Some(entry) = claim_mut(working, claims, self.to)
            .and_then(|waiting| waiting.pending.get_mut(self.area))
        {
            change(entry);
        }
    }

    fn withdrawal(&self, reason: WithdrawReason, pending: &PendingArea) -> Withdrawal {
        Withdrawal {
            moved: self.to_move(),
            reason,
            request: pending.request,
        }
    }

    fn to_move(&self) -> AreaMove {
        AreaMove {
            area: self.area.clone(),
            from: self.from,
            to: self.to,
        }
    }

    fn into_move(self) -> AreaMove {
        self.to_move()
    }
}

/// The claim on `item` as the settlement has left it so far, taken from `claims` on first use.
fn claim_mut<'w>(
    working: &'w mut BTreeMap<ItemId, Claim>,
    claims: &[Claim],
    item: ItemId,
) -> Option<&'w mut Claim> {
    match working.entry(item) {
        Entry::Occupied(entry) => Some(entry.into_mut()),
        Entry::Vacant(entry) => {
            let claim = claims.iter().find(|claim| claim.item == item)?;
            Some(entry.insert(claim.clone()))
        }
    }
}

#[cfg(test)]
mod tests;
