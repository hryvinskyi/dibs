//! Claims: a member holding a work item, and every decision that creates, renews or ends one.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::area::QualifiedArea;
use crate::ids::{ItemId, MemberId, SessionId};
use crate::member::Role;
use crate::radar::BranchName;
use crate::repo::RepoId;
use crate::review::ClaimState;
use crate::session::HostName;
use crate::settings::ProjectSettings;
use crate::wide::{WideScope, live_wide_naming};

/// A member holding a work item. The claim is the member's: `session` and `host` record where it
/// was taken and which machine's heartbeat keeps it alive, and any session of the member acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The item held. A claim has no id of its own; the item keys it.
    pub item: ItemId,
    /// The member holding it.
    pub member: MemberId,
    /// The session that took or last renewed it.
    pub session: SessionId,
    /// The host of that session.
    pub host: HostName,
    /// The areas the claim declared, and holds while it is live.
    pub areas: BTreeSet<QualifiedArea>,
    /// The areas a wide claim names and why; `None` for an ordinary claim. A wide claim holds none
    /// of them: they are disjoint from `areas`.
    pub wide: Option<WideScope>,
    /// When the member first claimed the item.
    pub claimed_at: OffsetDateTime,
    /// When the member last claimed it again; equal to `claimed_at` until then.
    pub renewed_at: OffsetDateTime,
    /// The last sign of life: a heartbeat, an edit, a renewal.
    pub last_seen: OffsetDateTime,
    /// Whether its holder works on it or its work waits in pull requests.
    pub state: ClaimState,
    /// The branch its work is on in each repository, the last one its holder's checkout was seen
    /// on there.
    pub branches: BTreeMap<RepoId, BranchName>,
}

impl Claim {
    /// Whether the claim is live: in review, or seen within `stale_after` of `now`. A claim in
    /// review never goes stale, since its open pull requests are its sign of life; a claim seen in
    /// the future (another machine's clock running ahead) is live.
    pub fn is_live(&self, now: OffsetDateTime, stale_after: Duration) -> bool {
        self.state.is_in_review() || now - self.last_seen <= stale_after
    }

    /// Whether its holder works on it, rather than waiting for its pull requests.
    pub fn is_working(&self) -> bool {
        !self.state.is_in_review()
    }

    /// Whether the claim holds the area — that is, declared it. The areas a wide claim names are
    /// not held.
    pub fn holds(&self, area: &QualifiedArea) -> bool {
        self.areas.contains(area)
    }

    /// Whether the claim is wide and its scope names the area.
    pub fn names_wide(&self, area: &QualifiedArea) -> bool {
        self.wide.as_ref().is_some_and(|scope| scope.names(area))
    }

    /// The branch its work is on in the repository, when one was seen.
    pub fn branch(&self, repo: &RepoId) -> Option<&BranchName> {
        self.branches.get(repo)
    }

    /// The claim's member and item.
    pub fn holder(&self) -> Holder {
        Holder {
            member: self.member,
            item: self.item,
        }
    }
}

/// Who holds something: a member, through their claim on an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Holder {
    /// The member.
    pub member: MemberId,
    /// The item of the claim.
    pub item: ItemId,
}

/// A member's request to claim (or take over) a work item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRequest {
    /// The item.
    pub item: ItemId,
    /// The member claiming.
    pub by: MemberId,
    /// The session asking.
    pub session: SessionId,
    /// That session's host.
    pub host: HostName,
    /// The areas to declare.
    pub areas: BTreeSet<QualifiedArea>,
    /// The scope to announce the claim over, for a wide claim. A renewal adds these areas to the
    /// scope the claim had and takes this reason; without one, the claim keeps the scope it had.
    pub wide: Option<WideScope>,
}

/// What a successful claim did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// Nobody held the item; this is the new claim.
    New {
        /// The claim to store.
        claim: Claim,
    },
    /// The member already held the item (a stale claim of theirs included). The claim moves to the
    /// asking session and host, keeps `claimed_at` and adds the requested areas to the ones it had.
    Renewed {
        /// The claim to store.
        claim: Claim,
        /// Areas the old claim had that it no longer gets: another member's live claim holds them
        /// now, another live wide claim names them now, or the settings no longer declare them.
        dropped_areas: BTreeSet<QualifiedArea>,
    },
}

impl ClaimOutcome {
    /// The claim to store.
    pub fn claim(&self) -> &Claim {
        match self {
            Self::New { claim } | Self::Renewed { claim, .. } => claim,
        }
    }
}

