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
        }
    }
}

fn held(member: MemberId, item: u64, text: &str) -> Verdict {
    Verdict::HeldBy {
        member,
        item: ItemId(item),
        area: area(text),
        reason: HeldReason::AskHolder,
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
            ..Case::default()
        },
        Case {
            name: "review: my own area stays mine while my claim is in review",
            claims: vec![reviewing(alice_billing(), &["acme/api#12"])],
            path: "api:packages/billing/src/Invoice.php",
            touch: vec![1],
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
    }
}

fn apply(plan: &ReservationPlan, claims: &mut [Claim], shared: &mut Vec<SharedPath>) {
    for add in &plan.add_areas {
        for claim in claims.iter_mut().filter(|claim| claim.item == add.claim) {
            claim.areas.insert(add.area.clone());
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
