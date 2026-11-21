use super::*;
use crate::hot_rule::HotRuleRegistry;
use crate::testing::{ALICE, BOB, area, at, claim, settings, two_repo_json};
use time::Duration;

fn path(text: &str) -> RepoPath {
    RepoPath::parse(text).unwrap()
}

fn note(text: &str) -> Option<Note> {
    Some(Note::parse(text).unwrap())
}

fn kind(name: &str) -> HotRuleKind {
    HotRuleRegistry::standard().kind(name).unwrap()
}

fn shared(item: u64, text: &str) -> SharedPath {
    SharedPath {
        claim: ItemId(item),
        path: path(text),
    }
}

fn add(item: u64, text: &str) -> AddArea {
    AddArea {
        claim: ItemId(item),
        area: area(text),
    }
}

/// One row of the verdict table: the world, one path, and everything the check must return.
struct Case {
    name: &'static str,
    policy: &'static str,
    claims: Vec<Claim>,
    grants: Vec<Grant>,
    shared: Vec<SharedPath>,
    path: &'static str,
    verdict: Verdict,
    add_areas: Vec<AddArea>,
    share_paths: Vec<SharedPath>,
    touch: Vec<u64>,
    by_area: Vec<u64>,
}

impl Default for Case {
    fn default() -> Self {
        Self {
            name: "",
            policy: "require-claim",
            claims: Vec::new(),
            grants: Vec::new(),
            shared: Vec::new(),
            path: "",
            verdict: Verdict::Allowed,
            add_areas: Vec::new(),
            share_paths: Vec::new(),
            touch: Vec::new(),
            by_area: Vec::new(),
        }
    }
}

fn held(member: MemberId, item: u64, text: &str) -> Verdict {
    Verdict::HeldBy {
        member,
        item: ItemId(item),
        area: area(text),
        reason: HeldReason::AskHolder { waiting: None },
    }
}

/// A claim in review of the pull requests named, since its last sign of life.
fn reviewing(claim: Claim, prs: &[&str]) -> Claim {
    use crate::review::{ClaimState, PrRef, Review};
    let prs = prs.iter().map(|text| PrRef::parse(text).unwrap()).collect();
    Claim {
        state: ClaimState::InReview(Review::new(prs, claim.last_seen, false).unwrap()),
        ..claim
    }
}

fn in_review_reason(prs: &[&str]) -> HeldReason {
    HeldReason::InReview {
        prs: prs
            .iter()
            .map(|text| crate::review::PrRef::parse(text).unwrap())
            .collect(),
    }
}

/// The claim as debt work.
fn debt(claim: Claim) -> Claim {
    Claim {
        kind: crate::handover::ClaimKind::Debt,
        ..claim
    }
}

/// The claim waiting for the area from the claim on `from`.
fn waiting(mut claim: Claim, text: &str, from: u64) -> Claim {
    claim.pending.insert(
        area(text),
        crate::handover::PendingArea::waiting_for(ItemId(from)),
    );
    claim
}

/// The claim's wait for `api:billing` asked at minute `asked`, escalated at `escalated`.
fn asked(mut claim: Claim, asked: i64, escalated: Option<i64>) -> Claim {
    if let Some(pending) = claim.pending.get_mut(&area("api:billing")) {
        pending.asked_at = Some(at(asked));
        pending.escalated_at = escalated.map(at);
    }
    claim
}

fn scope(named: &[&str]) -> crate::wide::WideScope {
    crate::wide::WideScope::new(
        crate::testing::areas(named),
        crate::wide::WideReason::parse("Rename the money type").unwrap(),
    )
    .unwrap()
}

fn run(case: &Case, now: OffsetDateTime) -> EditCheck {
    let settings = settings(&two_repo_json(case.policy));
    let input = EditCheckInput {
        me: ALICE,
        claims: &case.claims,
        grants: &case.grants,
        shared: &case.shared,
        settings: &settings,
        now,
    };
    edit_verdicts(&[path(case.path)], &input).unwrap()
}

