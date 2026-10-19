//! Grants: a holder's consent that another member may edit inside an area they hold.

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::area::QualifiedArea;
use crate::claim::Claim;
use crate::event::GrantEnd;
use crate::ids::{ItemId, MemberId};
use crate::settings::ProjectSettings;

/// A holder's consent that `to` may edit inside `area` for the work item `item` — the grantee's
/// item, the work that needs the edit.
///
/// A grant is in force while `from` still holds the area through a live claim and `to`'s claim on
/// `item` is live. It ends, and its record is removed, when either stops or `from` revokes it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Grant {
    /// The grantee's work item.
    pub item: ItemId,
    /// The area.
    pub area: QualifiedArea,
    /// The holder who grants.
    pub from: MemberId,
    /// The member who may edit.
    pub to: MemberId,
}

impl Grant {
    /// Whether the grant is in force among these claims.
    pub fn in_force(&self, claims: &[Claim], now: OffsetDateTime, stale_after: Duration) -> bool {
        let live = |claim: &&Claim| claim.is_live(now, stale_after);
        let grantor_holds = claims
            .iter()
            .filter(live)
            .any(|claim| claim.member == self.from && claim.holds(&self.area));
        let grantee_works = claims
            .iter()
            .filter(live)
            .any(|claim| claim.member == self.to && claim.item == self.item);
        grantor_holds && grantee_works
    }

    /// Why the grant no longer rests on these claims, or `None` while it does.
    ///
    /// A grant rests on the grantee's claim on its item and on a claim of the grantor declaring its
    /// area. It ends when either is gone — released, completed, taken over, forced, or the area no
    /// longer declared — and the grantee's side is looked at first. Liveness does not matter here: a
    /// claim that went stale suspends the grant ([`Grant::in_force`] reads it as not in force) without
    /// ending it.
    pub fn ending(&self, claims: &[Claim]) -> Option<GrantEnd> {
        let grantee_works = claims
            .iter()
            .any(|claim| claim.member == self.to && claim.item == self.item);
        let grantor_holds = claims
            .iter()
            .any(|claim| claim.member == self.from && claim.holds(&self.area));
        if !grantee_works {
            Some(GrantEnd::GranteeLeft)
        } else if !grantor_holds {
            Some(GrantEnd::GrantorLeft)
        } else {
            None
        }
    }
}

impl AsRef<Grant> for Grant {
    fn as_ref(&self) -> &Grant {
        self
    }
}

/// The grants, in the order given, that no longer rest on these claims, each with why it ended (see
/// [`Grant::ending`]). Takes anything that holds a grant, so a caller keeping grants with their ids
/// gets its own records back.
pub fn orphaned<'a, G: AsRef<Grant>>(grants: &'a [G], claims: &[Claim]) -> Vec<(&'a G, GrantEnd)> {
    grants
        .iter()
        .filter_map(|held| held.as_ref().ending(claims).map(|end| (held, end)))
        .collect()
}

/// What a successful grant did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantOutcome {
    /// The grant to store.
    New(Grant),
    /// The same grant exists; nothing changes.
    AlreadyGranted,
}

/// Why a grant is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GrantRefused {
    /// A member grants to themselves.
    #[error("a member cannot grant to themselves")]
    SelfGrant,
    /// The settings declare no such area.
    #[error("the project declares no area {area}")]
    UnknownArea {
        /// The area.
        area: QualifiedArea,
    },
    /// The granting member holds no live claim on the area.
    #[error("only the member holding {area} may grant edits in it")]
    NotHolder {
        /// The area.
        area: QualifiedArea,
    },
    /// The grantee holds no live claim on the item.
    #[error("the member being granted holds no live claim on that item")]
    GranteeHasNoClaim,
}

/// Decides a grant.
///
/// Only a member whose live claim holds the area may grant, never to themselves, and only to a
/// member holding a live claim on the grant's item. Granting what is already granted changes
/// nothing.
///
/// # Errors
/// [`GrantRefused`] naming the rule broken.
pub fn decide_grant(
    request: &Grant,
    existing: &[Grant],
    claims: &[Claim],
    now: OffsetDateTime,
    settings: &ProjectSettings,
) -> Result<GrantOutcome, GrantRefused> {
    if request.from == request.to {
        return Err(GrantRefused::SelfGrant);
    }
    if settings.area(&request.area).is_none() {
        return Err(GrantRefused::UnknownArea {
            area: request.area.clone(),
        });
    }
    let stale_after = settings.stale_after();
    let live = |claim: &&Claim| claim.is_live(now, stale_after);
    if !claims
        .iter()
        .filter(live)
        .any(|claim| claim.member == request.from && claim.holds(&request.area))
    {
        return Err(GrantRefused::NotHolder {
            area: request.area.clone(),
        });
    }
    if !claims
        .iter()
        .filter(live)
        .any(|claim| claim.member == request.to && claim.item == request.item)
    {
        return Err(GrantRefused::GranteeHasNoClaim);
    }
    if existing.contains(request) {
        return Ok(GrantOutcome::AlreadyGranted);
    }
    Ok(GrantOutcome::New(request.clone()))
}

