//! Wide claims: one claim announced over many areas for a change that cuts across them — a
//! dependency upgrade, a rename across packages, a hygiene pass over a dozen modules.
//!
//! A wide claim **holds none of the areas it names**, so it never stands in anyone's way: a member
//! claims such an area as if the wide claim were not there. What the wide claim gives is the
//! asking: every member whose live claim holds one of its areas is sent one announcement that asks
//! to be acknowledged, and a holder's acknowledgement of it is their grant, for the areas it named,
//! to the wide claim's member for the wide claim's item. dibs acknowledges it on the holder's
//! behalf by the test it grants requests by, at once or once the project's `grant_after` has
//! passed. Until it is acknowledged, the wide claimer's edits in their area are held. Two wide
//! claims never name one area at a time.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::area::QualifiedArea;
use crate::claim::{Claim, FootprintPaths, Holder};
use crate::grant::{Grant, GrantBlock, GrantDecision, GrantOutcome, decide_grant, judge_area};
use crate::ids::{ItemId, MemberId, MessageId};
use crate::settings::ProjectSettings;
use crate::text::{has_control, string_value};

/// Why a wide change is being made, as the holders it asks read it: 1 to 300 characters after
/// trimming, with no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WideReason(String);

/// Why a text is not a wide claim's reason.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WideReasonError {
    /// The trimmed text is empty, longer than 300 characters, or has control characters.
    #[error("the reason of a wide claim is 1 to 300 characters with no control characters")]
    Malformed,
}

impl WideReason {
    /// The longest reason, in characters.
    pub const MAX_CHARS: usize = 300;

    /// Parses a reason, trimming whitespace at both ends.
    ///
    /// # Errors
    /// [`WideReasonError::Malformed`] when the trimmed text breaks the rules above.
    pub fn parse(value: &str) -> Result<Self, WideReasonError> {
        let value = value.trim();
        if value.is_empty() || value.chars().count() > Self::MAX_CHARS || has_control(value) {
            return Err(WideReasonError::Malformed);
        }
        Ok(Self(value.to_owned()))
    }
}

string_value!(WideReason, WideReasonError);

/// The areas a wide claim names and why. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideScope {
    areas: BTreeSet<QualifiedArea>,
    reason: WideReason,
}

/// Why a wide scope cannot be made.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WideScopeError {
    /// It names no area.
    #[error("a wide claim names at least one area")]
    NoAreas,
}

impl WideScope {
    /// A scope over these areas, for this reason.
    ///
    /// # Errors
    /// [`WideScopeError::NoAreas`] when `areas` is empty.
    pub fn new(areas: BTreeSet<QualifiedArea>, reason: WideReason) -> Result<Self, WideScopeError> {
        if areas.is_empty() {
            return Err(WideScopeError::NoAreas);
        }
        Ok(Self { areas, reason })
    }

    /// The areas named.
    pub fn areas(&self) -> &BTreeSet<QualifiedArea> {
        &self.areas
    }

    /// Why.
    pub fn reason(&self) -> &WideReason {
        &self.reason
    }

    /// Whether the scope names the area.
    pub fn names(&self, area: &QualifiedArea) -> bool {
        self.areas.contains(area)
    }
}

/// An announcement of a wide claim, sent to one holder: the areas of the scope their claims held
/// when it was sent. It is answered by the holder acknowledging its message, or by dibs
/// acknowledging it on the holder's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideAsk {
    /// The message that carries it.
    pub message: MessageId,
    /// The wide claim's item.
    pub item: ItemId,
    /// The wide claim's member, who asks.
    pub from: MemberId,
    /// The holder asked.
    pub to: MemberId,
    /// The areas asked about.
    pub areas: BTreeSet<QualifiedArea>,
    /// Why dibs acknowledged it on the holder's behalf; `None` while it did not. An announcement
    /// dibs acknowledged is answered, although its holder wrote no acknowledgement.
    pub acked_by_dibs: Option<GrantDecision>,
}

/// An announcement to send: to `to`, for the wide claim of `from` on `item`, about `areas`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AskDue {
    /// The wide claim's item.
    pub item: ItemId,
    /// The wide claim's member.
    pub from: MemberId,
    /// The holder to ask.
    pub to: MemberId,
    /// The areas of the scope the holder's live claims hold and that nothing has asked them about
    /// yet.
    pub areas: BTreeSet<QualifiedArea>,
}

