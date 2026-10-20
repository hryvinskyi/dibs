use std::collections::BTreeSet;

use super::*;
use crate::claim::{
    ClaimOutcome, ClaimRefused, ClaimRequest, TakeoverRefused, decide_claim, decide_narrow,
    decide_takeover,
};
use crate::review::{ClaimState, PrRef, Review};
use crate::testing::{ALICE, BOB, CAROL, area, areas, at, claim, request, two_repo_settings};

fn debt(claim: Claim) -> Claim {
    Claim {
        kind: ClaimKind::Debt,
        ..claim
    }
}

fn feature(item: u64, by: crate::ids::MemberId, declared: &[&str]) -> ClaimRequest {
    request(item, by, declared)
}

fn of_kind(
    item: u64,
    by: crate::ids::MemberId,
    declared: &[&str],
    kind: ClaimKind,
) -> ClaimRequest {
    ClaimRequest {
        kind: Some(kind),
        ..request(item, by, declared)
    }
}

fn reviewing(claim: Claim) -> Claim {
    let prs = BTreeSet::from([PrRef::parse("acme/api#7").unwrap()]);
    Claim {
        state: ClaimState::InReview(Review::new(prs, claim.last_seen, false).unwrap()),
        ..claim
    }
}

fn working(claim: Claim) -> Claim {
    Claim {
        state: ClaimState::Working,
        ..claim
    }
}

/// Alice's debt claim on item 2 holds `api:billing` and `api:search`.
fn alice_debt() -> Claim {
    debt(claim(2, ALICE, &["api:billing", "api:search"], 0))
}

/// Bob's feature claim on item 1 over `areas`, decided against `others` at minute 1.
fn bob_claims(others: &[Claim], declared: &[&str]) -> Result<ClaimOutcome, ClaimRefused> {
    decide_claim(
        None,
        others,
        &feature(1, BOB, declared),
        at(1),
        &two_repo_settings(),
    )
}

fn waiting_for(from: u64) -> PendingArea {
    PendingArea::waiting_for(ItemId(from))
}

fn settle(claims: &[Claim], minutes: i64) -> Settlement {
    settle_pending(claims, at(minutes), two_repo_settings().stale_after())
}

/// The claims after a settlement, the changed ones in place of the stored.
fn after(claims: &[Claim], settlement: &Settlement) -> Vec<Claim> {
    claims
        .iter()
        .map(|claim| {
            settlement
                .changed
                .iter()
                .find(|changed| changed.item == claim.item)
                .unwrap_or(claim)
                .clone()
        })
        .collect()
}

const ESCALATES: Duration = Duration::minutes(30);

fn pair(texts: &[&str], from: u64, to: u64) -> YieldPair {
    YieldPair {
        from: ItemId(from),
        to: ItemId(to),
        areas: areas(texts),
        request: None,
    }
}

fn passing(text: &str, from: u64, to: u64) -> AreaMove {
    AreaMove {
        area: area(text),
        from: ItemId(from),
        to: ItemId(to),
    }
}

#[test]
fn a_kind_has_a_stable_name_and_a_new_claim_is_a_feature_unless_it_says_debt() {
    for kind in [ClaimKind::Feature, ClaimKind::Debt] {
        assert_eq!(ClaimKind::parse(kind.code()), Ok(kind));
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            serde_json::json!(kind.code())
        );
    }
    assert_eq!(ClaimKind::parse("chore"), Err(ClaimKindError::Unknown));
    let settings = two_repo_settings();
    let plain = decide_claim(None, &[], &feature(1, BOB, &[]), at(0), &settings).unwrap();
    assert_eq!(plain.claim().kind, ClaimKind::Feature);
    let debt_work = decide_claim(
        None,
        &[],
        &of_kind(1, BOB, &[], ClaimKind::Debt),
        at(0),
        &settings,
    )
    .unwrap();
    assert_eq!(debt_work.claim().kind, ClaimKind::Debt);
}

#[test]
fn only_a_feature_waits_and_only_for_debt_work() {
    assert!(ClaimKind::Feature.takes_from(ClaimKind::Debt));
    assert!(!ClaimKind::Feature.takes_from(ClaimKind::Feature));
    assert!(!ClaimKind::Debt.takes_from(ClaimKind::Debt));
    assert!(!ClaimKind::Debt.takes_from(ClaimKind::Feature));
}