/// Why a claim is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ClaimRefused {
    /// Another member's live claim holds the item.
    #[error("another member holds this item")]
    HeldBy {
        /// The holder.
        member: MemberId,
        /// When they claimed it.
        since: OffsetDateTime,
        /// When they were last seen.
        last_seen: OffsetDateTime,
    },
    /// Another member's claim on the item is stale; only an explicit takeover may take it.
    #[error("another member's claim on this item is stale; take it over explicitly")]
    Stale {
        /// The previous holder.
        member: MemberId,
        /// When they were last seen.
        last_seen: OffsetDateTime,
    },
    /// A declared area is held by another member's live claim.
    #[error("the area {area} is held by another member")]
    AreaHeld {
        /// The area.
        area: QualifiedArea,
        /// Its holder.
        member: MemberId,
        /// The item of the holder's claim.
        item: ItemId,
    },
    /// A declared area is not declared by the settings.
    #[error("the project declares no area {area}")]
    UnknownArea {
        /// The area.
        area: QualifiedArea,
    },
    /// An area of the wide scope is named by another live wide claim.
    #[error("the area {area} is already in another member's wide claim")]
    WideOverlap {
        /// The area.
        area: QualifiedArea,
        /// The member of the other wide claim.
        member: MemberId,
        /// Its item.
        item: ItemId,
    },
    /// An area would be both held by the claim and named by its wide scope.
    #[error("the claim cannot both hold {area} and name it in its wide scope")]
    WideAndHeld {
        /// The area.
        area: QualifiedArea,
    },
}

/// Decides a claim on an item.
///
/// `current` is the item's stored claim, `others` the project's other claims (stale ones are
/// ignored). An area belongs to one member at a time: a requested area another member's live claim
/// holds refuses the whole claim. The same member may hold an area under several claims. A wide
/// claim holds none of the areas its scope names, so another member's wide claim never refuses an
/// area; but one area is named by one live wide claim at a time, and a claim never both holds and
/// names one area.
///
/// # Errors
/// [`ClaimRefused`] naming the first conflict: the item first, then the areas in order.
pub fn decide_claim(
    current: Option<&Claim>,
    others: &[Claim],
    request: &ClaimRequest,
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> Result<ClaimOutcome, ClaimRefused> {
    let stale_after = settings.stale_after();
    match current {
        Some(existing) if existing.member == request.by => {
            check_areas(&request.areas, others, request, now, settings)?;
            let (kept, mut dropped_areas) =
                keep_free_areas(&existing.areas, others, request, now, settings);
            let mut areas = kept;
            areas.extend(request.areas.iter().cloned());
            let (wide, dropped_wide) = settle_wide(
                existing.wide.as_ref(),
                others,
                request,
                &areas,
                now,
                settings,
            )?;
            dropped_areas.extend(dropped_wide);
            let claim = Claim {
                session: request.session,
                host: request.host.clone(),
                areas,
                wide,
                renewed_at: now,
                last_seen: now,
                ..existing.clone()
            };
            Ok(ClaimOutcome::Renewed {
                claim,
                dropped_areas,
            })
        }
        Some(existing) if existing.is_live(now, stale_after) => Err(ClaimRefused::HeldBy {
            member: existing.member,
            since: existing.claimed_at,
            last_seen: existing.last_seen,
        }),
        Some(existing) => Err(ClaimRefused::Stale {
            member: existing.member,
            last_seen: existing.last_seen,
        }),
        None => {
            check_areas(&request.areas, others, request, now, settings)?;
            let (wide, _) = settle_wide(None, others, request, &request.areas, now, settings)?;
            Ok(ClaimOutcome::New {
                claim: new_claim(request, request.areas.clone(), wide, now),
            })
        }
    }
}

/// A takeover of another member's stale claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Takeover {
    /// The stale claim that ends.
    pub previous: Claim,
    /// The claim to store. It declares the requested areas and those of the previous claim that
    /// nobody else holds now, and carries on the previous claim's wide scope as far as no other
    /// live wide claim names it now.
    pub claim: Claim,
    /// Areas of the previous claim it does not get: another member's live claim holds them,
    /// another live wide claim names them, or the settings no longer declare them.
    pub dropped_areas: BTreeSet<QualifiedArea>,
}

/// Why a takeover is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TakeoverRefused {
    /// Nobody holds the item; claim it instead.
    #[error("nobody holds this item; claim it instead")]
    NothingToTake,
    /// The member's own claim; claim it again instead.
    #[error("this claim is yours; claim the item again to renew it")]
    OwnClaim,
    /// The holder's claim is live.
    #[error("the holder's claim is live; message them instead")]
    NotStale {
        /// The holder.
        member: MemberId,
        /// When they were last seen.
        last_seen: OffsetDateTime,
    },
    /// A requested area is held by another member's live claim.
    #[error("the area {area} is held by another member")]
    AreaHeld {
        /// The area.
        area: QualifiedArea,
        /// Its holder.
        member: MemberId,
        /// The item of the holder's claim.
        item: ItemId,
    },
    /// A requested area is not declared by the settings.
    #[error("the project declares no area {area}")]
    UnknownArea {
        /// The area.
        area: QualifiedArea,
    },
    /// An area of the requested wide scope is named by another live wide claim.
    #[error("the area {area} is already in another member's wide claim")]
    WideOverlap {
        /// The area.
        area: QualifiedArea,
        /// The member of the other wide claim.
        member: MemberId,
        /// Its item.
        item: ItemId,
    },
    /// An area would be both held by the claim and named by its wide scope.
    #[error("the claim cannot both hold {area} and name it in its wide scope")]
    WideAndHeld {
        /// The area.
        area: QualifiedArea,
    },
}

