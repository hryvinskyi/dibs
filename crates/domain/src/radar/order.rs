//! Which of two colliding claims lands first. When two claims' branches change one path, or one
//! changes a contract the other consumes, one of them merges and the other rebases onto it. The
//! rule settles which from what dibs stores: a claim in review lands before a claim at work; then
//! the item with the higher priority; then the older claim. Only when all three are equal does the
//! rule tie, and people decide.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::claim::Claim;
use crate::ids::ItemId;
use crate::item::Priority;

/// Why one claim lands before another: the first step of the rule that tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderRule {
    /// It is in review and the other is at work.
    InReview,
    /// Its item has the higher priority (the lower number).
    Priority,
    /// It was claimed earlier.
    Older,
}

/// Why a stored order does not make one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OrderError {
    /// The code is none of the names.
    #[error("an overlap's order is in_review, priority, older or tied")]
    Unknown,
    /// A tie that names an item landing first, or a rule that names none.
    #[error("an overlap's order names the item that lands first unless it is tied")]
    First,
}

impl OrderRule {
    /// Every rule, in the order the rule applies them: the first that tells two claims apart
    /// decides.
    pub const ALL: [Self; 3] = [Self::InReview, Self::Priority, Self::Older];

    /// The stable name: `in_review`, `priority` or `older`.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InReview => "in_review",
            Self::Priority => "priority",
            Self::Older => "older",
        }
    }

    /// The rule of this name.
    ///
    /// # Errors
    /// [`OrderError::Unknown`] for any other text, `tied` included.
    pub fn parse(code: &str) -> Result<Self, OrderError> {
        Self::ALL
            .into_iter()
            .find(|rule| rule.code() == code)
            .ok_or(OrderError::Unknown)
    }

    /// Why the claim that lands first does, for people: "it is in review".
    pub const fn why(self) -> &'static str {
        match self {
            Self::InReview => "it is in review",
            Self::Priority => "its work item has the higher priority",
            Self::Older => "its claim is older",
        }
    }

    /// How `a` stands against `b` under this rule alone: `Less` when `a` lands first, `Greater`
    /// when `b` does, `Equal` when the rule cannot tell them apart.
    fn compare(self, a: &Contender<'_>, b: &Contender<'_>) -> Ordering {
        match self {
            Self::InReview => b
                .claim
                .state
                .is_in_review()
                .cmp(&a.claim.state.is_in_review()),
            Self::Priority => a.priority.cmp(&b.priority),
            Self::Older => a.claim.claimed_at.cmp(&b.claim.claimed_at),
        }
    }
}

/// The order of two colliding claims: which item lands first and why, or a tie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OverlapOrder {
    /// The claim on `item` lands first, and the other rebases onto it once it merges.
    First {
        /// The item that lands first.
        item: ItemId,
        /// The step of the rule that told the claims apart.
        because: OrderRule,
    },
    /// Every step of the rule found the claims equal.
    Tied,
}

impl OverlapOrder {
    /// The stored name of a tie.
    pub const TIED: &'static str = "tied";

    /// The stable name: the code of the rule that decided, or `tied`.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::First { because, .. } => because.code(),
            Self::Tied => Self::TIED,
        }
    }

    /// The item that lands first; `None` for a tie.
    pub const fn first(&self) -> Option<ItemId> {
        match self {
            Self::First { item, .. } => Some(*item),
            Self::Tied => None,
        }
    }

    /// The order stored as `code` and the item that lands first.
    ///
    /// # Errors
    /// [`OrderError::Unknown`] when `code` names no rule and is not `tied`;
    /// [`OrderError::First`] when a tie names an item or a rule names none.
    pub fn from_parts(code: &str, first: Option<ItemId>) -> Result<Self, OrderError> {
        if code == Self::TIED {
            return match first {
                None => Ok(Self::Tied),
                Some(_) => Err(OrderError::First),
            };
        }
        let because = OrderRule::parse(code)?;
        first
            .map(|item| Self::First { item, because })
            .ok_or(OrderError::First)
    }

    /// Where `reader` stands in this order against `other`, the overlap's other item: first,
    /// second behind `other`, or tied. An order whose first item is neither is unknown.
    pub fn landing(&self, reader: ItemId, other: ItemId) -> Landing {
        match *self {
            Self::Tied => Landing::Tied,
            Self::First { item, because } if item == reader => Landing::First { because },
            Self::First { item, because } if item == other => Landing::Second {
                behind: other,
                because,
            },
            Self::First { .. } => Landing::Unknown,
        }
    }
}