#[test]
fn a_feature_claim_over_a_debt_held_area_waits_for_it_per_area_and_holds_the_free_ones() {
    let outcome = bob_claims(&[alice_debt()], &["api:billing", "web:checkout"]).unwrap();
    let made = outcome.claim();
    assert_eq!(made.areas, areas(&["web:checkout"]));
    assert_eq!(
        made.pending,
        BTreeMap::from([(area("api:billing"), waiting_for(2))])
    );
    assert!(made.waits_for(&area("api:billing")));
    assert!(!made.holds(&area("api:billing")));
}

#[test]
fn the_claim_is_atomic_one_area_another_feature_holds_refuses_it_whole() {
    let others = [alice_debt(), claim(3, CAROL, &["web:checkout"], 0)];
    assert_eq!(
        bob_claims(&others, &["api:billing", "web:checkout"]),
        Err(ClaimRefused::AreaHeld {
            area: area("web:checkout"),
            member: CAROL,
            item: ItemId(3),
        })
    );
}

#[test]
fn an_area_waits_for_one_claim_at_a_time() {
    let carol_waits = Claim {
        pending: BTreeMap::from([(area("api:billing"), waiting_for(2))]),
        ..claim(3, CAROL, &[], 0)
    };
    let others = [alice_debt(), carol_waits.clone()];
    assert_eq!(
        bob_claims(&others, &["api:billing"]),
        Err(ClaimRefused::AreaWaited {
            area: area("api:billing"),
            member: CAROL,
            item: ItemId(3),
            own: false,
        })
    );
    let bob_waits = Claim {
        pending: BTreeMap::from([(area("api:billing"), waiting_for(2))]),
        ..claim(3, BOB, &[], 0)
    };
    let own = bob_claims(&[alice_debt(), bob_waits], &["api:billing"]);
    assert_eq!(
        own,
        Err(ClaimRefused::AreaWaited {
            area: area("api:billing"),
            member: BOB,
            item: ItemId(3),
            own: true,
        })
    );
    assert_eq!(
        own.unwrap_err().to_string(),
        "your claim #3 already waits for api:billing to be handed over"
    );
    let late = at(0) + two_repo_settings().stale_after() + Duration::minutes(5);
    let stale_waiter = Claim {
        last_seen: at(-60 * 30),
        ..carol_waits
    };
    assert!(
        decide_claim(
            None,
            &[alice_debt_seen(late), stale_waiter],
            &feature(1, BOB, &["api:billing"]),
            late,
            &two_repo_settings(),
        )
        .is_err(),
        "a stale claim still waits until its pending areas are settled"
    );
}

fn alice_debt_seen(seen: OffsetDateTime) -> Claim {
    Claim {
        last_seen: seen,
        ..alice_debt()
    }
}

#[test]
fn no_preemption_between_features_and_debt_never_takes() {
    let feature_held = [claim(2, ALICE, &["api:billing"], 0)];
    assert!(matches!(
        bob_claims(&feature_held, &["api:billing"]),
        Err(ClaimRefused::AreaHeld { .. })
    ));
    let settings = two_repo_settings();
    for holder in [claim(2, ALICE, &["api:billing"], 0), alice_debt()] {
        assert!(
            matches!(
                decide_claim(
                    None,
                    &[holder],
                    &of_kind(1, BOB, &["api:billing"], ClaimKind::Debt),
                    at(1),
                    &settings,
                ),
                Err(ClaimRefused::AreaHeld { .. })
            ),
            "a debt claim never takes an area"
        );
    }
}

#[test]
fn a_wide_claims_named_areas_are_taken_at_once_and_its_held_areas_yield() {
    let scope = crate::wide::WideScope::new(
        areas(&["api:search"]),
        crate::wide::WideReason::parse("Upgrade the HTTP client").unwrap(),
    )
    .unwrap();
    let wide_debt = Claim {
        wide: Some(scope),
        ..debt(claim(2, ALICE, &["api:billing"], 0))
    };
    let outcome = bob_claims(&[wide_debt], &["api:billing", "api:search"]).unwrap();
    assert_eq!(outcome.claim().areas, areas(&["api:search"]));
    assert_eq!(
        outcome
            .claim()
            .pending
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        areas(&["api:billing"])
    );
}