/// Decides an explicit takeover of the item's claim. Only another member's stale claim can be taken.
///
/// # Errors
/// [`TakeoverRefused`] naming why not.
pub fn decide_takeover(
    current: Option<&Claim>,
    others: &[Claim],
    request: &ClaimRequest,
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> Result<Takeover, TakeoverRefused> {
    let previous = current.ok_or(TakeoverRefused::NothingToTake)?;
    if previous.member == request.by {
        return Err(TakeoverRefused::OwnClaim);
    }
    if previous.is_live(now, settings.stale_after()) {
        return Err(TakeoverRefused::NotStale {
            member: previous.member,
            last_seen: previous.last_seen,
        });
    }
    check_areas(&request.areas, others, request, now, settings)?;
    let (mut areas, mut dropped_areas) =
        keep_free_areas(&previous.areas, others, request, now, settings);
    areas.extend(request.areas.iter().cloned());
    let (wide, dropped_wide) = settle_wide(
        previous.wide.as_ref(),
        others,
        request,
        &areas,
        now,
        settings,
    )?;
    dropped_areas.extend(dropped_wide);
    Ok(Takeover {
        previous: previous.clone(),
        claim: new_claim(request, areas, wide, now),
        dropped_areas,
    })
}

/// A claim the holder released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Released {
    /// The claim that ends.
    pub claim: Claim,
}

/// A claim the holder completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completed {
    /// The claim that ends.
    pub claim: Claim,
}

/// Why a release or completion is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HolderOnly {
    /// Nobody holds the item.
    #[error("nobody holds this item")]
    NoClaim,
    /// Someone else holds it.
    #[error("only the member holding this item may do that")]
    NotHolder {
        /// The holder.
        member: MemberId,
    },
}

fn holder_only(current: Option<&Claim>, by: MemberId) -> Result<&Claim, HolderOnly> {
    let claim = current.ok_or(HolderOnly::NoClaim)?;
    if claim.member == by {
        Ok(claim)
    } else {
        Err(HolderOnly::NotHolder {
            member: claim.member,
        })
    }
}

/// Decides a release. Only the holder releases, whether their claim is live or stale.
///
/// # Errors
/// [`HolderOnly`] when nobody or somebody else holds the item.
pub fn decide_release(current: Option<&Claim>, by: MemberId) -> Result<Released, HolderOnly> {
    holder_only(current, by).map(|claim| Released {
        claim: claim.clone(),
    })
}

/// Decides a completion. Only the holder completes.
///
/// # Errors
/// [`HolderOnly`] when nobody or somebody else holds the item.
pub fn decide_complete(current: Option<&Claim>, by: MemberId) -> Result<Completed, HolderOnly> {
    holder_only(current, by).map(|claim| Completed {
        claim: claim.clone(),
    })
}

/// A claim an admin ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForceReleased {
    /// The claim that ends.
    pub claim: Claim,
}

/// Why a forced release is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ForceReleaseRefused {
    /// The caller is not an admin.
    #[error("only an admin may force a release")]
    NotAdmin,
    /// Nobody holds the item.
    #[error("nobody holds this item")]
    NoClaim,
}

/// Decides a forced release, which only an admin may do.
///
/// # Errors
/// [`ForceReleaseRefused`] when the caller is no admin or nobody holds the item.
pub fn decide_force_release(
    current: Option<&Claim>,
    by: Role,
) -> Result<ForceReleased, ForceReleaseRefused> {
    if !by.is_admin() {
        return Err(ForceReleaseRefused::NotAdmin);
    }
    let claim = current.ok_or(ForceReleaseRefused::NoClaim)?;
    Ok(ForceReleased {
        claim: claim.clone(),
    })
}

/// The live claim of another member than `me` that holds the area; the lowest item when, against
/// the rules, several do.
pub fn held_by_other<'a, I>(
    area: &QualifiedArea,
    me: MemberId,
    claims: I,
    now: OffsetDateTime,
    stale_after: Duration,
) -> Option<&'a Claim>
where
    I: IntoIterator<Item = &'a Claim>,
{
    claims
        .into_iter()
        .filter(|claim| claim.member != me && claim.is_live(now, stale_after) && claim.holds(area))
        .min_by_key(|claim| claim.item)
}

/// The claim a member's edits are attributed to when nothing else decides: of their live claims,
/// the one most recently claimed or renewed, the lowest item on a tie.
pub fn target_claim<'a, I>(claims: I) -> Option<&'a Claim>
where
    I: IntoIterator<Item = &'a Claim>,
{
    claims
        .into_iter()
        .max_by_key(|claim| (claim.renewed_at, Reverse(claim.item)))
}