/// The announcements due among these claims, ordered by wide item and then by holder.
///
/// For every live wide claim and every area it names, the member whose live claim holds that area
/// is asked — whether that claim is worked on or in review — unless it is the wide claim's own
/// member, they already granted that member edits in the area for the wide claim's item, an
/// announcement of that wide claim already asked them about that area and still waits for their
/// acknowledgement, or a revoke between that member and the wide claim's member in the area sticks
/// ([`GrantBlock::applies`]). `asked` lists the announcements that still wait; one dibs
/// acknowledged on the holder's behalf is not counted as asking, even when the caller lists it.
/// Its grants stand in for it while they are in force, and a revoke of one sticks through
/// `blocks`; once its grants ended with the holder's claim, a later claim of that holder over the
/// area is asked again, so the wide claimer's edits there are not held with nothing sent. One
/// announcement goes to each holder, naming every area due for them, so a holder who comes to hold
/// an area later, or a wide claim that widens, asks only about what is new.
pub fn asks_due(
    claims: &[Claim],
    grants: &[Grant],
    asked: &[WideAsk],
    blocks: &[GrantBlock],
    now: OffsetDateTime,
    stale_after: Duration,
) -> Vec<AskDue> {
    let live: Vec<&Claim> = claims
        .iter()
        .filter(|claim| claim.is_live(now, stale_after))
        .collect();
    let mut due: BTreeMap<(ItemId, MemberId, MemberId), BTreeSet<QualifiedArea>> = BTreeMap::new();
    for wide_claim in &live {
        let Some(scope) = &wide_claim.wide else {
            continue;
        };
        for holder in live
            .iter()
            .filter(|claim| claim.member != wide_claim.member)
        {
            for area in holder.areas.intersection(scope.areas()) {
                let granted = grants.iter().any(|grant| {
                    grant.item == wide_claim.item
                        && grant.area == *area
                        && grant.from == holder.member
                        && grant.to == wide_claim.member
                });
                let already = asked.iter().any(|ask| {
                    ask.acked_by_dibs.is_none()
                        && ask.item == wide_claim.item
                        && ask.from == wide_claim.member
                        && ask.to == holder.member
                        && ask.areas.contains(area)
                });
                let blocked = blocks
                    .iter()
                    .any(|block| block.applies(wide_claim.member, holder.member, area, claims));
                if !granted && !already && !blocked {
                    due.entry((wide_claim.item, wide_claim.member, holder.member))
                        .or_default()
                        .insert(area.clone());
                }
            }
        }
    }
    due.into_iter()
        .map(|((item, from, to), areas)| AskDue {
            item,
            from,
            to,
            areas,
        })
        .collect()
}

/// Whether an announcement still stands: its member holds a wide claim on its item. Once that
/// claim ends (released, completed, taken over), the announcement asks for nothing: it is no
/// longer delivered or listed as awaiting an acknowledgement, and acknowledging it grants nothing.
pub fn ask_stands(ask: &WideAsk, claims: &[Claim]) -> bool {
    announced_scope(ask, claims).is_some()
}

/// Whether an announcement still waits for its holder's acknowledgement, as far as the claims and
/// dibs tell: it stands ([`ask_stands`]) and dibs did not acknowledge it on the holder's behalf.
/// Whether the holder acknowledged it is not judged here. An announcement dibs acknowledged is not
/// delivered or listed as awaiting an acknowledgement, and cannot be escalated as unacknowledged,
/// although its wide claim has not ended.
pub fn ask_awaits(ask: &WideAsk, claims: &[Claim]) -> bool {
    ask.acked_by_dibs.is_none() && ask_stands(ask, claims)
}

/// The scope of the wide claim an announcement announced, while its member holds it.
fn announced_scope<'a>(ask: &WideAsk, claims: &'a [Claim]) -> Option<&'a WideScope> {
    claims
        .iter()
        .find(|claim| claim.item == ask.item && claim.member == ask.from)
        .and_then(|claim| claim.wide.as_ref())
}

/// The grants an acknowledgement of `ask` by its holder makes: one per area it named that the
/// wide claim still names, decided by the rules of every grant — the holder holds the area through
/// a live claim, and the wide claim's member holds a live claim on its item. An area those rules
/// refuse now, or that is granted already, gives nothing; the acknowledgement stands either way.
pub fn grants_by_ack(
    ask: &WideAsk,
    existing: &[Grant],
    claims: &[Claim],
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> Vec<Grant> {
    let Some(scope) = announced_scope(ask, claims) else {
        return Vec::new();
    };
    ask.areas
        .iter()
        .filter(|area| scope.names(area))
        .filter_map(|area| {
            let request = Grant {
                item: ask.item,
                area: area.clone(),
                from: ask.to,
                to: ask.from,
            };
            match decide_grant(&request, existing, claims, now, settings) {
                Ok(GrantOutcome::New(grant)) => Some(grant),
                Ok(GrantOutcome::AlreadyGranted) | Err(_) => None,
            }
        })
        .collect()
}

/// The reasons dibs acknowledges an announcement for at once, in the order that names the
/// acknowledgement when its areas give different ones: the reason latest in this list wins.
const REASONS_LATEST_WINS: [GrantDecision; 3] = [
    GrantDecision::Untouched,
    GrantDecision::Idle,
    GrantDecision::InReview,
];

/// The areas of an announcement dibs judges, each with the holder's live claim that holds it, in
/// area order: the areas it asked about that its wide claim, still live, names and that its holder
/// still holds through a live claim. An area the holder no longer holds needs no grant and is left
/// out. Empty once the wide claim has ended or gone stale.
pub fn judged_areas<'a>(
    ask: &WideAsk,
    claims: &'a [Claim],
    now: OffsetDateTime,
    stale_after: Duration,
) -> Vec<(QualifiedArea, &'a Claim)> {
    areas_left(ask, claims, |claim| claim.is_live(now, stale_after))
}