#[test]
fn a_renewal_keeps_the_kind_and_a_wait_already_asked_for() {
    let settings = two_repo_settings();
    let first = bob_claims(&[alice_debt()], &["api:billing"]).unwrap();
    let mut asked = first.claim().clone();
    asked
        .pending
        .get_mut(&area("api:billing"))
        .unwrap()
        .asked_at = Some(at(1));
    assert_eq!(
        decide_claim(
            Some(&asked),
            &[alice_debt()],
            &of_kind(1, BOB, &["api:billing"], ClaimKind::Debt),
            at(5),
            &settings,
        ),
        Err(ClaimRefused::KindFixed {
            kind: ClaimKind::Feature
        }),
        "a renewal naming another kind is refused"
    );
    let again = decide_claim(
        Some(&asked),
        &[alice_debt()],
        &of_kind(1, BOB, &["api:billing", "web:checkout"], ClaimKind::Feature),
        at(5),
        &settings,
    )
    .unwrap();
    let renewed = again.claim();
    assert_eq!(renewed.kind, ClaimKind::Feature, "a claim keeps its kind");
    assert_eq!(renewed.pending[&area("api:billing")].asked_at, Some(at(1)));
    assert_eq!(renewed.areas, areas(&["web:checkout"]));
    let freed = decide_claim(
        Some(renewed),
        &[],
        &feature(1, BOB, &["api:billing"]),
        at(6),
        &settings,
    )
    .unwrap();
    assert!(freed.claim().pending.is_empty());
    assert!(freed.claim().holds(&area("api:billing")));
}

#[test]
fn the_debt_holder_renewing_what_another_claim_waits_for_keeps_it() {
    let settings = two_repo_settings();
    let bob_waits = Claim {
        pending: BTreeMap::from([(area("api:billing"), waiting_for(2))]),
        ..claim(1, BOB, &[], 0)
    };
    let renewed = decide_claim(
        Some(&alice_debt()),
        std::slice::from_ref(&bob_waits),
        &request(2, ALICE, &["api:billing"]),
        at(1),
        &settings,
    )
    .unwrap();
    assert!(renewed.claim().holds(&area("api:billing")));
    let late = at(0) + settings.stale_after() + Duration::minutes(1);
    let bob_live = Claim {
        last_seen: late,
        ..bob_waits
    };
    let back = decide_claim(
        Some(&alice_debt()),
        &[bob_live],
        &request(2, ALICE, &[]),
        late,
        &settings,
    )
    .unwrap();
    let ClaimOutcome::Renewed {
        claim,
        dropped_areas,
    } = back
    else {
        panic!("expected a renewal");
    };
    assert_eq!(
        claim.areas,
        areas(&["api:search"]),
        "back from stale, the waited-for area is the waiting claim's"
    );
    assert_eq!(dropped_areas, areas(&["api:billing"]));
}

#[test]
fn a_wide_scope_never_names_an_area_the_claim_waits_for() {
    let settings = two_repo_settings();
    let refused = decide_claim(
        None,
        &[alice_debt()],
        &ClaimRequest {
            wide: Some(
                crate::wide::WideScope::new(
                    areas(&["api:billing"]),
                    crate::wide::WideReason::parse("Rename the money type").unwrap(),
                )
                .unwrap(),
            ),
            ..feature(1, BOB, &["api:billing"])
        },
        at(1),
        &settings,
    );
    assert_eq!(
        refused,
        Err(ClaimRefused::WideAndHeld {
            area: area("api:billing")
        })
    );
}

#[test]
fn a_debt_claim_working_on_the_area_is_asked_once() {
    let bob = bob_claims(&[alice_debt()], &["api:billing"])
        .unwrap()
        .claim()
        .clone();
    let claims = [bob, alice_debt()];
    let settled = settle(&claims, 2);
    assert_eq!(settled.asked, [pair(&["api:billing"], 2, 1)]);
    assert!(settled.handed_over.is_empty());
    let now = after(&claims, &settled);
    assert_eq!(now[0].pending[&area("api:billing")].asked_at, Some(at(2)));
    assert!(settle(&now, 3).is_empty(), "asked once");
}

#[test]
fn a_debt_claim_in_review_is_not_asked_until_it_is_worked_on_again() {
    let bob = bob_claims(&[alice_debt()], &["api:billing"])
        .unwrap()
        .claim()
        .clone();
    let claims = [bob.clone(), reviewing(alice_debt())];
    let settled = settle(&claims, 2);
    assert!(settled.is_empty(), "{settled:?}");
    let back = [bob, working(reviewing(alice_debt()))];
    assert_eq!(settle(&back, 3).asked, [pair(&["api:billing"], 2, 1)]);
}