fn new_claim(
    request: &ClaimRequest,
    areas: BTreeSet<QualifiedArea>,
    wide: Option<WideScope>,
    now: OffsetDateTime,
) -> Claim {
    Claim {
        item: request.item,
        member: request.by,
        session: request.session,
        host: request.host.clone(),
        areas,
        wide,
        claimed_at: now,
        renewed_at: now,
        last_seen: now,
        state: ClaimState::Working,
        branches: BTreeMap::new(),
    }
}

fn check_areas(
    areas: &BTreeSet<QualifiedArea>,
    others: &[Claim],
    request: &ClaimRequest,
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> Result<(), AreaConflict> {
    for area in areas {
        if settings.area(area).is_none() {
            return Err(AreaConflict::Unknown { area: area.clone() });
        }
        let other_items = others.iter().filter(|claim| claim.item != request.item);
        if let Some(holder) =
            held_by_other(area, request.by, other_items, now, settings.stale_after())
        {
            return Err(AreaConflict::Held {
                area: area.clone(),
                member: holder.member,
                item: holder.item,
            });
        }
    }
    Ok(())
}

/// A requested area that cannot be declared.
enum AreaConflict {
    Held {
        area: QualifiedArea,
        member: MemberId,
        item: ItemId,
    },
    Unknown {
        area: QualifiedArea,
    },
    WideOverlap {
        area: QualifiedArea,
        member: MemberId,
        item: ItemId,
    },
    WideAndHeld {
        area: QualifiedArea,
    },
}

impl From<AreaConflict> for ClaimRefused {
    fn from(conflict: AreaConflict) -> Self {
        match conflict {
            AreaConflict::Held { area, member, item } => Self::AreaHeld { area, member, item },
            AreaConflict::Unknown { area } => Self::UnknownArea { area },
            AreaConflict::WideOverlap { area, member, item } => {
                Self::WideOverlap { area, member, item }
            }
            AreaConflict::WideAndHeld { area } => Self::WideAndHeld { area },
        }
    }
}

impl From<AreaConflict> for TakeoverRefused {
    fn from(conflict: AreaConflict) -> Self {
        match conflict {
            AreaConflict::Held { area, member, item } => Self::AreaHeld { area, member, item },
            AreaConflict::Unknown { area } => Self::UnknownArea { area },
            AreaConflict::WideOverlap { area, member, item } => {
                Self::WideOverlap { area, member, item }
            }
            AreaConflict::WideAndHeld { area } => Self::WideAndHeld { area },
        }
    }
}

/// The wide scope a claim ends up with, and the inherited areas it loses.
///
/// The requested scope must name declared areas no other live wide claim names; an inherited
/// scope keeps the areas that still pass and drops the rest. The two are merged, the requested
/// reason winning. The result names no area of `held`.
fn settle_wide(
    inherited: Option<&WideScope>,
    others: &[Claim],
    request: &ClaimRequest,
    held: &BTreeSet<QualifiedArea>,
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> Result<(Option<WideScope>, BTreeSet<QualifiedArea>), AreaConflict> {
    let stale_after = settings.stale_after();
    let named_elsewhere = |area: &QualifiedArea| {
        live_wide_naming(
            area,
            others.iter().filter(|claim| claim.item != request.item),
            now,
            stale_after,
        )
    };
    if let Some(requested) = &request.wide {
        for area in requested.areas() {
            if settings.area(area).is_none() {
                return Err(AreaConflict::Unknown { area: area.clone() });
            }
            if let Some(other) = named_elsewhere(area) {
                return Err(AreaConflict::WideOverlap {
                    area: area.clone(),
                    member: other.member,
                    item: other.item,
                });
            }
        }
    }
    let (kept, dropped): (BTreeSet<QualifiedArea>, BTreeSet<QualifiedArea>) =
        inherited
            .map(|scope| {
                scope.areas().iter().cloned().partition(|area| {
                    settings.area(area).is_some() && named_elsewhere(area).is_none()
                })
            })
            .unwrap_or_default();
    let mut names = kept;
    let reason = match (&request.wide, inherited) {
        (Some(requested), _) => {
            names.extend(requested.areas().iter().cloned());
            requested.reason().clone()
        }
        (None, Some(scope)) => scope.reason().clone(),
        (None, None) => return Ok((None, dropped)),
    };
    if let Some(area) = names.intersection(held).next() {
        return Err(AreaConflict::WideAndHeld { area: area.clone() });
    }
    Ok((WideScope::new(names, reason).ok(), dropped))
}

/// Splits inherited areas into those the request may keep and those it may not.
fn keep_free_areas(
    inherited: &BTreeSet<QualifiedArea>,
    others: &[Claim],
    request: &ClaimRequest,
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> (BTreeSet<QualifiedArea>, BTreeSet<QualifiedArea>) {
    inherited.iter().cloned().partition(|area| {
        settings.area(area).is_some()
            && held_by_other(
                area,
                request.by,
                others.iter().filter(|claim| claim.item != request.item),
                now,
                settings.stale_after(),
            )
            .is_none()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ALICE, BOB, CAROL, area, areas, at, claim, request, two_repo_settings};

    #[test]
    fn liveness_is_measured_from_last_seen() {
        let settings = two_repo_settings();
        let stale_after = settings.stale_after();
        let c = claim(1, ALICE, &[], 0);
        assert!(c.is_live(at(0), stale_after));
        assert!(c.is_live(at(0) + stale_after, stale_after));
        assert!(!c.is_live(at(0) + stale_after + Duration::seconds(1), stale_after));
        assert!(
            c.is_live(at(-10), stale_after),
            "a clock behind the claim still sees it live"
        );
    }

    #[test]
    fn a_blocked_item_is_claimed_and_its_blockers_are_there_to_warn_about() {
        use crate::dependency::{Dependency, DependencySource, blockers};
        use crate::item::ItemStatus;

        let settings = two_repo_settings();
        let graph = [Dependency {
            item: ItemId(1),
            blocked_by: ItemId(2),
            source: DependencySource::Imported,
        }];
        let outcome = decide_claim(None, &[], &request(1, ALICE, &[]), at(5), &settings);
        assert!(
            matches!(outcome, Ok(ClaimOutcome::New { .. })),
            "{outcome:?}"
        );
        let open = |_: ItemId| Some((ItemStatus::Open, false));
        assert_eq!(blockers(ItemId(1), &graph, open), [ItemId(2)]);
    }

    #[test]
    fn a_free_item_is_claimed() {
        let settings = two_repo_settings();
        let req = request(1, ALICE, &["api:billing"]);
        let outcome = decide_claim(None, &[], &req, at(5), &settings).unwrap();
        let ClaimOutcome::New { claim } = outcome else {
            panic!("expected a new claim, got {outcome:?}");
        };
        assert_eq!(claim.member, ALICE);
        assert_eq!(claim.areas, areas(&["api:billing"]));
        assert_eq!(
            (claim.claimed_at, claim.renewed_at, claim.last_seen),
            (at(5), at(5), at(5))
        );
    }

    #[test]
    fn another_members_live_claim_refuses_with_its_holder() {
        let settings = two_repo_settings();
        let current = claim(1, BOB, &[], 0);
        let refused = decide_claim(
            Some(&current),
            &[],
            &request(1, ALICE, &[]),
            at(10),
            &settings,
        );
        assert_eq!(
            refused,
            Err(ClaimRefused::HeldBy {
                member: BOB,
                since: at(0),
                last_seen: at(0)
            })
        );
    }

    #[test]
    fn another_members_stale_claim_needs_a_takeover() {
        let settings = two_repo_settings();
        let current = claim(1, BOB, &[], 0);
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        let refused = decide_claim(
            Some(&current),
            &[],
            &request(1, ALICE, &[]),
            late,
            &settings,
        );
        assert_eq!(
            refused,
            Err(ClaimRefused::Stale {
                member: BOB,
                last_seen: at(0)
            })
        );
    }

    #[test]
    fn an_area_another_member_holds_refuses_the_claim() {
        let settings = two_repo_settings();
        let others = [claim(2, BOB, &["api:billing"], 0)];
        let refused = decide_claim(
            None,
            &others,
            &request(1, ALICE, &["api:search", "api:billing"]),
            at(1),
            &settings,
        );
        assert_eq!(
            refused,
            Err(ClaimRefused::AreaHeld {
                area: area("api:billing"),
                member: BOB,
                item: crate::ids::ItemId(2)
            })
        );
    }

    #[test]
    fn a_stale_holder_does_not_hold_its_areas() {
        let settings = two_repo_settings();
        let others = [claim(2, BOB, &["api:billing"], 0)];
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        let outcome = decide_claim(
            None,
            &others,
            &request(1, ALICE, &["api:billing"]),
            late,
            &settings,
        );
        assert!(matches!(outcome, Ok(ClaimOutcome::New { .. })));
    }

    #[test]
    fn one_member_may_hold_an_area_under_several_claims() {
        let settings = two_repo_settings();
        let others = [claim(2, ALICE, &["api:billing"], 0)];
        let outcome = decide_claim(
            None,
            &others,
            &request(1, ALICE, &["api:billing"]),
            at(1),
            &settings,
        );
        assert!(matches!(outcome, Ok(ClaimOutcome::New { .. })));
    }

    #[test]
    fn an_area_name_reused_in_two_repositories_is_two_areas() {
        let settings = two_repo_settings();
        let others = [claim(2, BOB, &["api:billing"], 0)];
        let outcome = decide_claim(
            None,
            &others,
            &request(1, ALICE, &["web:billing"]),
            at(1),
            &settings,
        );
        assert!(matches!(outcome, Ok(ClaimOutcome::New { .. })));
    }

    #[test]
    fn a_claim_may_span_repositories() {
        let settings = two_repo_settings();
        let req = request(1, ALICE, &["api:billing", "web:checkout"]);
        let outcome = decide_claim(None, &[], &req, at(1), &settings).unwrap();
        assert_eq!(
            outcome.claim().areas,
            areas(&["api:billing", "web:checkout"])
        );
    }

    #[test]
    fn an_undeclared_area_is_refused() {
        let settings = two_repo_settings();
        let refused = decide_claim(
            None,
            &[],
            &request(1, ALICE, &["web:search"]),
            at(1),
            &settings,
        );
        assert_eq!(
            refused,
            Err(ClaimRefused::UnknownArea {
                area: area("web:search")
            })
        );
    }

    #[test]
    fn a_member_renewing_their_own_stale_claim_keeps_it_and_unions_areas() {
        let settings = two_repo_settings();
        let current = claim(1, ALICE, &["api:billing"], 0);
        let late = at(0) + settings.stale_after() + Duration::hours(1);
        let mut req = request(1, ALICE, &["web:checkout"]);
        req.session = SessionId(99);
        let outcome = decide_claim(Some(&current), &[], &req, late, &settings).unwrap();
        let ClaimOutcome::Renewed {
            claim,
            dropped_areas,
        } = outcome
        else {
            panic!("expected a renewal, got {outcome:?}");
        };
        assert_eq!(claim.claimed_at, at(0));
        assert_eq!((claim.renewed_at, claim.last_seen), (late, late));
        assert_eq!(claim.session, SessionId(99));
        assert_eq!(claim.areas, areas(&["api:billing", "web:checkout"]));
        assert!(dropped_areas.is_empty());
    }

    #[test]
    fn renewing_twice_with_the_same_areas_changes_only_the_times() {
        let settings = two_repo_settings();
        let req = request(1, ALICE, &["api:billing"]);
        let first = decide_claim(None, &[], &req, at(0), &settings).unwrap();
        let again = decide_claim(Some(first.claim()), &[], &req, at(3), &settings).unwrap();
        assert_eq!(again.claim().areas, first.claim().areas);
        assert_eq!(again.claim().claimed_at, first.claim().claimed_at);
        assert_eq!(again.claim().renewed_at, at(3));
    }

    #[test]
    fn renewal_drops_an_old_area_someone_took_while_the_claim_was_stale() {
        let settings = two_repo_settings();
        let current = claim(1, ALICE, &["api:billing", "api:search"], 0);
        let late = at(0) + settings.stale_after() + Duration::hours(1);
        let others = [claim(2, BOB, &["api:billing"], 60 * 25)];
        let outcome = decide_claim(
            Some(&current),
            &others,
            &request(1, ALICE, &[]),
            late,
            &settings,
        )
        .unwrap();
        let ClaimOutcome::Renewed {
            claim,
            dropped_areas,
        } = outcome
        else {
            panic!("expected a renewal");
        };
        assert_eq!(claim.areas, areas(&["api:search"]));
        assert_eq!(dropped_areas, areas(&["api:billing"]));
    }

    #[test]
    fn renewal_refuses_a_newly_requested_area_another_member_holds() {
        let settings = two_repo_settings();
        let current = claim(1, ALICE, &[], 0);
        let others = [claim(2, BOB, &["api:search"], 0)];
        let refused = decide_claim(
            Some(&current),
            &others,
            &request(1, ALICE, &["api:search"]),
            at(1),
            &settings,
        );
        assert!(matches!(refused, Err(ClaimRefused::AreaHeld { .. })));
    }

    #[test]
    fn takeover_outcomes() {
        let settings = two_repo_settings();
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        let bob_stale = claim(1, BOB, &["api:billing", "api:search"], 0);
        let others = [claim(2, CAROL, &["api:search"], 60 * 25)];
        let taken = decide_takeover(
            Some(&bob_stale),
            &others,
            &request(1, ALICE, &["web:checkout"]),
            late,
            &settings,
        )
        .unwrap();
        assert_eq!(taken.previous, bob_stale);
        assert_eq!(taken.claim.member, ALICE);
        assert_eq!(taken.claim.claimed_at, late);
        assert_eq!(taken.claim.areas, areas(&["api:billing", "web:checkout"]));
        assert_eq!(taken.dropped_areas, areas(&["api:search"]));

        assert_eq!(
            decide_takeover(None, &[], &request(1, ALICE, &[]), late, &settings),
            Err(TakeoverRefused::NothingToTake)
        );
        let mine = claim(1, ALICE, &[], 0);
        assert_eq!(
            decide_takeover(Some(&mine), &[], &request(1, ALICE, &[]), late, &settings),
            Err(TakeoverRefused::OwnClaim)
        );
        let bob_live = claim(1, BOB, &[], 0);
        assert_eq!(
            decide_takeover(
                Some(&bob_live),
                &[],
                &request(1, ALICE, &[]),
                at(5),
                &settings
            ),
            Err(TakeoverRefused::NotStale {
                member: BOB,
                last_seen: at(0)
            })
        );
        assert!(matches!(
            decide_takeover(
                Some(&bob_stale),
                &others,
                &request(1, ALICE, &["api:search"]),
                late,
                &settings
            ),
            Err(TakeoverRefused::AreaHeld { .. })
        ));
        assert!(matches!(
            decide_takeover(
                Some(&bob_stale),
                &[],
                &request(1, ALICE, &["web:search"]),
                late,
                &settings
            ),
            Err(TakeoverRefused::UnknownArea { .. })
        ));
    }

    #[test]
    fn only_the_holder_releases_or_completes() {
        let current = claim(1, ALICE, &[], 0);
        assert_eq!(
            decide_release(Some(&current), ALICE).unwrap().claim,
            current
        );
        assert_eq!(
            decide_complete(Some(&current), ALICE).unwrap().claim,
            current
        );
        assert_eq!(
            decide_release(Some(&current), BOB),
            Err(HolderOnly::NotHolder { member: ALICE })
        );
        assert_eq!(
            decide_complete(Some(&current), BOB),
            Err(HolderOnly::NotHolder { member: ALICE })
        );
        assert_eq!(decide_release(None, ALICE), Err(HolderOnly::NoClaim));
        assert_eq!(decide_complete(None, ALICE), Err(HolderOnly::NoClaim));
    }

    #[test]
    fn only_an_admin_forces_a_release() {
        let current = claim(1, ALICE, &[], 0);
        assert_eq!(
            decide_force_release(Some(&current), Role::Admin)
                .unwrap()
                .claim,
            current
        );
        assert_eq!(
            decide_force_release(Some(&current), Role::Member),
            Err(ForceReleaseRefused::NotAdmin)
        );
        assert_eq!(
            decide_force_release(Some(&current), Role::Importer),
            Err(ForceReleaseRefused::NotAdmin)
        );
        assert_eq!(
            decide_force_release(None, Role::Admin),
            Err(ForceReleaseRefused::NoClaim)
        );
    }

    fn scope(named: &[&str], why: &str) -> WideScope {
        WideScope::new(areas(named), crate::wide::WideReason::parse(why).unwrap()).unwrap()
    }

    fn wide_request(item: u64, by: MemberId, held: &[&str], named: &[&str]) -> ClaimRequest {
        ClaimRequest {
            wide: Some(scope(named, "Upgrade the HTTP client")),
            ..request(item, by, held)
        }
    }

    fn wide_claim(item: u64, member: MemberId, named: &[&str], minutes: i64) -> Claim {
        Claim {
            wide: Some(scope(named, "Upgrade the HTTP client")),
            ..claim(item, member, &[], minutes)
        }
    }

    #[test]
    fn a_wide_claim_over_held_areas_holds_none_of_them() {
        let settings = two_repo_settings();
        let others = [claim(2, BOB, &["api:billing"], 0)];
        let outcome = decide_claim(
            None,
            &others,
            &wide_request(1, ALICE, &[], &["api:billing", "api:search"]),
            at(1),
            &settings,
        )
        .unwrap();
        let made = outcome.claim();
        assert!(made.areas.is_empty());
        assert!(made.names_wide(&area("api:billing")));
        assert!(!made.holds(&area("api:billing")));
        assert!(!made.holds(&area("api:search")));
    }

    #[test]
    fn nobody_is_refused_an_area_a_wide_claim_names() {
        let settings = two_repo_settings();
        let others = [wide_claim(1, ALICE, &["api:search"], 0)];
        let outcome = decide_claim(
            None,
            &others,
            &request(2, BOB, &["api:search"]),
            at(1),
            &settings,
        );
        assert!(
            matches!(outcome, Ok(ClaimOutcome::New { .. })),
            "{outcome:?}"
        );
    }

    #[test]
    fn two_live_wide_claims_never_name_one_area() {
        let settings = two_repo_settings();
        let others = [wide_claim(1, ALICE, &["api:billing"], 0)];
        assert_eq!(
            decide_claim(
                None,
                &others,
                &wide_request(2, BOB, &[], &["api:search", "api:billing"]),
                at(1),
                &settings,
            ),
            Err(ClaimRefused::WideOverlap {
                area: area("api:billing"),
                member: ALICE,
                item: ItemId(1),
            })
        );
        let mine = [wide_claim(1, BOB, &["api:billing"], 0)];
        assert!(
            matches!(
                decide_claim(
                    None,
                    &mine,
                    &wide_request(2, BOB, &[], &["api:billing"]),
                    at(1),
                    &settings,
                ),
                Err(ClaimRefused::WideOverlap { .. })
            ),
            "not even two of one member's"
        );
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        assert!(
            decide_claim(
                None,
                &others,
                &wide_request(2, BOB, &[], &["api:billing"]),
                late,
                &settings,
            )
            .is_ok(),
            "a stale wide claim names nothing"
        );
    }

    #[test]
    fn a_wide_scope_names_declared_areas_the_claim_does_not_hold() {
        let settings = two_repo_settings();
        assert_eq!(
            decide_claim(
                None,
                &[],
                &wide_request(1, ALICE, &[], &["web:search"]),
                at(1),
                &settings,
            ),
            Err(ClaimRefused::UnknownArea {
                area: area("web:search")
            })
        );
        assert_eq!(
            decide_claim(
                None,
                &[],
                &wide_request(1, ALICE, &["api:billing"], &["api:billing", "api:search"]),
                at(1),
                &settings,
            ),
            Err(ClaimRefused::WideAndHeld {
                area: area("api:billing")
            })
        );
        let current = claim(1, ALICE, &["api:billing"], 0);
        assert_eq!(
            decide_claim(
                Some(&current),
                &[],
                &wide_request(1, ALICE, &[], &["api:billing"]),
                at(1),
                &settings,
            ),
            Err(ClaimRefused::WideAndHeld {
                area: area("api:billing")
            }),
            "a renewal cannot name an area the claim already holds"
        );
    }

    #[test]
    fn a_renewal_widens_the_scope_takes_the_new_reason_and_keeps_it_when_asked_nothing() {
        let settings = two_repo_settings();
        let current = wide_claim(1, ALICE, &["api:billing"], 0);
        let widened = decide_claim(
            Some(&current),
            &[],
            &ClaimRequest {
                wide: Some(scope(&["api:search"], "Rename the money type")),
                ..request(1, ALICE, &[])
            },
            at(1),
            &settings,
        )
        .unwrap();
        let scope_now = widened.claim().wide.clone().unwrap();
        assert_eq!(scope_now.areas(), &areas(&["api:billing", "api:search"]));
        assert_eq!(scope_now.reason().as_str(), "Rename the money type");
        let kept = decide_claim(
            Some(widened.claim()),
            &[],
            &request(1, ALICE, &["web:checkout"]),
            at(2),
            &settings,
        )
        .unwrap();
        assert_eq!(kept.claim().wide, Some(scope_now));
        assert_eq!(kept.claim().areas, areas(&["web:checkout"]));
    }

    #[test]
    fn a_renewal_drops_a_scope_area_another_wide_claim_took_while_it_was_stale() {
        let settings = two_repo_settings();
        let current = wide_claim(1, ALICE, &["api:billing", "api:search"], 0);
        let late = at(0) + settings.stale_after() + Duration::hours(1);
        let others = [wide_claim(2, BOB, &["api:billing"], 60 * 25)];
        let outcome = decide_claim(
            Some(&current),
            &others,
            &request(1, ALICE, &[]),
            late,
            &settings,
        )
        .unwrap();
        let ClaimOutcome::Renewed {
            claim,
            dropped_areas,
        } = outcome
        else {
            panic!("expected a renewal");
        };
        assert_eq!(
            claim.wide.map(|scope| scope.areas().clone()),
            Some(areas(&["api:search"]))
        );
        assert_eq!(dropped_areas, areas(&["api:billing"]));
    }

    #[test]
    fn a_takeover_carries_the_wide_scope_on() {
        let settings = two_repo_settings();
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        let stale = wide_claim(1, BOB, &["api:billing", "web:checkout"], 0);
        let taken =
            decide_takeover(Some(&stale), &[], &request(1, ALICE, &[]), late, &settings).unwrap();
        assert_eq!(taken.claim.member, ALICE);
        assert_eq!(taken.claim.wide, stale.wide);
        let others = [wide_claim(2, CAROL, &["web:checkout"], 60 * 24)];
        let partly = decide_takeover(
            Some(&stale),
            &others,
            &request(1, ALICE, &[]),
            late,
            &settings,
        )
        .unwrap();
        assert_eq!(
            partly.claim.wide.map(|scope| scope.areas().clone()),
            Some(areas(&["api:billing"]))
        );
        assert_eq!(partly.dropped_areas, areas(&["web:checkout"]));
        assert!(matches!(
            decide_takeover(
                Some(&stale),
                &others,
                &wide_request(1, ALICE, &[], &["web:checkout"]),
                late,
                &settings
            ),
            Err(TakeoverRefused::WideOverlap { .. })
        ));
    }

    #[test]
    fn the_target_claim_is_the_latest_claimed_or_renewed_then_the_lowest_item() {
        let mut older = claim(5, ALICE, &[], 0);
        let mut newer = claim(9, ALICE, &[], 10);
        assert_eq!(
            target_claim([&older, &newer]).map(|c| c.item),
            Some(newer.item)
        );
        newer.renewed_at = at(0);
        older.renewed_at = at(0);
        assert_eq!(
            target_claim([&newer, &older]).map(|c| c.item),
            Some(older.item)
        );
        assert_eq!(target_claim(std::iter::empty::<&Claim>()), None);
    }
}