#[test]
fn verdict_table() {
    let stale = -(60 * 25);
    let bob_billing = || claim(2, BOB, &["api:billing"], 0);
    let bob_search = || claim(2, BOB, &["api:search"], 0);
    let alice_billing = || claim(1, ALICE, &["api:billing"], 0);
    let alice_plain = || claim(1, ALICE, &[], 0);
    let alice_wide = || Claim {
        wide: Some(scope(&["api:billing", "api:search"])),
        ..claim(1, ALICE, &[], 0)
    };
    let bob_debt = || debt(bob_billing());
    let alice_waiting =
        |item: u64, from: u64| waiting(claim(item, ALICE, &[], 0), "api:billing", from);
    let lock_note = || Verdict::AllowedWithNote {
        rule: kind("regenerate"),
        note: note("regenerate it; never merge by hand"),
        touched_by: Vec::new(),
    };
    let cases = vec![
        Case {
            name: "ungoverned path",
            claims: vec![alice_plain()],
            path: "api:README.md",
            ..Case::default()
        },
        Case {
            name: "ungoverned path, no claim",
            path: "api:README.md",
            ..Case::default()
        },
        Case {
            name: "own area",
            claims: vec![alice_billing()],
            path: "api:packages/billing/src/Invoice.php",
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "own area, claim taken on another host",
            claims: vec![Claim {
                host: crate::session::HostName::parse("build-box").unwrap(),
                ..alice_billing()
            }],
            path: "api:packages/billing/src/Invoice.php",
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "other member's area",
            claims: vec![alice_plain(), bob_search()],
            path: "api:packages/search/src/Index.php",
            verdict: held(BOB, 2, "api:search"),
            ..Case::default()
        },
        Case {
            name: "other member's area, no claim of mine",
            claims: vec![bob_search()],
            path: "api:packages/search/src/Index.php",
            verdict: held(BOB, 2, "api:search"),
            ..Case::default()
        },
        Case {
            name: "other member's area under the advise policy",
            policy: "advise",
            claims: vec![bob_search()],
            path: "api:packages/search/src/Index.php",
            verdict: held(BOB, 2, "api:search"),
            ..Case::default()
        },
        Case {
            name: "other member's stale claim holds nothing",
            claims: vec![alice_plain(), claim(2, BOB, &["api:search"], stale)],
            path: "api:packages/search/src/Index.php",
            add_areas: vec![add(1, "api:search")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "granted",
            claims: vec![alice_plain(), bob_search()],
            grants: vec![Grant {
                item: ItemId(1),
                area: area("api:search"),
                from: BOB,
                to: ALICE,
            }],
            path: "api:packages/search/src/Index.php",
            verdict: Verdict::Granted {
                by: BOB,
                area: area("api:search"),
            },
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "a grant for an item I no longer hold is not in force",
            claims: vec![claim(3, ALICE, &[], 0), bob_search()],
            grants: vec![Grant {
                item: ItemId(1),
                area: area("api:search"),
                from: BOB,
                to: ALICE,
            }],
            path: "api:packages/search/src/Index.php",
            verdict: held(BOB, 2, "api:search"),
            ..Case::default()
        },
        Case {
            name: "a grant for another area does not apply",
            claims: vec![
                alice_plain(),
                claim(2, BOB, &["api:search", "api:billing"], 0),
            ],
            grants: vec![Grant {
                item: ItemId(1),
                area: area("api:billing"),
                from: BOB,
                to: ALICE,
            }],
            path: "api:packages/search/src/Index.php",
            verdict: held(BOB, 2, "api:search"),
            ..Case::default()
        },
        Case {
            name: "needs a claim under require-claim",
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::NeedsClaim,
            ..Case::default()
        },
        Case {
            name: "only a stale claim of mine needs a claim",
            claims: vec![claim(1, ALICE, &["api:billing"], stale)],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::NeedsClaim,
            ..Case::default()
        },
        Case {
            name: "no claim needed under advise",
            policy: "advise",
            path: "api:packages/billing/src/Invoice.php",
            ..Case::default()
        },
        Case {
            name: "a free area joins my claim",
            claims: vec![alice_plain()],
            path: "api:packages/billing/src/Invoice.php",
            add_areas: vec![add(1, "api:billing")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "regenerate rule, no claim needed",
            path: "api:composer.lock",
            verdict: lock_note(),
            ..Case::default()
        },
        Case {
            name: "a hot rule wins over the area containing the path",
            claims: vec![bob_billing()],
            path: "api:packages/billing/composer.lock",
            verdict: lock_note(),
            ..Case::default()
        },
        Case {
            name: "first matching hot rule decides: free before warn-shared",
            path: "api:config/routes.yaml",
            ..Case::default()
        },
        Case {
            name: "warn-shared, nobody else touched it",
            claims: vec![alice_plain()],
            path: "api:config/app.yaml",
            share_paths: vec![shared(1, "api:config/app.yaml")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "warn-shared, another member's live claim touched it",
            claims: vec![alice_plain(), bob_search()],
            shared: vec![shared(2, "api:config/app.yaml")],
            path: "api:config/app.yaml",
            verdict: Verdict::AllowedWithNote {
                rule: kind("warn-shared"),
                note: note("shared configuration"),
                touched_by: vec![Holder {
                    member: BOB,
                    item: ItemId(2),
                }],
            },
            share_paths: vec![shared(1, "api:config/app.yaml")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "warn-shared, a stale claim's record is not named",
            claims: vec![alice_plain(), claim(2, BOB, &[], stale)],
            shared: vec![shared(2, "api:config/app.yaml")],
            path: "api:config/app.yaml",
            share_paths: vec![shared(1, "api:config/app.yaml")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "warn-shared, another file's record is not named",
            claims: vec![alice_plain(), bob_search()],
            shared: vec![shared(2, "api:config/other.yaml")],
            path: "api:config/app.yaml",
            share_paths: vec![shared(1, "api:config/app.yaml")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "warn-shared, already recorded under my claim",
            claims: vec![alice_plain()],
            shared: vec![shared(1, "api:config/app.yaml")],
            path: "api:config/app.yaml",
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "warn-shared without a claim records nothing",
            path: "api:config/app.yaml",
            ..Case::default()
        },
        Case {
            name: "owner rule, the area's holder is another member",
            claims: vec![alice_plain(), bob_billing()],
            path: "api:deploy/billing.yaml",
            verdict: held(BOB, 2, "api:billing"),
            ..Case::default()
        },
        Case {
            name: "owner rule, I hold the area",
            claims: vec![alice_billing()],
            path: "api:deploy/billing.yaml",
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "owner rule, nobody holds the area",
            claims: vec![alice_plain()],
            path: "api:deploy/billing.yaml",
            add_areas: vec![add(1, "api:billing")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "owner rule, no claim of mine",
            path: "api:deploy/billing.yaml",
            verdict: Verdict::NeedsClaim,
            ..Case::default()
        },
        Case {
            name: "wide: an area my wide claim names and nobody holds",
            claims: vec![alice_wide()],
            path: "api:packages/billing/src/Invoice.php",
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "wide: held by a member who acknowledged the announcement",
            claims: vec![alice_wide(), bob_billing()],
            grants: vec![Grant {
                item: ItemId(1),
                area: area("api:billing"),
                from: BOB,
                to: ALICE,
            }],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::Granted {
                by: BOB,
                area: area("api:billing"),
            },
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "wide: held by a member who has not acknowledged yet",
            claims: vec![alice_wide(), bob_billing()],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: HeldReason::AwaitingAck,
            },
            ..Case::default()
        },
        Case {
            name: "wide: my stale wide claim waits for nobody's acknowledgement",
            claims: vec![
                Claim {
                    wide: Some(scope(&["api:billing"])),
                    ..claim(3, ALICE, &[], stale)
                },
                alice_plain(),
                bob_billing(),
            ],
            path: "api:packages/billing/src/Invoice.php",
            verdict: held(BOB, 2, "api:billing"),
            ..Case::default()
        },
        Case {
            name: "wide: an area outside the scope joins the wide claim as held",
            claims: vec![alice_wide()],
            path: "web:src/checkout/Cart.ts",
            add_areas: vec![add(1, "web:checkout")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "wide: inside the scope nothing is added to a newer ordinary claim",
            claims: vec![alice_wide(), claim(3, ALICE, &[], 5)],
            path: "api:packages/search/src/Index.php",
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "wide: another member's wide claim holds what it names for nobody",
            claims: vec![
                alice_plain(),
                Claim {
                    wide: Some(scope(&["api:search"])),
                    ..claim(2, BOB, &[], 0)
                },
            ],
            path: "api:packages/search/src/Index.php",
            add_areas: vec![add(1, "api:search")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "review: a holder in review still holds the area, free when it merges",
            claims: vec![
                alice_plain(),
                reviewing(bob_billing(), &["acme/api#12", "acme/web#3"]),
            ],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: in_review_reason(&["acme/api#12", "acme/web#3"]),
            },
            ..Case::default()
        },
        Case {
            name: "review: a holder in review is held long after a working claim would be stale",
            claims: vec![
                alice_plain(),
                reviewing(claim(2, BOB, &["api:billing"], stale), &["acme/api#12"]),
            ],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: in_review_reason(&["acme/api#12"]),
            },
            ..Case::default()
        },
        Case {
            name: "review: being in review comes before waiting for an acknowledgement",
            claims: vec![alice_wide(), reviewing(bob_billing(), &["acme/api#12"])],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: in_review_reason(&["acme/api#12"]),
            },
            ..Case::default()
        },
        Case {
            name: "review: a grant from a holder in review is in force",
            claims: vec![alice_plain(), reviewing(bob_billing(), &["acme/api#12"])],
            grants: vec![Grant {
                item: ItemId(1),
                area: area("api:billing"),
                from: BOB,
                to: ALICE,
            }],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::Granted {
                by: BOB,
                area: area("api:billing"),
            },
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "review: my own area stays mine while my claim is in review",
            claims: vec![reviewing(alice_billing(), &["acme/api#12"])],
            path: "api:packages/billing/src/Invoice.php",
            touch: vec![1],
            by_area: vec![1],
            ..Case::default()
        },
        Case {
            name: "review: a new area goes to my claim I am working on, not the newer one in review",
            claims: vec![
                claim(5, ALICE, &[], 0),
                reviewing(claim(9, ALICE, &[], 10), &["acme/api#12"]),
            ],
            path: "api:packages/billing/src/Invoice.php",
            add_areas: vec![add(5, "api:billing")],
            touch: vec![5],
            ..Case::default()
        },
        Case {
            name: "review: with every claim of mine in review, the latest of them gets the area",
            claims: vec![
                reviewing(claim(5, ALICE, &[], 0), &["acme/api#11"]),
                reviewing(claim(9, ALICE, &[], 10), &["acme/api#12"]),
            ],
            path: "api:packages/billing/src/Invoice.php",
            add_areas: vec![add(9, "api:billing")],
            touch: vec![9],
            ..Case::default()
        },
        Case {
            name: "review: a claim of mine in review is a live claim, so no claim is needed",
            claims: vec![reviewing(claim(1, ALICE, &[], stale), &["acme/api#12"])],
            path: "api:packages/search/src/Index.php",
            add_areas: vec![add(1, "api:search")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "the first area containing the path decides",
            claims: vec![alice_plain(), claim(2, BOB, &["api:billing-docs"], 0)],
            path: "api:packages/billing/docs/guide.md",
            add_areas: vec![add(1, "api:billing")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "two live claims: the most recently claimed one gets the area",
            claims: vec![claim(5, ALICE, &[], 0), claim(9, ALICE, &[], 10)],
            path: "api:packages/billing/src/Invoice.php",
            add_areas: vec![add(9, "api:billing")],
            touch: vec![9],
            ..Case::default()
        },
        Case {
            name: "two live claims claimed at once: the lowest item gets the area",
            claims: vec![claim(9, ALICE, &[], 0), claim(5, ALICE, &[], 0)],
            path: "api:packages/billing/src/Invoice.php",
            add_areas: vec![add(5, "api:billing")],
            touch: vec![5],
            ..Case::default()
        },
        Case {
            name: "two live claims: the one holding the area is touched",
            claims: vec![
                claim(5, ALICE, &["api:billing"], 0),
                claim(9, ALICE, &[], 10),
            ],
            path: "api:packages/billing/src/Invoice.php",
            touch: vec![5],
            by_area: vec![5],
            ..Case::default()
        },
        Case {
            name: "one area name in another repository is another area",
            claims: vec![alice_plain(), bob_billing()],
            path: "web:src/billing/Cart.ts",
            add_areas: vec![add(1, "web:billing")],
            touch: vec![1],
            ..Case::default()
        },
        Case {
            name: "another repository's hot rules do not apply",
            claims: vec![alice_plain()],
            path: "web:composer.lock",
            ..Case::default()
        },
        Case {
            name: "my feature claim waits for the debt holder to hand the area over",
            claims: vec![alice_waiting(1, 2), bob_debt()],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: HeldReason::AwaitingHandover { escalates_at: None },
            },
            ..Case::default()
        },
        Case {
            name: "a debt holder my feature claim did not ask: declaring the area asks them",
            claims: vec![
                claim(5, ALICE, &[], 0),
                claim(9, ALICE, &[], 10),
                bob_debt(),
            ],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: HeldReason::AskHandover { claim: ItemId(9) },
            },
            ..Case::default()
        },
        Case {
            name: "another member's claim already waits for the area: ask the holder, naming it",
            claims: vec![
                claim(9, ALICE, &[], 0),
                bob_debt(),
                waiting(claim(4, crate::testing::CAROL, &[], 0), "api:billing", 2),
            ],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: HeldReason::AskHolder {
                    waiting: Some(Holder {
                        member: crate::testing::CAROL,
                        item: ItemId(4),
                    }),
                },
            },
            ..Case::default()
        },
        Case {
            name: "an asked wait says when it goes to the arbiters",
            claims: vec![asked(alice_waiting(1, 2), 3, None), bob_debt()],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: HeldReason::AwaitingHandover {
                    escalates_at: Some(at(33)),
                },
            },
            ..Case::default()
        },
        Case {
            name: "an escalated wait has nothing left to wait for but the handover",
            claims: vec![asked(alice_waiting(1, 2), 3, Some(40)), bob_debt()],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: HeldReason::AwaitingHandover { escalates_at: None },
            },
            ..Case::default()
        },
        Case {
            name: "a debt holder and only my own debt claim: ask the holder",
            claims: vec![debt(claim(1, ALICE, &[], 0)), bob_debt()],
            path: "api:packages/billing/src/Invoice.php",
            verdict: held(BOB, 2, "api:billing"),
            ..Case::default()
        },
        Case {
            name: "a debt holder in review: free when its pull requests merge, not asked",
            claims: vec![alice_waiting(1, 2), reviewing(bob_debt(), &["acme/api#7"])],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: in_review_reason(&["acme/api#7"]),
            },
            ..Case::default()
        },
        Case {
            name: "my wide claim names the area a debt claim holds: wait for the ack",
            claims: vec![alice_wide(), bob_debt()],
            path: "api:packages/billing/src/Invoice.php",
            verdict: Verdict::HeldBy {
                member: BOB,
                item: ItemId(2),
                area: area("api:billing"),
                reason: HeldReason::AwaitingAck,
            },
            touch: Vec::new(),
            ..Case::default()
        },
        Case {
            name: "a stale debt holder: the area is not added to the claim waiting for it",
            claims: vec![
                alice_waiting(5, 2),
                claim(9, ALICE, &[], -10),
                debt(claim(2, BOB, &["api:billing"], stale)),
            ],
            path: "api:packages/billing/src/Invoice.php",
            add_areas: vec![add(9, "api:billing")],
            touch: vec![9],
            ..Case::default()
        },
        Case {
            name: "another member's claim waits for an area nobody holds: no reservation",
            claims: vec![
                alice_plain(),
                waiting(claim(3, BOB, &[], 0), "api:billing", 2),
                debt(claim(2, crate::testing::CAROL, &["api:billing"], stale)),
            ],
            path: "api:packages/billing/src/Invoice.php",
            touch: vec![1],
            ..Case::default()
        },
    ];
    for case in &cases {
        let check = run(case, at(1));
        let expected_touch: BTreeSet<ItemId> = case.touch.iter().copied().map(ItemId).collect();
        assert_eq!(check.verdicts.len(), 1, "{}", case.name);
        assert_eq!(check.verdicts[0].path, path(case.path), "{}", case.name);
        assert_eq!(check.verdicts[0].verdict, case.verdict, "{}", case.name);
        assert_eq!(
            check.plan.add_areas, case.add_areas,
            "{}: add areas",
            case.name
        );
        assert_eq!(
            check.plan.share_paths, case.share_paths,
            "{}: shared paths",
            case.name
        );
        assert_eq!(check.touch, expected_touch, "{}: touch", case.name);
        let expected_by_area: BTreeSet<ItemId> = case.by_area.iter().copied().map(ItemId).collect();
        assert_eq!(check.by_area, expected_by_area, "{}: by area", case.name);
    }
}

fn apply(plan: &ReservationPlan, claims: &mut [Claim], shared: &mut Vec<SharedPath>) {
    for add in &plan.add_areas {
        for claim in claims.iter_mut().filter(|claim| claim.item == add.claim) {
            claim.areas.gain(add.area.clone(), at(0));
        }
    }
    shared.extend(plan.share_paths.iter().cloned());
}

#[test]
fn a_repeated_check_plans_nothing() {
    let settings = settings(&two_repo_json("require-claim"));
    let mut claims = vec![claim(1, ALICE, &[], 0)];
    let mut shared_paths = Vec::new();
    let paths = [
        path("api:packages/billing/src/Invoice.php"),
        path("api:config/app.yaml"),
        path("web:src/checkout/Pay.ts"),
    ];
    let check = |claims: &[Claim], shared_paths: &[SharedPath]| {
        let input = EditCheckInput {
            me: ALICE,
            claims,
            grants: &[],
            shared: shared_paths,
            settings: &settings,
            now: at(1),
        };
        edit_verdicts(&paths, &input).unwrap()
    };
    let first = check(&claims, &shared_paths);
    assert_eq!(
        first.plan.add_areas,
        vec![add(1, "api:billing"), add(1, "web:checkout")]
    );
    assert_eq!(
        first.plan.share_paths,
        vec![shared(1, "api:config/app.yaml")]
    );
    apply(&first.plan, &mut claims, &mut shared_paths);
    let again = check(&claims, &shared_paths);
    assert!(again.plan.is_empty(), "{:?}", again.plan);
    assert_eq!(again.verdicts, first.verdicts);
    assert_eq!(
        again.touch, first.touch,
        "an edit still refreshes the claim"
    );
}

#[test]
fn several_paths_in_one_area_plan_it_once() {
    let settings = settings(&two_repo_json("require-claim"));
    let claims = [claim(1, ALICE, &[], 0)];
    let input = EditCheckInput {
        me: ALICE,
        claims: &claims,
        grants: &[],
        shared: &[],
        settings: &settings,
        now: at(1),
    };
    let paths = [
        path("api:packages/billing/a.php"),
        path("api:packages/billing/b.php"),
        path("api:README.md"),
    ];
    let check = edit_verdicts(&paths, &input).unwrap();
    assert_eq!(check.verdicts.len(), 3);
    assert_eq!(check.plan.add_areas, vec![add(1, "api:billing")]);
}

#[test]
fn a_path_of_an_unlisted_repository_refuses_the_check() {
    let settings = settings(&two_repo_json("require-claim"));
    let input = EditCheckInput {
        me: ALICE,
        claims: &[],
        grants: &[],
        shared: &[],
        settings: &settings,
        now: at(1),
    };
    let refused = edit_verdicts(&[path("api:README.md"), path("search:README.md")], &input);
    assert_eq!(
        refused,
        Err(EditCheckRefused::UnknownRepo {
            repo: RepoId::parse("search").unwrap()
        })
    );
}

#[test]
fn a_claim_seen_just_within_stale_after_still_counts() {
    let settings = settings(&two_repo_json("require-claim"));
    let claims = [claim(1, ALICE, &["api:billing"], 0)];
    let edge = at(0) + settings.stale_after();
    let input = EditCheckInput {
        me: ALICE,
        claims: &claims,
        grants: &[],
        shared: &[],
        settings: &settings,
        now: edge,
    };
    let check = edit_verdicts(&[path("api:packages/billing/a.php")], &input).unwrap();
    assert_eq!(check.verdicts[0].verdict, Verdict::Allowed);
    let late = EditCheckInput {
        now: edge + Duration::seconds(1),
        ..input
    };
    let check = edit_verdicts(&[path("api:packages/billing/a.php")], &late).unwrap();
    assert_eq!(check.verdicts[0].verdict, Verdict::NeedsClaim);
}

#[test]
fn a_claim_reached_by_its_area_is_told_from_one_reached_as_the_target() {
    let settings = settings(&two_repo_json("require-claim"));
    let claims = [
        claim(5, ALICE, &["api:billing"], 0),
        claim(9, ALICE, &[], 10),
    ];
    let input = EditCheckInput {
        me: ALICE,
        claims: &claims,
        grants: &[],
        shared: &[],
        settings: &settings,
        now: at(11),
    };
    let check = |paths: &[&str]| {
        let paths: Vec<RepoPath> = paths.iter().map(|text| path(text)).collect();
        edit_verdicts(&paths, &input).unwrap()
    };
    let both = check(&[
        "api:packages/billing/a.php",
        "api:packages/search/a.php",
        "api:config/app.yaml",
    ]);
    assert_eq!(both.touch, [ItemId(5), ItemId(9)].into());
    assert_eq!(both.by_area, [ItemId(5)].into());
    assert_eq!(both.attribution(ItemId(5)), Some(Attribution::Area));
    assert_eq!(
        both.attribution(ItemId(9)),
        Some(Attribution::Target),
        "an area nobody holds and a shared path go to the newest claim as a guess"
    );
    assert_eq!(both.attribution(ItemId(7)), None);

    let shared_only = check(&["api:config/app.yaml"]);
    assert_eq!(shared_only.touch, [ItemId(9)].into());
    assert!(
        shared_only.by_area.is_empty(),
        "a shared path names no area"
    );

    let holding = [claim(9, ALICE, &["api:billing"], 10)];
    let one = EditCheckInput {
        claims: &holding,
        ..input
    };
    let paths = [
        path("api:packages/billing/a.php"),
        path("api:config/app.yaml"),
    ];
    let check = edit_verdicts(&paths, &one).unwrap();
    assert_eq!(
        check.attribution(ItemId(9)),
        Some(Attribution::Area),
        "a claim reached by its area and as the target is reached by its area"
    );
}

#[test]
fn an_edit_in_an_area_a_claim_gained_as_the_target_is_still_the_targets() {
    let settings = settings(&two_repo_json("require-claim"));
    let mut claims = vec![
        claim(5, ALICE, &["api:billing"], 0),
        claim(9, ALICE, &[], 10),
    ];
    let search = [path("api:packages/search/a.php")];
    let check = |claims: &[Claim]| {
        let input = EditCheckInput {
            me: ALICE,
            claims,
            grants: &[],
            shared: &[],
            settings: &settings,
            now: at(11),
        };
        edit_verdicts(&search, &input).unwrap()
    };
    let first = check(&claims);
    assert_eq!(first.plan.add_areas, vec![add(9, "api:search")]);
    assert_eq!(first.attribution(ItemId(9)), Some(Attribution::Target));
    apply(&first.plan, &mut claims, &mut Vec::new());
    assert_eq!(
        claims[1].areas.origin(&area("api:search")),
        Some(AreaOrigin::Gained),
        "the fallback adds the area as gained"
    );

    let again = check(&claims);
    assert!(again.plan.is_empty(), "{:?}", again.plan);
    assert_eq!(again.touch, [ItemId(9)].into(), "the area holds like any");
    assert_eq!(
        again.attribution(ItemId(9)),
        Some(Attribution::Target),
        "an area the claim only gained tells no more of the work than the first edit did"
    );

    claims[1].areas.declare(area("api:search"), at(0));
    assert_eq!(
        check(&claims).attribution(ItemId(9)),
        Some(Attribution::Area),
        "once declared, the area tells the claim's work"
    );
}

#[test]
fn an_area_another_member_gained_still_holds_against_me() {
    let settings = settings(&two_repo_json("require-claim"));
    let mut bobs = claim(4, BOB, &[], 0);
    bobs.areas.gain(area("api:search"), at(0));
    let claims = [bobs, claim(5, ALICE, &[], 0)];
    let input = EditCheckInput {
        me: ALICE,
        claims: &claims,
        grants: &[],
        shared: &[],
        settings: &settings,
        now: at(1),
    };
    let check = edit_verdicts(&[path("api:packages/search/a.php")], &input).unwrap();
    assert_eq!(check.verdicts[0].verdict, held(BOB, 4, "api:search"));
}

#[test]
fn of_my_claims_holding_an_area_the_one_that_declared_it_takes_the_edit() {
    let settings = settings(&two_repo_json("require-claim"));
    let search = [path("api:packages/search/a.php")];
    let check = |claims: &[Claim], minute: i64| {
        let input = EditCheckInput {
            me: ALICE,
            claims,
            grants: &[],
            shared: &[],
            settings: &settings,
            now: at(minute),
        };
        edit_verdicts(&search, &input).unwrap()
    };
    let mut claims = vec![claim(9, ALICE, &[], 0)];
    let first = check(&claims, 1);
    assert_eq!(first.plan.add_areas, vec![add(9, "api:search")]);
    apply(&first.plan, &mut claims, &mut Vec::new());

    claims.push(claim(12, ALICE, &["api:search"], 10));
    claims[0].renewed_at = at(20);
    claims[0].last_seen = at(20);

    let edit = check(&claims, 21);
    assert_eq!(edit.verdicts[0].verdict, Verdict::Allowed);
    assert!(edit.plan.is_empty(), "{:?}", edit.plan);
    assert_eq!(
        edit.touch,
        [ItemId(12)].into(),
        "the claim that declared the area takes the edit, not the newer one that only gained it"
    );
    assert_eq!(edit.attribution(ItemId(12)), Some(Attribution::Area));
    assert_eq!(edit.attribution(ItemId(9)), None);
    let edited: Vec<String> = edit
        .edited_areas(ItemId(12))
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        edited,
        ["api:search"],
        "the edit's area is told with the claim it went to"
    );
    assert_eq!(edit.edited_areas(ItemId(9)).count(), 0);
    let mut seen = claims[1].clone();
    seen.saw_work(edit.edited_areas(ItemId(12)), at(21));
    assert_eq!(seen.last_seen, at(21));
    assert_eq!(seen.areas.clock(&area("api:search")), Some(at(21)));

    claims[1].areas = crate::claim::ClaimAreas::default();
    claims[1].areas.gain(area("api:search"), at(0));
    assert_eq!(
        check(&claims, 21).touch,
        [ItemId(9)].into(),
        "among claims that only gained the area, the newest takes the edit"
    );
}