/// Where one item of an overlap stands in its order, as its holder is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// It lands first.
    First {
        /// Why.
        because: OrderRule,
    },
    /// It lands second: its holder rebases onto `behind` once that one merges.
    Second {
        /// The item that lands first.
        behind: ItemId,
        /// Why that one lands first.
        because: OrderRule,
    },
    /// The rule ties, and people decide.
    Tied,
    /// There is no order for it: a moved base, a collision whose order was not worked out yet, or
    /// an item the overlap does not name.
    Unknown,
}

/// One side of a collision, as the order rule reads it: the claim and its item's priority.
#[derive(Debug, Clone, Copy)]
pub struct Contender<'a> {
    /// The claim.
    pub claim: &'a Claim,
    /// The priority of the claim's item.
    pub priority: Priority,
}

/// Which of two colliding claims lands first. A claim in review lands before a claim at work;
/// between two in review or two at work, the item with the higher priority (the lower number)
/// lands first; with equal priority, the claim claimed earlier does. When all three are equal the
/// rule ties. The answer does not depend on which claim is `a` and which is `b`.
pub fn order(a: Contender<'_>, b: Contender<'_>) -> OverlapOrder {
    OrderRule::ALL
        .into_iter()
        .find_map(|rule| match rule.compare(&a, &b) {
            Ordering::Less => Some(OverlapOrder::First {
                item: a.claim.item,
                because: rule,
            }),
            Ordering::Greater => Some(OverlapOrder::First {
                item: b.claim.item,
                because: rule,
            }),
            Ordering::Equal => None,
        })
        .unwrap_or(OverlapOrder::Tied)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::review::{ClaimState, PrRef, Review};
    use crate::testing::{ALICE, BOB, at, claim};

    /// A claim on `item`, claimed at `minutes`, at work.
    fn working(item: u64, minutes: i64) -> Claim {
        claim(item, ALICE, &[], minutes)
    }

    /// A claim on `item`, claimed at `minutes`, in review since later.
    fn reviewed(item: u64, minutes: i64) -> Claim {
        let mut made = claim(item, BOB, &[], minutes);
        let prs: BTreeSet<PrRef> = [PrRef::parse("acme/api#12").unwrap()].into();
        made.state = ClaimState::InReview(Review::new(prs, at(minutes + 30), false).unwrap());
        made
    }

    fn priority(value: u8) -> Priority {
        Priority::new(value).unwrap()
    }

    fn side(claim: &Claim, value: u8) -> Contender<'_> {
        Contender {
            claim,
            priority: priority(value),
        }
    }

    /// The order of `a` and `b`, checked to be the same with the two swapped.
    fn symmetric(a: Contender<'_>, b: Contender<'_>) -> OverlapOrder {
        let forward = order(a, b);
        assert_eq!(
            forward,
            order(b, a),
            "the order does not depend on the sides"
        );
        forward
    }

    fn first(item: u64, because: OrderRule) -> OverlapOrder {
        OverlapOrder::First {
            item: ItemId(item),
            because,
        }
    }

    #[test]
    fn a_claim_in_review_lands_before_one_at_work_whatever_the_priorities_and_ages() {
        let in_review = reviewed(1, 50);
        let at_work = working(2, 0);
        assert_eq!(
            symmetric(side(&in_review, 4), side(&at_work, 0)),
            first(1, OrderRule::InReview)
        );
        assert_eq!(
            symmetric(side(&in_review, 2), side(&at_work, 2)),
            first(1, OrderRule::InReview)
        );
    }

    #[test]
    fn between_two_at_work_the_higher_priority_lands_first_even_against_an_older_claim() {
        let older = working(1, 0);
        let newer = working(2, 60);
        assert_eq!(
            symmetric(side(&older, 3), side(&newer, 1)),
            first(2, OrderRule::Priority)
        );
        assert_eq!(
            symmetric(side(&older, 0), side(&newer, 4)),
            first(1, OrderRule::Priority)
        );
    }

    #[test]
    fn between_two_in_review_the_priority_decides() {
        let older = reviewed(1, 0);
        let newer = reviewed(2, 60);
        assert_eq!(
            symmetric(side(&older, 2), side(&newer, 1)),
            first(2, OrderRule::Priority)
        );
    }

    #[test]
    fn with_equal_priority_the_older_claim_lands_first() {
        let older = working(7, 0);
        let newer = working(3, 1);
        assert_eq!(
            symmetric(side(&newer, 2), side(&older, 2)),
            first(7, OrderRule::Older)
        );
        let older_review = reviewed(7, 0);
        let newer_review = reviewed(3, 1);
        assert_eq!(
            symmetric(side(&newer_review, 2), side(&older_review, 2)),
            first(7, OrderRule::Older)
        );
    }

    #[test]
    fn the_rule_ties_when_state_priority_and_age_are_all_equal() {
        let one = working(1, 5);
        let two = working(2, 5);
        assert_eq!(symmetric(side(&one, 2), side(&two, 2)), OverlapOrder::Tied);
        let one_review = reviewed(1, 5);
        let two_review = reviewed(2, 5);
        assert_eq!(
            symmetric(side(&one_review, 0), side(&two_review, 0)),
            OverlapOrder::Tied
        );
    }

    #[test]
    fn an_order_has_one_stored_name_and_comes_back_from_it() {
        for rule in OrderRule::ALL {
            assert_eq!(OrderRule::parse(rule.code()), Ok(rule));
            let decided = first(9, rule);
            assert_eq!(decided.code(), rule.code());
            assert_eq!(
                OverlapOrder::from_parts(decided.code(), decided.first()),
                Ok(decided)
            );
            assert!(!rule.why().is_empty());
        }
        assert_eq!(OverlapOrder::Tied.code(), "tied");
        assert_eq!(
            OverlapOrder::from_parts("tied", None),
            Ok(OverlapOrder::Tied)
        );
        assert_eq!(OrderRule::parse("tied"), Err(OrderError::Unknown));
        assert_eq!(
            OverlapOrder::from_parts("sooner", Some(ItemId(1))),
            Err(OrderError::Unknown)
        );
        assert_eq!(
            OverlapOrder::from_parts("tied", Some(ItemId(1))),
            Err(OrderError::First)
        );
        assert_eq!(
            OverlapOrder::from_parts("older", None),
            Err(OrderError::First)
        );
        let codes: BTreeSet<&str> = OrderRule::ALL.iter().map(|rule| rule.code()).collect();
        assert_eq!(codes, ["in_review", "older", "priority"].into());
    }

    #[test]
    fn each_side_reads_whether_it_lands_first_second_or_tied() {
        let decided = first(1, OrderRule::Priority);
        assert_eq!(
            decided.landing(ItemId(1), ItemId(2)),
            Landing::First {
                because: OrderRule::Priority
            }
        );
        assert_eq!(
            decided.landing(ItemId(2), ItemId(1)),
            Landing::Second {
                behind: ItemId(1),
                because: OrderRule::Priority
            }
        );
        assert_eq!(decided.landing(ItemId(3), ItemId(4)), Landing::Unknown);
        assert_eq!(
            OverlapOrder::Tied.landing(ItemId(1), ItemId(2)),
            Landing::Tied
        );
    }
}