#[test]
fn going_into_review_after_the_ask_resets_it_so_it_is_asked_again_later() {
    let mut bob = bob_claims(&[alice_debt()], &["api:billing"])
        .unwrap()
        .claim()
        .clone();
    let entry = bob.pending.get_mut(&area("api:billing")).unwrap();
    entry.asked_at = Some(at(1));
    entry.escalated_at = Some(at(40));
    let claims = [bob, reviewing(alice_debt())];
    let settled = settle(&claims, 50);
    assert!(settled.asked.is_empty() && settled.handed_over.is_empty());
    let reset = &settled.changed[0].pending[&area("api:billing")];
    assert_eq!((reset.asked_at, reset.escalated_at), (None, None));
}

#[test]
fn each_way_the_debt_claim_lets_go_hands_the_area_over() {
    let settings = two_repo_settings();
    let bob = bob_claims(&[alice_debt()], &["api:billing", "api:search"])
        .unwrap()
        .claim()
        .clone();

    let narrowed =
        decide_narrow(Some(&alice_debt()), ALICE, &areas(&["api:billing"]), at(2)).unwrap();
    assert_eq!(narrowed.areas, areas(&["api:billing"]));
    let claims = [bob.clone(), narrowed.claim];
    let settled = settle(&claims, 2);
    assert_eq!(settled.handed_over, [passing("api:billing", 2, 1)]);
    let now = after(&claims, &settled);
    assert!(now[0].holds(&area("api:billing")));
    assert!(!now[0].waits_for(&area("api:billing")));
    assert!(now[0].waits_for(&area("api:search")), "per area");

    let ended = [bob.clone()];
    let settled = settle(&ended, 2);
    assert_eq!(
        settled.handed_over,
        [passing("api:billing", 2, 1), passing("api:search", 2, 1)],
        "completed, released or removed: the claim is gone"
    );
    assert_eq!(
        settled.changed[0].areas,
        areas(&["api:billing", "api:search"])
    );

    let late = at(0) + settings.stale_after() + Duration::minutes(1);
    let bob_live = Claim {
        last_seen: late,
        ..bob.clone()
    };
    let stale = [bob_live.clone(), alice_debt()];
    let settled = settle_pending(&stale, late, settings.stale_after());
    assert_eq!(settled.handed_over.len(), 2, "gone stale");
    let now = after(&stale, &settled);
    assert!(
        now[1].areas.is_empty(),
        "the debt claim drops what it handed over"
    );

    let taken = decide_takeover(
        Some(&alice_debt()),
        std::slice::from_ref(&bob_live),
        &request(2, CAROL, &[]),
        late,
        &settings,
    )
    .unwrap();
    assert!(taken.claim.areas.is_empty());
    assert_eq!(
        taken.claim.kind,
        ClaimKind::Debt,
        "a takeover carries the kind on"
    );
    assert_eq!(taken.dropped_areas, areas(&["api:billing", "api:search"]));
    let claims = [bob_live, taken.claim];
    assert_eq!(
        settle_pending(&claims, late, settings.stale_after())
            .handed_over
            .len(),
        2,
        "taken over"
    );
}

#[test]
fn a_takeover_may_not_declare_an_area_another_claim_waits_for() {
    let settings = two_repo_settings();
    let late = at(0) + settings.stale_after() + Duration::minutes(1);
    let bob_waits = Claim {
        pending: BTreeMap::from([(area("api:billing"), waiting_for(2))]),
        ..claim(1, BOB, &[], 24 * 60 + 30)
    };
    assert_eq!(
        decide_takeover(
            Some(&alice_debt()),
            &[bob_waits],
            &request(2, CAROL, &["api:billing"]),
            late,
            &settings,
        ),
        Err(TakeoverRefused::AreaWaited {
            area: area("api:billing"),
            member: BOB,
            item: ItemId(1),
            own: false,
        })
    );
}