#[test]
fn my_wide_claim_naming_an_area_takes_the_edit_over_a_claim_that_only_gained_it() {
    let settings = settings(&two_repo_json("require-claim"));
    let search = [path("api:packages/search/a.php")];
    let check = |claims: &[Claim], minute: i64| {
        let input = EditCheckInput {
            me: ALICE,
            claims,
            grants: &[],
            shared: &[],
            settings: &settings,
            now: at(minute),
        };
        edit_verdicts(&search, &input).unwrap()
    };
    let mut claims = vec![claim(9, ALICE, &[], 0)];
    let first = check(&claims, 1);
    assert_eq!(first.plan.add_areas, vec![add(9, "api:search")]);
    apply(&first.plan, &mut claims, &mut Vec::new());

    claims.push(Claim {
        wide: Some(scope(&["api:search"])),
        ..claim(12, ALICE, &[], 10)
    });
    claims[0].renewed_at = at(20);
    claims[0].last_seen = at(20);

    let edit = check(&claims, 21);
    assert_eq!(edit.verdicts[0].verdict, Verdict::Allowed);
    assert_eq!(
        edit.touch,
        [ItemId(12)].into(),
        "the wide claim that names the area takes the edit, not the one that only gained it"
    );
    assert_eq!(edit.attribution(ItemId(12)), Some(Attribution::Area));
}
