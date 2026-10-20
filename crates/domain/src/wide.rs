//! Wide claims: one claim announced over many areas for a change that cuts across them — a
//! dependency upgrade, a rename across packages, a hygiene pass over a dozen modules.
//!
//! A wide claim **holds none of the areas it names**, so it never stands in anyone's way: a member
//! claims such an area as if the wide claim were not there. What the wide claim gives is the
//! asking: every member whose live claim holds one of its areas is sent one announcement that asks
//! to be acknowledged, and a holder's acknowledgement of it is their grant, for the areas it named,
//! to the wide claim's member for the wide claim's item. Until a holder acknowledges, the wide
//! claimer's edits in their area are held. Two wide claims never name one area at a time.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::area::QualifiedArea;
use crate::claim::{Claim, Holder};
use crate::grant::{Grant, GrantOutcome, decide_grant};
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
/// when it was sent. It is answered by the holder acknowledging its message.
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
/// is asked — unless it is the wide claim's own member, they already granted that member edits in
/// the area for the wide claim's item, or an announcement of that wide claim still waits for their
/// acknowledgement about that area. `open` lists the announcements not acknowledged yet. One
/// announcement goes to each holder, naming every area due for them, so a holder who comes to hold
/// an area later, or a wide claim that widens, asks only about what is new.
pub fn asks_due(
    claims: &[Claim],
    grants: &[Grant],
    open: &[WideAsk],
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
                let asked = open.iter().any(|ask| {
                    ask.item == wide_claim.item
                        && ask.from == wide_claim.member
                        && ask.to == holder.member
                        && ask.areas.contains(area)
                });
                if !granted && !asked {
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
        let due = asks_due(&claims, &[], &[], at(1), settings.stale_after());
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

    #[test]
    fn an_open_ask_or_a_grant_is_not_asked_again_and_only_what_is_new_is() {
        let settings = two_repo_settings();
        let claims = [
            wide(1, BOB, &["api:billing", "api:search"], 0),
            claim(2, ALICE, &["api:billing", "api:search"], 0),
        ];
        let open = [ask(9, 1, BOB, ALICE, &["api:billing"])];
        let due = asks_due(&claims, &[], &open, at(1), settings.stale_after());
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].areas, areas(&["api:search"]));
        let granted = [Grant {
            item: ItemId(1),
            area: area("api:search"),
            from: ALICE,
            to: BOB,
        }];
        assert!(asks_due(&claims, &granted, &open, at(1), settings.stale_after()).is_empty());
    }

    #[test]
    fn stale_claims_neither_ask_nor_are_asked() {
        let settings = two_repo_settings();
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        let stale_wide = [
            wide(1, BOB, &["api:billing"], 0),
            claim(2, ALICE, &["api:billing"], 60 * 24),
        ];
        assert!(asks_due(&stale_wide, &[], &[], late, settings.stale_after()).is_empty());
        let stale_holder = [
            wide(1, BOB, &["api:billing"], 60 * 24),
            claim(2, ALICE, &["api:billing"], 0),
        ];
        assert!(asks_due(&stale_holder, &[], &[], late, settings.stale_after()).is_empty());
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
}