#[test]
fn narrowing_gives_up_what_the_claim_holds_or_waits_for_and_nothing_else() {
    let bob = bob_claims(&[alice_debt()], &["api:billing", "web:checkout"])
        .unwrap()
        .claim()
        .clone();
    let narrowed = decide_narrow(
        Some(&bob),
        BOB,
        &areas(&["api:billing", "api:search"]),
        at(9),
    )
    .unwrap();
    assert_eq!(narrowed.areas, areas(&["api:billing"]));
    assert!(narrowed.claim.pending.is_empty());
    assert_eq!(narrowed.claim.areas, areas(&["web:checkout"]));
    assert_eq!(narrowed.claim.last_seen, at(9));
    assert!(decide_narrow(Some(&bob), ALICE, &areas(&["web:checkout"]), at(9)).is_err());
    assert!(decide_narrow(None, BOB, &areas(&["web:checkout"]), at(9)).is_err());
}

#[test]
fn a_stale_feature_claim_stops_waiting() {
    let settings = two_repo_settings();
    let bob = bob_claims(&[alice_debt()], &["api:billing"])
        .unwrap()
        .claim()
        .clone();
    let late = at(1) + settings.stale_after() + Duration::minutes(1);
    let alice_live = alice_debt_seen(late);
    let claims = [bob, alice_live];
    let settled = settle_pending(&claims, late, settings.stale_after());
    assert!(settled.handed_over.is_empty() && settled.asked.is_empty());
    assert!(settled.changed[0].pending.is_empty());
}

#[test]
fn an_unanswered_request_escalates_at_thirty_minutes_and_not_before_and_once() {
    let settings = two_repo_settings();
    let stale_after = settings.stale_after();
    let bob = bob_claims(&[alice_debt()], &["api:billing"])
        .unwrap()
        .claim()
        .clone();
    let start = [bob.clone(), alice_debt()];
    let claims = after(&start, &settle(&start, 2));
    let asked_at = at(2);
    assert_eq!(
        claims[0].pending[&area("api:billing")].asked_at,
        Some(asked_at)
    );
    let early = claims;
    let (_, none) = escalations_due(
        &early,
        asked_at + Duration::minutes(29),
        stale_after,
        ESCALATES,
    );
    assert!(none.is_empty(), "not before thirty minutes");
    let due_at = asked_at + ESCALATES;
    let (changed, due) = escalations_due(&early, due_at, stale_after, ESCALATES);
    assert_eq!(due, [pair(&["api:billing"], 2, 1)]);
    assert_eq!(
        changed[0].pending[&area("api:billing")].escalated_at,
        Some(due_at)
    );
    let marked = after(
        &early,
        &Settlement {
            changed,
            ..Settlement::default()
        },
    );
    assert!(
        escalations_due(&marked, due_at + Duration::hours(1), stale_after, ESCALATES)
            .1
            .is_empty(),
        "once"
    );
    let in_review = [early[0].clone(), reviewing(early[1].clone())];
    assert!(
        escalations_due(&in_review, due_at, stale_after, ESCALATES)
            .1
            .is_empty(),
        "not while the debt claim is in review"
    );
    let not_asked = [bob, alice_debt()];
    assert!(
        escalations_due(&not_asked, due_at, stale_after, ESCALATES)
            .1
            .is_empty()
    );
}

#[test]
fn a_feature_never_takes_from_a_member_who_also_holds_the_area_under_a_feature_claim() {
    let alice_feature = claim(5, ALICE, &["api:billing"], 0);
    let others = [alice_debt(), alice_feature];
    assert_eq!(
        bob_claims(&others, &["api:billing"]),
        Err(ClaimRefused::AreaHeld {
            area: area("api:billing"),
            member: ALICE,
            item: ItemId(5),
        })
    );
}

#[test]
fn an_area_moves_only_once_no_other_member_holds_it() {
    let late = at(0) + two_repo_settings().stale_after() + Duration::minutes(1);
    let bob = Claim {
        pending: BTreeMap::from([(area("api:billing"), waiting_for(2))]),
        last_seen: late,
        ..claim(1, BOB, &[], 0)
    };
    let carol = Claim {
        last_seen: late,
        ..claim(3, CAROL, &["api:billing"], 0)
    };
    let claims = [bob, alice_debt(), carol];
    let settled = settle_pending(&claims, late, two_repo_settings().stale_after());
    assert!(settled.handed_over.is_empty(), "{settled:?}");
    assert_eq!(
        settled.withdrawn,
        [Withdrawal {
            moved: passing("api:billing", 2, 1),
            reason: WithdrawReason::HolderChanged,
            request: None,
        }],
        "a feature holder in between ends the wait"
    );
    let now = after(&claims, &settled);
    assert!(!now[0].holds(&area("api:billing")) && now[0].pending.is_empty());
    assert!(now[2].holds(&area("api:billing")));
}