/// Why a revocation is refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RevokeRefused {
    /// Only the member who granted may revoke.
    #[error("only the member who granted this may revoke it")]
    NotGrantor,
}

/// Decides a revocation, which only the grantor may make.
///
/// # Errors
/// [`RevokeRefused::NotGrantor`] for anyone else.
pub fn decide_revoke_grant(grant: &Grant, by: MemberId) -> Result<(), RevokeRefused> {
    if grant.from == by {
        Ok(())
    } else {
        Err(RevokeRefused::NotGrantor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ALICE, BOB, CAROL, area, at, claim, two_repo_settings};

    fn grant(item: u64, area_text: &str, from: MemberId, to: MemberId) -> Grant {
        Grant {
            item: ItemId(item),
            area: area(area_text),
            from,
            to,
        }
    }

    #[test]
    fn a_holder_grants_a_member_working_on_an_item() {
        let settings = two_repo_settings();
        let claims = [claim(1, ALICE, &["api:billing"], 0), claim(2, BOB, &[], 0)];
        let request = grant(2, "api:billing", ALICE, BOB);
        assert_eq!(
            decide_grant(&request, &[], &claims, at(1), &settings),
            Ok(GrantOutcome::New(request.clone()))
        );
        assert_eq!(
            decide_grant(
                &request,
                std::slice::from_ref(&request),
                &claims,
                at(1),
                &settings
            ),
            Ok(GrantOutcome::AlreadyGranted)
        );
    }

    #[test]
    fn grant_refusals() {
        let settings = two_repo_settings();
        let claims = [claim(1, ALICE, &["api:billing"], 0), claim(2, BOB, &[], 0)];
        let cases: &[(Grant, GrantRefused)] = &[
            (
                grant(1, "api:billing", ALICE, ALICE),
                GrantRefused::SelfGrant,
            ),
            (
                grant(2, "web:search", ALICE, BOB),
                GrantRefused::UnknownArea {
                    area: area("web:search"),
                },
            ),
            (
                grant(2, "api:search", ALICE, BOB),
                GrantRefused::NotHolder {
                    area: area("api:search"),
                },
            ),
            (
                grant(2, "api:billing", CAROL, BOB),
                GrantRefused::NotHolder {
                    area: area("api:billing"),
                },
            ),
            (
                grant(3, "api:billing", ALICE, BOB),
                GrantRefused::GranteeHasNoClaim,
            ),
            (
                grant(1, "api:billing", ALICE, BOB),
                GrantRefused::GranteeHasNoClaim,
            ),
        ];
        for (request, expected) in cases {
            assert_eq!(
                decide_grant(request, &[], &claims, at(1), &settings).as_ref(),
                Err(expected),
                "{request:?}"
            );
        }
    }

    #[test]
    fn a_stale_holder_cannot_grant() {
        let settings = two_repo_settings();
        let claims = [
            claim(1, ALICE, &["api:billing"], 0),
            claim(2, BOB, &[], 60 * 30),
        ];
        let late = at(0) + settings.stale_after() + Duration::minutes(1);
        assert!(matches!(
            decide_grant(
                &grant(2, "api:billing", ALICE, BOB),
                &[],
                &claims,
                late,
                &settings
            ),
            Err(GrantRefused::NotHolder { .. })
        ));
    }

    #[test]
    fn a_grant_ends_with_either_claim() {
        let settings = two_repo_settings();
        let stale_after = settings.stale_after();
        let g = grant(2, "api:billing", ALICE, BOB);
        let both = [claim(1, ALICE, &["api:billing"], 0), claim(2, BOB, &[], 0)];
        assert!(g.in_force(&both, at(1), stale_after));
        assert!(
            !g.in_force(&both[1..], at(1), stale_after),
            "grantor's claim ended"
        );
        assert!(
            !g.in_force(&both[..1], at(1), stale_after),
            "grantee's claim ended"
        );
        let narrowed = [claim(1, ALICE, &["api:search"], 0), claim(2, BOB, &[], 0)];
        assert!(
            !g.in_force(&narrowed, at(1), stale_after),
            "grantor no longer holds the area"
        );
        let late = at(0) + stale_after + Duration::minutes(1);
        let grantor_fresh = [
            claim(1, ALICE, &["api:billing"], 60 * 24),
            claim(2, BOB, &[], 0),
        ];
        assert!(
            !g.in_force(&grantor_fresh, late, stale_after),
            "grantee's claim went stale"
        );
    }

    #[test]
    fn only_the_grantor_revokes() {
        let g = grant(2, "api:billing", ALICE, BOB);
        assert_eq!(decide_revoke_grant(&g, ALICE), Ok(()));
        assert_eq!(decide_revoke_grant(&g, BOB), Err(RevokeRefused::NotGrantor));
    }

    #[test]
    fn a_grant_stands_while_both_claims_stand() {
        let grants = [grant(2, "api:billing", ALICE, BOB)];
        let claims = [claim(1, ALICE, &["api:billing"], 0), claim(2, BOB, &[], 0)];
        assert!(orphaned(&grants, &claims).is_empty());
        assert_eq!(grants[0].ending(&claims), None);
    }

    #[test]
    fn a_grant_ends_with_the_grantees_claim() {
        let grants = [grant(2, "api:billing", ALICE, BOB)];
        let claims = [claim(1, ALICE, &["api:billing"], 0)];
        assert_eq!(
            orphaned(&grants, &claims),
            vec![(&grants[0], GrantEnd::GranteeLeft)]
        );
    }

    #[test]
    fn a_grant_ends_when_the_grantor_declares_the_area_nowhere() {
        let grants = [grant(2, "api:billing", ALICE, BOB)];
        let claims = [claim(1, ALICE, &["api:search"], 0), claim(2, BOB, &[], 0)];
        assert_eq!(
            orphaned(&grants, &claims),
            vec![(&grants[0], GrantEnd::GrantorLeft)]
        );
    }

    #[test]
    fn the_grantees_side_is_named_when_both_are_gone() {
        let grants = [grant(2, "api:billing", ALICE, BOB)];
        assert_eq!(grants[0].ending(&[]), Some(GrantEnd::GranteeLeft));
    }

    #[test]
    fn another_claim_of_the_grantor_on_the_area_keeps_the_grant() {
        let grants = [grant(2, "api:billing", ALICE, BOB)];
        let claims = [claim(3, ALICE, &["api:billing"], 0), claim(2, BOB, &[], 0)];
        assert!(orphaned(&grants, &claims).is_empty());
    }

    #[test]
    fn a_claim_of_the_grantee_on_another_item_does_not_carry_the_grant() {
        let grants = [grant(2, "api:billing", ALICE, BOB)];
        let claims = [claim(1, ALICE, &["api:billing"], 0), claim(4, BOB, &[], 0)];
        assert_eq!(
            orphaned(&grants, &claims),
            vec![(&grants[0], GrantEnd::GranteeLeft)]
        );
    }

    #[test]
    fn a_stale_claim_suspends_a_grant_without_ending_it() {
        let grants = [grant(2, "api:billing", ALICE, BOB)];
        let claims = [claim(1, ALICE, &["api:billing"], 0), claim(2, BOB, &[], 0)];
        let stale_after = two_repo_settings().stale_after();
        let late = at(0) + stale_after + Duration::minutes(1);
        assert!(!grants[0].in_force(&claims, late, stale_after));
        assert!(orphaned(&grants, &claims).is_empty());
    }

    #[test]
    fn orphaned_returns_the_callers_own_records() {
        struct Stored {
            id: u64,
            grant: Grant,
        }
        impl AsRef<Grant> for Stored {
            fn as_ref(&self) -> &Grant {
                &self.grant
            }
        }
        let stored = [
            Stored {
                id: 7,
                grant: grant(2, "api:billing", ALICE, BOB),
            },
            Stored {
                id: 8,
                grant: grant(5, "api:billing", ALICE, CAROL),
            },
        ];
        let claims = [claim(1, ALICE, &["api:billing"], 0), claim(2, BOB, &[], 0)];
        let ended: Vec<(u64, GrantEnd)> = orphaned(&stored, &claims)
            .into_iter()
            .map(|(record, end)| (record.id, end))
            .collect();
        assert_eq!(ended, [(8, GrantEnd::GranteeLeft)]);
    }
}