/// The areas of an announcement that claims `counts` keeps can still be judged by, each with the
/// holder's claim that holds it, in area order: the areas it asked about that its wide claim, kept,
/// names and that its holder holds through a kept claim.
fn areas_left<'a>(
    ask: &WideAsk,
    claims: &'a [Claim],
    counts: impl Fn(&Claim) -> bool,
) -> Vec<(QualifiedArea, &'a Claim)> {
    let kept = |claim: &&Claim| counts(claim);
    let Some(scope) = claims
        .iter()
        .filter(kept)
        .find(|claim| claim.item == ask.item && claim.member == ask.from)
        .and_then(|claim| claim.wide.as_ref())
    else {
        return Vec::new();
    };
    ask.areas
        .iter()
        .filter(|area| scope.names(area))
        .filter_map(|area| {
            claims
                .iter()
                .filter(kept)
                .find(|claim| claim.member == ask.to && claim.holds(area))
                .map(|holding| (area.clone(), holding))
        })
        .collect()
}

/// What dibs stores that bears on acknowledging an announcement on its holder's behalf.
#[derive(Debug, Clone, Copy)]
pub struct WideAckFacts<'a> {
    /// The project's claims.
    pub claims: &'a [Claim],
    /// The stored footprints of the holder's claims that hold the judged areas, by the claim's
    /// item. A claim with none stored has no entry.
    pub footprints: &'a BTreeMap<ItemId, Vec<FootprintPaths<'a>>>,
    /// The revokes that stick.
    pub blocks: &'a [GrantBlock],
}

/// What dibs does with an announcement on its holder's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckOnBehalf {
    /// Acknowledge it now, as the holder's acknowledgement would, for this reason.
    Ack {
        /// Why dibs acknowledges it.
        because: GrantDecision,
    },
    /// Nothing lets dibs acknowledge it yet; at `until` the wait does.
    Wait {
        /// When the announcement has waited the project's `grant_after`.
        until: OffsetDateTime,
    },
    /// A revoke between the wide claim's member and the holder in one of its areas sticks: dibs
    /// never acknowledges it, and the holder may.
    LeftToHolder,
    /// No area is left to judge now ([`judged_areas`]), but a stale claim still keeps one — the
    /// wide claim, or the holder's claim that names the area, went stale — and may be live again:
    /// the announcement is left as it is, and judged again later.
    Lapsed,
    /// No claim, live or stale, leaves an area to judge: the wide claim ended, or its holder holds
    /// none of the areas it still names. dibs stops judging it for good; the holder may still
    /// acknowledge it.
    Closed,
}

/// Decides whether dibs acknowledges an announcement, sent at `sent_at`, on its holder's behalf,
/// from stored facts only.
///
/// With no area to judge ([`judged_areas`]), nothing is decided: the announcement is **closed**
/// when no claim, live or stale, leaves one either — its wide claim ended (released, completed,
/// taken over), or its holder holds none of the areas it asked about that the wide claim names, by
/// any claim — and has **lapsed** while only stale claims keep one, so an announcement only stale
/// claims keep is never closed. When a revoke between the wide
/// claim's member and the holder sticks in any judged area ([`GrantBlock::applies`]), it is left
/// to the holder. Otherwise each judged area is judged by [`judge_area`] against the holder's
/// claim that holds it, with that claim's footprints: when every area gives a reason, dibs
/// acknowledges it at once, naming the reason that comes latest in the order `untouched`, `idle`,
/// `in_review` — so one area in review names the acknowledgement `in_review`, and one idle area
/// beside untouched ones names it `idle`; when any area gives none, it waits until it has
/// waited the project's `grant_after`, and is acknowledged from then on (`waited`).
pub fn decide_ack_on_holders_behalf(
    ask: &WideAsk,
    sent_at: OffsetDateTime,
    facts: &WideAckFacts<'_>,
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> AckOnBehalf {
    let judged = judged_areas(ask, facts.claims, now, settings.stale_after());
    if judged.is_empty() {
        return if areas_left(ask, facts.claims, |_| true).is_empty() {
            AckOnBehalf::Closed
        } else {
            AckOnBehalf::Lapsed
        };
    }
    if judged.iter().any(|(area, _)| {
        facts
            .blocks
            .iter()
            .any(|block| block.applies(ask.from, ask.to, area, facts.claims))
    }) {
        return AckOnBehalf::LeftToHolder;
    }
    let reasons: Option<Vec<GrantDecision>> = judged
        .iter()
        .map(|(area, holding)| {
            let footprints = facts
                .footprints
                .get(&holding.item)
                .map_or(&[][..], Vec::as_slice);
            judge_area(holding, area, footprints, settings, now)
        })
        .collect();
    if let Some(because) = reasons.and_then(|reasons| {
        reasons.into_iter().max_by_key(|reason| {
            REASONS_LATEST_WINS
                .iter()
                .position(|listed| listed == reason)
        })
    }) {
        return AckOnBehalf::Ack { because };
    }
    let until = sent_at + settings.grant_after();
    if now >= until {
        AckOnBehalf::Ack {
            because: GrantDecision::Waited,
        }
    } else {
        AckOnBehalf::Wait { until }
    }
}

/// Where one area of a wide claim stands for its member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaStanding {
    /// The area.
    pub area: QualifiedArea,
    /// The other member's live claim holding it, or `None` when nobody else holds it.
    pub holder: Option<Holder>,
    /// Whether that holder's grant to the wide claim's member is in force there.
    pub granted: bool,
}