#[test]
fn when_one_debt_claim_lets_go_the_wait_turns_to_the_next_and_asks_it_afresh() {
    let second_debt = debt(claim(6, ALICE, &["api:billing"], 0));
    let mut bob = bob_claims(&[alice_debt(), second_debt.clone()], &["api:billing"])
        .unwrap()
        .claim()
        .clone();
    let entry = bob.pending.get_mut(&area("api:billing")).unwrap();
    assert_eq!(entry.from, ItemId(2), "the lowest of the debt claims");
    entry.asked_at = Some(at(1));
    entry.escalated_at = Some(at(40));
    let narrowed =
        decide_narrow(Some(&alice_debt()), ALICE, &areas(&["api:billing"]), at(41)).unwrap();
    let claims = [bob, narrowed.claim, second_debt];
    let settled = settle(&claims, 41);
    assert!(
        settled.handed_over.is_empty(),
        "alice still holds it under #6"
    );
    assert_eq!(settled.asked, [pair(&["api:billing"], 6, 1)]);
    let now = after(&claims, &settled);
    let entry = &now[0].pending[&area("api:billing")];
    assert_eq!(
        (entry.from, entry.asked_at, entry.escalated_at),
        (ItemId(6), Some(at(41)), None)
    );
    let narrowed = decide_narrow(Some(&now[2]), ALICE, &areas(&["api:billing"]), at(42)).unwrap();
    let claims = [now[0].clone(), now[1].clone(), narrowed.claim];
    let settled = settle(&claims, 42);
    assert_eq!(settled.handed_over, [passing("api:billing", 6, 1)]);
}

#[test]
fn the_areas_of_one_pair_are_asked_in_one_request_that_later_areas_reuse() {
    let bob = bob_claims(&[alice_debt()], &["api:billing", "api:search"])
        .unwrap()
        .claim()
        .clone();
    let claims = [bob, alice_debt()];
    let settled = settle(&claims, 2);
    assert_eq!(settled.asked, [pair(&["api:billing", "api:search"], 2, 1)]);
    let mut asked = after(&claims, &settled);
    asked[0].note_request(ItemId(2), crate::ids::ThreadId(90));
    assert_eq!(
        asked[0].request_from(ItemId(2)),
        Some(crate::ids::ThreadId(90))
    );
    let narrowed = decide_narrow(Some(&asked[0]), BOB, &areas(&["api:search"]), at(3)).unwrap();
    assert_eq!(
        narrowed.withdrawn[&area("api:search")].request,
        Some(crate::ids::ThreadId(90)),
        "a wait given up keeps the conversation it was asked in, to say so there"
    );
    let reclaimed = decide_claim(
        Some(&narrowed.claim),
        &[alice_debt()],
        &feature(1, BOB, &["api:search"]),
        at(4),
        &two_repo_settings(),
    )
    .unwrap()
    .claim()
    .clone();
    let claims = [reclaimed, alice_debt()];
    let settled = settle(&claims, 4);
    assert_eq!(
        settled.asked,
        [YieldPair {
            request: Some(crate::ids::ThreadId(90)),
            ..pair(&["api:search"], 2, 1)
        }],
        "the request still open for the pair is reused"
    );
}

#[test]
fn a_stale_waiter_takes_nothing_from_a_debt_holder_who_comes_back() {
    let settings = two_repo_settings();
    let late = at(0) + settings.stale_after() + Duration::minutes(1);
    let bob_waits = Claim {
        pending: BTreeMap::from([(area("api:billing"), waiting_for(2))]),
        ..claim(1, BOB, &[], 0)
    };
    let renewed = decide_claim(
        Some(&alice_debt()),
        &[bob_waits.clone(), alice_debt()],
        &request(2, ALICE, &[]),
        late,
        &settings,
    )
    .unwrap();
    let alice = renewed.claim().clone();
    assert!(alice.holds(&area("api:billing")), "alice keeps billing");
    let claims = [bob_waits, alice];
    let settled = settle_pending(&claims, late, settings.stale_after());
    assert_eq!(
        settled.withdrawn,
        [Withdrawal {
            moved: passing("api:billing", 2, 1),
            reason: WithdrawReason::Stale,
            request: None,
        }]
    );
    let now = after(&claims, &settled);
    assert!(now[0].pending.is_empty() && !now[0].holds(&area("api:billing")));
    assert!(now[1].holds(&area("api:billing")));
}