/// Where each area of a wide claim stands, in area order: free, held by a member who granted the
/// edits, or held by a member who has not yet. Empty for a claim that is not wide.
pub fn standing(
    wide_claim: &Claim,
    claims: &[Claim],
    grants: &[Grant],
    now: OffsetDateTime,
    stale_after: Duration,
) -> Vec<AreaStanding> {
    let Some(scope) = &wide_claim.wide else {
        return Vec::new();
    };
    scope
        .areas()
        .iter()
        .map(|area| {
            let holder =
                crate::claim::held_by_other(area, wide_claim.member, claims, now, stale_after);
            let granted = holder.is_some_and(|holder| {
                grants.iter().any(|grant| {
                    grant.from == holder.member
                        && grant.to == wide_claim.member
                        && grant.area == *area
                        && grant.in_force(claims, now, stale_after)
                })
            });
            AreaStanding {
                area: area.clone(),
                holder: holder.map(Claim::holder),
                granted,
            }
        })
        .collect()
}

/// An area a claim holds that another member's live wide claim names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsideWide {
    /// The area.
    pub area: QualifiedArea,
    /// The wide claim's member and item.
    pub wide: Holder,
}

/// The areas `claim` holds that other members' live wide claims name, in area order: what its
/// member is told when they claim, since the wide claim's member will ask them for the edits there.
pub fn inside_wide(
    claim: &Claim,
    claims: &[Claim],
    now: OffsetDateTime,
    stale_after: Duration,
) -> Vec<InsideWide> {
    claim
        .areas
        .iter()
        .filter_map(|area| {
            live_wide_naming(area, claims, now, stale_after)
                .filter(|wide| wide.member != claim.member)
                .map(|wide| InsideWide {
                    area: area.clone(),
                    wide: wide.holder(),
                })
        })
        .collect()
}

/// The live wide claim naming the area; the lowest item when, against the rules, several do.
pub fn live_wide_naming<'a, I>(
    area: &QualifiedArea,
    claims: I,
    now: OffsetDateTime,
    stale_after: Duration,
) -> Option<&'a Claim>
where
    I: IntoIterator<Item = &'a Claim>,
{
    claims
        .into_iter()
        .filter(|claim| claim.is_live(now, stale_after) && claim.names_wide(area))
        .min_by_key(|claim| claim.item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ALICE, BOB, CAROL, area, areas, at, claim, two_repo_settings};

    fn reason() -> WideReason {
        WideReason::parse("Upgrade the HTTP client everywhere").unwrap()
    }

    fn wide(item: u64, member: MemberId, named: &[&str], minutes: i64) -> Claim {
        Claim {
            wide: Some(WideScope::new(areas(named), reason()).unwrap()),
            ..claim(item, member, &[], minutes)
        }
    }

    fn ask(message: u64, item: u64, from: MemberId, to: MemberId, named: &[&str]) -> WideAsk {
        WideAsk {
            message: MessageId(message),
            item: ItemId(item),
            from,
            to,
            areas: areas(named),
            acked_by_dibs: None,
        }
    }

    #[test]
    fn a_reason_is_one_to_three_hundred_characters_without_control_characters() {
        assert_eq!(WideReason::parse("  rename  ").unwrap().as_str(), "rename");
        assert!(WideReason::parse(&"r".repeat(300)).is_ok());
        for bad in ["", "  ", "two\nlines", &"r".repeat(301)] {
            assert_eq!(WideReason::parse(bad), Err(WideReasonError::Malformed));
        }
    }

    #[test]
    fn a_scope_names_at_least_one_area() {
        assert_eq!(
            WideScope::new(BTreeSet::new(), reason()),
            Err(WideScopeError::NoAreas)
        );
        let scope = WideScope::new(areas(&["api:billing"]), reason()).unwrap();
        assert!(scope.names(&area("api:billing")));
        assert!(!scope.names(&area("api:search")));
        assert_eq!(scope.reason(), &reason());
    }

    #[test]
    fn each_holder_is_asked_once_about_the_areas_they_hold_and_the_claimer_never() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing", "api:search", "web:checkout"], 0),
            claim(2, ALICE, &["api:billing", "api:search"], 0),
            claim(3, CAROL, &["web:checkout", "web:billing"], 0),
            claim(4, BOB, &["web:checkout"], 0),
        ];
        let due = asks_due(&claims, &[], &[], &[], at(1), settings.stale_after());
        assert_eq!(
            due,
            [
                AskDue {
                    item: ItemId(1),
                    from: BOB,
                    to: ALICE,
                    areas: areas(&["api:billing", "api:search"]),
                },
                AskDue {
                    item: ItemId(1),
                    from: BOB,
                    to: CAROL,
                    areas: areas(&["web:checkout"]),
                },
            ]
        );
    }

    fn in_review(held: Claim) -> Claim {
        use crate::review::{ClaimState, PrRef, Review};

        Claim {
            state: ClaimState::InReview(
                Review::new(
                    BTreeSet::from([PrRef::parse("acme/api#12").unwrap()]),
                    at(0),
                    false,
                )
                .unwrap(),
            ),
            ..held
        }
    }

    #[test]
    fn a_holder_in_review_is_asked_as_any_other_holder() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing"], 0),
            in_review(claim(2, ALICE, &["api:billing"], 0)),
        ];
        assert_eq!(
            asks_due(&claims, &[], &[], &[], at(1), settings.stale_after()),
            [AskDue {
                item: ItemId(1),
                from: BOB,
                to: ALICE,
                areas: areas(&["api:billing"]),
            }]
        );
    }

    #[test]
    fn an_area_dibs_acknowledged_is_announced_again_to_the_holders_new_claim() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing", "api:search"], 0),
            claim(3, ALICE, &["api:billing"], 20),
        ];
        let acked = WideAsk {
            acked_by_dibs: Some(GrantDecision::Untouched),
            ..ask(9, 1, BOB, ALICE, &["api:billing", "api:search"])
        };
        let due = asks_due(
            &claims,
            &[],
            std::slice::from_ref(&acked),
            &[],
            at(21),
            settings.stale_after(),
        );
        assert_eq!(
            due,
            [AskDue {
                item: ItemId(1),
                from: BOB,
                to: ALICE,
                areas: areas(&["api:billing"]),
            }],
            "the grants of the acknowledgement ended with the holder's old claim, so the new claim \
             over billing is asked again"
        );
    }

    #[test]
    fn an_area_dibs_acknowledged_whose_grant_was_revoked_is_not_announced_again() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing", "api:search"], 0),
            claim(2, ALICE, &["api:billing", "api:search"], 0),
        ];
        let acked = WideAsk {
            acked_by_dibs: Some(GrantDecision::Untouched),
            ..ask(9, 1, BOB, ALICE, &["api:billing"])
        };
        let revoked = GrantBlock {
            asker: BOB,
            asker_item: ItemId(1),
            asker_claimed_at: at(0),
            holder: ALICE,
            holder_item: ItemId(2),
            holder_claimed_at: at(0),
            area: area("api:billing"),
            revoked_at: at(1),
        };
        let due = asks_due(
            &claims,
            &[],
            std::slice::from_ref(&acked),
            std::slice::from_ref(&revoked),
            at(1),
            settings.stale_after(),
        );
        assert_eq!(
            due.iter().map(|ask| ask.areas.clone()).collect::<Vec<_>>(),
            [areas(&["api:search"])],
            "the revoke sticks between these claims"
        );
        let granted = [Grant {
            item: ItemId(1),
            area: area("api:billing"),
            from: ALICE,
            to: BOB,
        }];
        let due = asks_due(
            &claims,
            &granted,
            std::slice::from_ref(&acked),
            &[],
            at(1),
            settings.stale_after(),
        );
        assert_eq!(
            due.iter().map(|ask| ask.areas.clone()).collect::<Vec<_>>(),
            [areas(&["api:search"])],
            "a grant still in force leaves nothing due"
        );
    }

    #[test]
    fn a_revoke_that_sticks_keeps_its_area_from_being_announced_again() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing", "api:search"], 0),
            claim(2, ALICE, &["api:billing", "api:search"], 0),
        ];
        let block = |claimed_at| GrantBlock {
            asker: BOB,
            asker_item: ItemId(1),
            asker_claimed_at: at(0),
            holder: ALICE,
            holder_item: ItemId(2),
            holder_claimed_at: claimed_at,
            area: area("api:billing"),
            revoked_at: at(1),
        };
        let due = asks_due(
            &claims,
            &[],
            &[],
            &[block(at(0))],
            at(1),
            settings.stale_after(),
        );
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].areas, areas(&["api:search"]));
        let earlier_claim = asks_due(
            &claims,
            &[],
            &[],
            &[block(at(-5))],
            at(1),
            settings.stale_after(),
        );
        assert_eq!(
            earlier_claim[0].areas,
            areas(&["api:billing", "api:search"]),
            "a revoke between other claims of the same members does not stick"
        );
    }

    #[test]
    fn an_open_ask_or_a_grant_is_not_asked_again_and_only_what_is_new_is() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing", "api:search"], 0),
            claim(2, ALICE, &["api:billing", "api:search"], 0),
        ];
        let open = [ask(9, 1, BOB, ALICE, &["api:billing"])];
        let due = asks_due(&claims, &[], &open, &[], at(1), settings.stale_after());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].areas, areas(&["api:search"]));
        let granted = [Grant {
            item: ItemId(1),
            area: area("api:search"),
            from: ALICE,
            to: BOB,
        }];
        assert!(asks_due(&claims, &granted, &open, &[], at(1), settings.stale_after()).is_empty());
    }

    #[test]
    fn stale_claims_neither_ask_nor_are_asked() {
        let settings = two_repo_settings();
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        let stale_wide = [
            wide(1, BOB, &["api:billing"], 0),
            claim(2, ALICE, &["api:billing"], 60 * 24),
        ];
        assert!(asks_due(&stale_wide, &[], &[], &[], late, settings.stale_after()).is_empty());
        let stale_holder = [
            wide(1, BOB, &["api:billing"], 60 * 24),
            claim(2, ALICE, &["api:billing"], 0),
        ];
        assert!(asks_due(&stale_holder, &[], &[], &[], late, settings.stale_after()).is_empty());
    }

    #[test]
    fn an_announcement_stands_while_its_member_holds_the_wide_claim_on_its_item() {
        let settings = two_repo_settings();
        let asked = ask(9, 1, BOB, ALICE, &["api:billing"]);
        let holder = claim(2, ALICE, &["api:billing"], 0);
        let live = [wide(1, BOB, &["api:billing"], 0), holder.clone()];
        assert!(ask_stands(&asked, &live));
        let ended = [holder.clone()];
        assert!(!ask_stands(&asked, &ended), "the wide claim was released");
        let taken_over = [wide(1, CAROL, &["api:billing"], 0), holder.clone()];
        assert!(
            !ask_stands(&asked, &taken_over),
            "another member holds the item"
        );
        let narrowed = [claim(1, BOB, &["api:search"], 0), holder];
        assert!(
            !ask_stands(&asked, &narrowed),
            "the claim is no longer wide"
        );
        assert!(
            grants_by_ack(&asked, &[], &ended, at(1), &settings).is_empty(),
            "an ended announcement grants nothing"
        );
    }

    #[test]
    fn an_announcement_awaits_while_it_stands_and_dibs_did_not_acknowledge_it() {
        let asked = ask(9, 1, BOB, ALICE, &["api:billing"]);
        let holder = claim(2, ALICE, &["api:billing"], 0);
        let live = [wide(1, BOB, &["api:billing"], 0), holder.clone()];
        assert!(ask_awaits(&asked, &live));
        assert!(
            !ask_awaits(&asked, std::slice::from_ref(&holder)),
            "the wide claim ended"
        );
        for because in [
            GrantDecision::Untouched,
            GrantDecision::InReview,
            GrantDecision::Idle,
            GrantDecision::Waited,
        ] {
            let acked = WideAsk {
                acked_by_dibs: Some(because),
                ..asked.clone()
            };
            assert!(
                !ask_awaits(&acked, &live),
                "dibs acknowledged it: {because:?}"
            );
            assert!(
                ask_stands(&acked, &live),
                "an announcement dibs acknowledged still stands: its wide claim did not end"
            );
            assert!(!ask_awaits(&acked, std::slice::from_ref(&holder)));
        }
    }

    #[test]
    fn an_ack_grants_the_areas_its_holder_still_holds_for_the_wide_item() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing", "api:search"], 0),
            claim(2, ALICE, &["api:billing"], 0),
        ];
        let asked = ask(9, 1, BOB, ALICE, &["api:billing", "api:search"]);
        let grants = grants_by_ack(&asked, &[], &claims, at(1), &settings);
        assert_eq!(
            grants,
            [Grant {
                item: ItemId(1),
                area: area("api:billing"),
                from: ALICE,
                to: BOB,
            }],
            "search is no longer Alice's to grant"
        );
        assert!(
            grants_by_ack(&asked, &grants, &claims, at(1), &settings).is_empty(),
            "what is granted is not granted twice"
        );
    }

    #[test]
    fn an_ack_grants_nothing_once_the_wide_claim_is_gone_or_another_members() {
        let settings = two_repo_settings();
        let asked = ask(9, 1, BOB, ALICE, &["api:billing"]);
        let holder = claim(2, ALICE, &["api:billing"], 0);
        let gone = [holder.clone()];
        assert!(grants_by_ack(&asked, &[], &gone, at(1), &settings).is_empty());
        let taken = [wide(1, CAROL, &["api:billing"], 0), holder.clone()];
        assert!(grants_by_ack(&asked, &[], &taken, at(1), &settings).is_empty());
        let narrowed = [
            Claim {
                wide: None,
                ..wide(1, BOB, &["api:billing"], 0)
            },
            holder,
        ];
        assert!(grants_by_ack(&asked, &[], &narrowed, at(1), &settings).is_empty());
    }

    #[test]
    fn a_grant_by_ack_ends_with_either_claim() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing"], 0),
            claim(2, ALICE, &["api:billing"], 0),
        ];
        let asked = ask(9, 1, BOB, ALICE, &["api:billing"]);
        let grant = grants_by_ack(&asked, &[], &claims, at(1), &settings).remove(0);
        let stale_after = settings.stale_after();
        assert!(grant.in_force(&claims, at(1), stale_after));
        assert_eq!(
            grant.ending(&claims[1..]),
            Some(crate::event::GrantEnd::GranteeLeft)
        );
        assert_eq!(
            grant.ending(&claims[..1]),
            Some(crate::event::GrantEnd::GrantorLeft)
        );
    }

    #[test]
    fn the_standing_of_each_area_says_free_granted_or_waiting() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing", "api:search", "web:checkout"], 0),
            claim(2, ALICE, &["api:billing"], 0),
            claim(3, CAROL, &["api:search"], 0),
        ];
        let grants = [Grant {
            item: ItemId(1),
            area: area("api:billing"),
            from: ALICE,
            to: BOB,
        }];
        let rows = standing(&claims[0], &claims, &grants, at(1), settings.stale_after());
        let summary: Vec<(String, Option<MemberId>, bool)> = rows
            .iter()
            .map(|row| {
                (
                    row.area.to_string(),
                    row.holder.map(|holder| holder.member),
                    row.granted,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("api:billing".to_owned(), Some(ALICE), true),
                ("api:search".to_owned(), Some(CAROL), false),
                ("web:checkout".to_owned(), None, false),
            ]
        );
        assert!(standing(&claims[1], &claims, &grants, at(1), settings.stale_after()).is_empty());
    }

    #[test]
    fn a_claim_is_told_which_of_its_areas_another_members_wide_claim_names() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing"], 0),
            wide(5, ALICE, &["api:search"], 0),
            claim(2, ALICE, &["api:billing", "api:search"], 0),
        ];
        assert_eq!(
            inside_wide(&claims[2], &claims, at(1), settings.stale_after()),
            [InsideWide {
                area: area("api:billing"),
                wide: Holder {
                    member: BOB,
                    item: ItemId(1)
                },
            }],
            "her own wide claim tells her nothing"
        );
    }

    /// The claims of a wide claim by Bob on item 1 over `named`, and Alice's claim on item 2
    /// holding `held`, both claimed at the fixed instant.
    fn announced_to_alice(named: &[&str], held: &[&str]) -> Vec<Claim> {
        vec![wide(1, BOB, named, 0), claim(2, ALICE, held, 0)]
    }

    fn api() -> crate::repo::RepoId {
        crate::repo::RepoId::parse("api").unwrap()
    }

    fn paths(texts: &[&str]) -> BTreeSet<crate::paths::RelPath> {
        texts
            .iter()
            .map(|text| crate::paths::RelPath::parse(text).unwrap())
            .collect()
    }

    fn decide_ack(
        asked: &WideAsk,
        claims: &[Claim],
        footprints: &BTreeMap<ItemId, Vec<FootprintPaths<'_>>>,
        blocks: &[GrantBlock],
        now: OffsetDateTime,
    ) -> AckOnBehalf {
        let facts = WideAckFacts {
            claims,
            footprints,
            blocks,
        };
        decide_ack_on_holders_behalf(asked, at(1), &facts, &two_repo_settings(), now)
    }

    #[test]
    fn dibs_acknowledges_at_once_when_the_holders_branch_changes_none_of_the_areas() {
        let claims = announced_to_alice(
            &["api:billing", "api:search"],
            &["api:billing", "api:search"],
        );
        let asked = ask(9, 1, BOB, ALICE, &["api:billing", "api:search"]);
        let changed = paths(&["README.md"]);
        let repo = api();
        let footprints = BTreeMap::from([(
            ItemId(2),
            vec![FootprintPaths {
                repo: &repo,
                paths: &changed,
                truncated: false,
            }],
        )]);
        assert_eq!(
            decide_ack(&asked, &claims, &footprints, &[], at(2)),
            AckOnBehalf::Ack {
                because: GrantDecision::Untouched
            }
        );
    }

    #[test]
    fn one_touched_area_waits_until_the_grant_delay_has_passed() {
        let claims = announced_to_alice(
            &["api:billing", "api:search"],
            &["api:billing", "api:search"],
        );
        let asked = ask(9, 1, BOB, ALICE, &["api:billing", "api:search"]);
        let changed = paths(&["packages/search/index.rs"]);
        let repo = api();
        let footprints = BTreeMap::from([(
            ItemId(2),
            vec![FootprintPaths {
                repo: &repo,
                paths: &changed,
                truncated: false,
            }],
        )]);
        assert_eq!(
            decide_ack(&asked, &claims, &footprints, &[], at(2)),
            AckOnBehalf::Wait { until: at(11) }
        );
        assert_eq!(
            decide_ack(&asked, &claims, &footprints, &[], at(11)),
            AckOnBehalf::Ack {
                because: GrantDecision::Waited
            }
        );
    }

    #[test]
    fn a_holder_in_review_is_acknowledged_for_at_once_and_review_names_a_mixed_acknowledgement() {
        let claims = [
            wide(1, BOB, &["api:billing", "web:checkout"], 0),
            in_review(claim(2, ALICE, &["api:billing"], 0)),
            claim(3, ALICE, &["web:checkout"], 0),
        ];
        let asked = ask(9, 1, BOB, ALICE, &["api:billing", "web:checkout"]);
        let changed = paths(&["src/billing/invoice.ts"]);
        let web = crate::repo::RepoId::parse("web").unwrap();
        let footprints = BTreeMap::from([(
            ItemId(3),
            vec![FootprintPaths {
                repo: &web,
                paths: &changed,
                truncated: false,
            }],
        )]);
        assert_eq!(
            decide_ack(&asked, &claims, &footprints, &[], at(2)),
            AckOnBehalf::Ack {
                because: GrantDecision::InReview
            },
            "checkout is untouched by item 3, billing's claim is in review"
        );
        assert_eq!(
            decide_ack(&asked, &claims, &BTreeMap::new(), &[], at(2)),
            AckOnBehalf::Wait { until: at(11) },
            "without item 3's footprint checkout gives no reason"
        );
    }

    #[test]
    fn an_idle_area_beside_an_untouched_one_names_the_acknowledgement_idle() {
        const WEEK: i64 = 7 * 24 * 60;
        let mut claims = vec![
            wide(1, BOB, &["api:billing", "web:checkout"], 0),
            claim(2, ALICE, &["api:billing"], 0),
            claim(3, ALICE, &["web:checkout"], WEEK - 10),
        ];
        for one in &mut claims {
            one.last_seen = at(WEEK);
        }
        let asked = ask(9, 1, BOB, ALICE, &["api:billing", "web:checkout"]);
        let changed = paths(&["src/billing/invoice.ts"]);
        let web = crate::repo::RepoId::parse("web").unwrap();
        let footprints = BTreeMap::from([(
            ItemId(3),
            vec![FootprintPaths {
                repo: &web,
                paths: &changed,
                truncated: false,
            }],
        )]);
        assert_eq!(
            decide_ack(&asked, &claims, &footprints, &[], at(WEEK + 1)),
            AckOnBehalf::Ack {
                because: GrantDecision::Idle
            },
            "billing lies idle with no footprint in api, checkout, claimed lately, is untouched in web"
        );
    }

    #[test]
    fn a_revoke_that_sticks_in_one_area_leaves_the_announcement_to_the_holder() {
        let claims = in_review_holder();
        let asked = ask(9, 1, BOB, ALICE, &["api:billing", "api:search"]);
        let block = GrantBlock {
            asker: BOB,
            asker_item: ItemId(1),
            asker_claimed_at: at(0),
            holder: ALICE,
            holder_item: ItemId(2),
            holder_claimed_at: at(0),
            area: area("api:search"),
            revoked_at: at(1),
        };
        assert_eq!(
            decide_ack(&asked, &claims, &BTreeMap::new(), &[block], at(60)),
            AckOnBehalf::LeftToHolder,
            "even in review and past the grant delay"
        );
    }

    fn in_review_holder() -> Vec<Claim> {
        vec![
            wide(1, BOB, &["api:billing", "api:search"], 0),
            in_review(claim(2, ALICE, &["api:billing", "api:search"], 0)),
        ]
    }

    #[test]
    fn only_the_areas_the_wide_claim_names_and_the_holder_holds_are_judged() {
        let claims = announced_to_alice(&["api:billing"], &["api:search"]);
        let asked = ask(9, 1, BOB, ALICE, &["api:billing", "api:search"]);
        assert!(
            judged_areas(&asked, &claims, at(1), two_repo_settings().stale_after()).is_empty(),
            "billing is no longer Alice's, search no longer the wide claim's"
        );
        assert_eq!(
            decide_ack(&asked, &claims, &BTreeMap::new(), &[], at(60)),
            AckOnBehalf::Closed,
            "not even the wait acknowledges it: no area is left to judge, by any claim"
        );
        let narrower = announced_to_alice(&["api:billing", "api:search"], &["api:billing"]);
        let judged = judged_areas(&asked, &narrower, at(1), two_repo_settings().stale_after());
        assert_eq!(
            judged
                .iter()
                .map(|(area, holding)| (area.to_string(), holding.item))
                .collect::<Vec<_>>(),
            [("api:billing".to_owned(), ItemId(2))]
        );
    }

    #[test]
    fn an_announcement_only_stale_claims_keep_is_not_judged_now_and_not_closed() {
        let settings = two_repo_settings();
        let asked = ask(9, 1, BOB, ALICE, &["api:billing"]);
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        let mut stale_wide = announced_to_alice(&["api:billing"], &["api:billing"]);
        stale_wide[1].last_seen = late;
        assert_eq!(
            decide_ack(&asked, &stale_wide, &BTreeMap::new(), &[], late),
            AckOnBehalf::Lapsed,
            "the wide claim went stale"
        );
        let mut stale_holder = announced_to_alice(&["api:billing"], &["api:billing"]);
        stale_holder[0].last_seen = late;
        assert_eq!(
            decide_ack(&asked, &stale_holder, &BTreeMap::new(), &[], late),
            AckOnBehalf::Lapsed,
            "the holder's claim went stale, and still names the area"
        );
    }

    #[test]
    fn an_announcement_whose_wide_claim_ended_or_whose_holder_holds_none_of_its_areas_is_closed() {
        let asked = ask(9, 1, BOB, ALICE, &["api:billing"]);
        let ended = [claim(2, ALICE, &["api:billing"], 0)];
        assert_eq!(
            decide_ack(&asked, &ended, &BTreeMap::new(), &[], at(60)),
            AckOnBehalf::Closed,
            "the wide claim ended"
        );
        let released = [wide(1, BOB, &["api:billing"], 0)];
        assert_eq!(
            decide_ack(&asked, &released, &BTreeMap::new(), &[], at(60)),
            AckOnBehalf::Closed,
            "the holder released"
        );
        let narrowed = announced_to_alice(&["api:billing"], &["api:search"]);
        assert_eq!(
            decide_ack(&asked, &narrowed, &BTreeMap::new(), &[], at(60)),
            AckOnBehalf::Closed,
            "the holder narrowed the area away"
        );
        let late = at(0) + two_repo_settings().stale_after() + Duration::minutes(1);
        let mut stale_and_released = vec![
            wide(1, BOB, &["api:billing"], 0),
            claim(2, ALICE, &["api:search"], 0),
        ];
        stale_and_released[1].last_seen = late;
        assert_eq!(
            decide_ack(&asked, &stale_and_released, &BTreeMap::new(), &[], late),
            AckOnBehalf::Closed,
            "a stale wide claim keeps nothing when its holder holds none of the areas"
        );
    }
}
