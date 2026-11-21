//! The radar over the in-memory store: branches recorded from a member's checkout, footprints an
//! integration reports, the overlaps they make announced once to both holders, the moved bases a
//! merge brings, and everything a claim had cleared when it ends.

use std::collections::BTreeSet;

use dibs_domain::event::EventKind;
use dibs_domain::ids::ItemId;
use dibs_domain::message::Importance;
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::radar::{
    BranchName, BranchSource, CommitSha, FootprintBase, NotStored, OverlapKind,
};
use dibs_domain::repo::RepoId;
use dibs_domain::review::PrRef;

use super::*;
use crate::use_cases::testing::{Env, Person, START, checkout, document, in_checkout, session};
use crate::use_cases::{
    ApplySettings, ClaimItem, ClaimItemInput, EditCheck, EditCheckInput, Heartbeat, ListThreads,
    PrReport, ReadThread, Release, ReportPr, ReportedEnd, StartReview, StartReviewInput, Status,
    TakeOver, TakeOverInput,
};

/// One repository with `billing` and `search`, where search depends on billing and every
/// package's `src/Api/` is a published contract.
const SETTINGS: &str = r#"{
    "areas": [
        {"name": "billing", "paths": ["packages/billing/**"]},
        {"name": "search", "paths": ["packages/search/**"]}
    ],
    "contracts": [{"paths": ["packages/*/src/Api/**"]}],
    "area_deps": {"search": ["billing"]}
}"#;

/// The same areas, with `composer.lock` declared `regenerate`: made again on the new base, never
/// merged by hand.
const REGENERATED: &str = r#"{
    "areas": [
        {"name": "billing", "paths": ["src/billing/**"]},
        {"name": "search", "paths": ["src/search/**"]}
    ],
    "hot": [{"paths": ["composer.lock"], "kind": "regenerate"}]
}"#;

/// The same areas with no hot-file rule, so `composer.lock` is merged by hand.
const HAND_MERGED: &str = r#"{
    "areas": [
        {"name": "billing", "paths": ["src/billing/**"]},
        {"name": "search", "paths": ["src/search/**"]}
    ]
}"#;

fn repo() -> RepoId {
    RepoId::implicit()
}

/// The checkout `a` on `branch`: each person's own, since checkouts are told apart only within
/// one member.
fn on(branch: &str) -> FromCheckout {
    in_checkout('a', Some(branch))
}

fn path(text: &str) -> RepoPath {
    RepoPath::parse(&format!("default:{text}")).unwrap()
}

async fn claim(env: &Env, who: &Person, item: ItemId) {
    ClaimItem::new(&env.deps)
        .execute(
            &who.actor,
            ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item,
                session: who.session,
                areas: Vec::new(),
                kind: None,
            },
        )
        .await
        .unwrap();
}

/// Claims `item` for `who` and has the heartbeat of their checkout, where their session works on
/// that one claim alone, say its work is on `branch`.
async fn claim_on(env: &Env, who: &Person, item: ItemId, branch: &str) {
    claim(env, who, item).await;
    Heartbeat::new(&env.deps)
        .execute(&who.actor, who.session, Some(&on(branch)))
        .await
        .unwrap();
}

/// Records `branch` on `item`'s claim as one the claim named, through the radar as every writer
/// records a branch.
async fn named(env: &Env, item: ItemId, branch: &str) {
    let deps = &env.deps;
    let mut work = Work::begin_unattended(&*deps.store, env.project)
        .await
        .unwrap();
    let settings = work.project_settings(&deps.hot_rules).await.unwrap();
    let claim = work.claim(env.project, item).await.unwrap().unwrap();
    let radar = Radar {
        ids: &*deps.ids,
        schemes: &deps.ref_schemes,
        settings: &settings,
        now: env.now(),
    };
    let branch = BranchName::parse(branch).unwrap();
    radar
        .record_branch(&mut work, &claim, &repo(), &branch, BranchSource::Named)
        .await
        .unwrap();
    work.commit(&*deps.notifier).await.unwrap();
}

/// A footprint of `item`'s `branch` against `main`, from the merge base `base` committed when
/// the tests start.
fn footprint(item: ItemId, branch: &str, base: &str, paths: &[&str]) -> FootprintInput {
    footprint_from(item, branch, base, START, paths)
}

/// A footprint of `item`'s `branch` against `main`, from the merge base `base` committed `at`.
fn footprint_from(
    item: ItemId,
    branch: &str,
    base: &str,
    at: time::OffsetDateTime,
    paths: &[&str],
) -> FootprintInput {
    FootprintInput {
        item,
        repo: "default".to_owned(),
        branch: BranchName::parse(branch).unwrap(),
        base: FootprintBase {
            branch: BranchName::parse("main").unwrap(),
            sha: CommitSha::parse(base).unwrap(),
            at,
        },
        paths: paths
            .iter()
            .map(|text| RelPath::parse(text).unwrap())
            .collect(),
        truncated: false,
    }
}

async fn report(env: &Env, footprint: FootprintInput) -> FootprintRecorded {
    RecordFootprint::new(&env.deps)
        .execute(&env.importer, footprint)
        .await
        .unwrap()
}

async fn overlaps(env: &Env) -> Vec<Overlap> {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.overlaps(env.project).await.unwrap()
}

async fn stored_footprints(env: &Env) -> usize {
    let mut tx = env.deps.store.begin().await.unwrap();
    tx.footprints(env.project).await.unwrap().len()
}

/// The subjects and importance of the threads `who` can read that dibs started.
async fn told(env: &Env, who: &Person) -> Vec<(String, Importance)> {
    ListThreads::new(&env.deps)
        .execute(&who.actor, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|summary| {
            summary.thread.record.subject.as_str().contains("Overlap")
                || summary
                    .thread
                    .record
                    .subject
                    .as_str()
                    .starts_with("Base moved")
        })
        .map(|summary| {
            (
                summary.thread.record.subject.to_string(),
                summary.importance,
            )
        })
        .collect()
}

/// The subject and the first message's text of each thread about an overlap that `who` can read.
async fn told_texts(env: &Env, who: &Person) -> Vec<(String, String)> {
    let threads = ListThreads::new(&env.deps)
        .execute(&who.actor, None)
        .await
        .unwrap();
    let mut texts = Vec::new();
    for summary in threads {
        let subject = summary.thread.record.subject.to_string();
        if !(subject.contains("Overlap") || subject.starts_with("Base moved")) {
            continue;
        }
        let thread = ReadThread::new(&env.deps)
            .execute(&who.actor, summary.thread.record.id)
            .await
            .unwrap();
        texts.push((subject, thread.messages[0].record.body.to_string()));
    }
    texts
}

fn found(kinds: &[EventKind]) -> usize {
    kinds
        .iter()
        .filter(|kind| matches!(kind, EventKind::OverlapFound(_)))
        .count()
}

#[tokio::test]
async fn an_edit_records_the_branch_its_claim_works_on_once_per_change() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#1").await;
    claim(&env, &env.bob, item).await;
    let edit = |branch: Option<&str>| EditCheckInput {
        session: env.bob.session,
        paths: vec![path("packages/billing/a.php")],
        reserve: true,
        checkout: Some(in_checkout('a', branch)),
        worktree: None,
    };
    let check = EditCheck::new(&env.deps);
    let before = env.events().await.len();
    check
        .execute(&env.bob.actor, edit(Some("bob/1-x")))
        .await
        .unwrap();
    let recorded = |kinds: &[EventKind]| {
        kinds
            .iter()
            .filter(|kind| matches!(kind, EventKind::BranchRecorded(_)))
            .count()
    };
    assert_eq!(recorded(&env.kinds_after(before).await), 1);
    let again = env.events().await.len();
    check
        .execute(&env.bob.actor, edit(Some("bob/1-x")))
        .await
        .unwrap();
    check.execute(&env.bob.actor, edit(None)).await.unwrap();
    assert_eq!(
        recorded(&env.kinds_after(again).await),
        0,
        "the same branch, and a detached head, change nothing"
    );
    check
        .execute(&env.bob.actor, edit(Some("bob/1-y")))
        .await
        .unwrap();
    assert_eq!(recorded(&env.kinds_after(again).await), 1);
    let mut tx = env.deps.store.begin().await.unwrap();
    let stored = tx.claim(env.project, item).await.unwrap().unwrap();
    assert_eq!(
        stored.branch(&repo()).map(BranchName::as_str),
        Some("bob/1-y")
    );
}

#[tokio::test]
async fn a_footprint_is_stored_for_the_live_claims_branch_only_and_a_repeat_changes_nothing() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#1").await;
    assert_eq!(
        report(&env, footprint(item, "bob/1-x", "abcd", &["a.rs"])).await,
        FootprintRecorded::NotStored(NotStored::NoLiveClaim)
    );
    claim(&env, &env.bob, item).await;
    assert_eq!(
        report(&env, footprint(item, "bob/1-x", "abcd", &["a.rs"])).await,
        FootprintRecorded::NotStored(NotStored::OtherBranch),
        "no branch recorded yet"
    );
    Heartbeat::new(&env.deps)
        .execute(&env.bob.actor, env.bob.session, Some(&on("bob/1-x")))
        .await
        .unwrap();
    let before = env.events().await.len();
    assert_eq!(
        report(&env, footprint(item, "bob/1-x", "abcd", &["a.rs"])).await,
        FootprintRecorded::Stored { changed: true }
    );
    let kinds = env.kinds_after(before).await;
    assert!(
        matches!(kinds.as_slice(), [EventKind::FootprintChanged(changed)] if changed.paths == 1),
        "{kinds:?}"
    );
    let after = env.events().await.len();
    assert_eq!(
        report(&env, footprint(item, "bob/1-x", "abcd", &["a.rs"])).await,
        FootprintRecorded::Stored { changed: false }
    );
    assert_eq!(env.events().await.len(), after, "a repeat appends nothing");
    assert_eq!(
        report(&env, footprint(item, "bob/1-old", "abcd", &["a.rs"])).await,
        FootprintRecorded::NotStored(NotStored::OtherBranch)
    );
    let member = RecordFootprint::new(&env.deps)
        .execute(&env.bob.actor, footprint(item, "bob/1-x", "abcd", &[]))
        .await
        .unwrap_err();
    assert_eq!(member.code(), "forbidden");
    let elsewhere = RecordFootprint::new(&env.deps)
        .execute(
            &env.importer,
            FootprintInput {
                repo: "acme/elsewhere".to_owned(),
                ..footprint(item, "bob/1-x", "abcd", &[])
            },
        )
        .await
        .unwrap_err();
    assert_eq!(elsewhere.code(), "not_found");
}

#[tokio::test]
async fn two_members_changing_one_file_are_both_told_once_and_again_only_after_it_ended() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1-x").await;
    claim_on(&env, &env.bob, bobs, "bob/2-y").await;
    report(
        &env,
        footprint(alices, "alice/1-x", "abcd", &["config/app.yaml", "a.rs"]),
    )
    .await;
    assert!(overlaps(&env).await.is_empty());
    let before = env.events().await.len();
    report(
        &env,
        footprint(bobs, "bob/2-y", "abcd", &["config/app.yaml"]),
    )
    .await;
    assert_eq!(found(&env.kinds_after(before).await), 1);
    let stored = overlaps(&env).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].kind(), OverlapKind::SameFile);
    assert_eq!(stored[0].items(), &[alices, bobs].into());
    assert_eq!(stored[0].paths(), &[path("config/app.yaml")].into());
    let subject = "Overlap: gh:acme/widgets#1 and gh:acme/widgets#2 change the same files";
    for who in [&env.alice, &env.bob] {
        assert_eq!(
            told(&env, who).await,
            [(subject.to_owned(), Importance::High)]
        );
    }

    report(
        &env,
        footprint(alices, "alice/1-x", "abcd", &["config/app.yaml", "b.rs"]),
    )
    .await;
    report(
        &env,
        footprint(bobs, "bob/2-y", "abcd", &["config/app.yaml", "a.rs"]),
    )
    .await;
    assert_eq!(
        told(&env, &env.bob).await.len(),
        1,
        "still one overlap, told once"
    );
    assert_eq!(found(&env.kinds_after(before).await), 1);

    report(&env, footprint(bobs, "bob/2-y", "abcd", &["README.md"])).await;
    assert!(
        overlaps(&env).await.is_empty(),
        "it ended with the shared file"
    );
    report(
        &env,
        footprint(bobs, "bob/2-y", "abcd", &["config/app.yaml"]),
    )
    .await;
    assert_eq!(overlaps(&env).await.len(), 1);
    assert_eq!(
        told(&env, &env.alice).await.len(),
        2,
        "it came back: told again"
    );
}

#[tokio::test]
async fn one_members_two_claims_never_overlap() {
    let env = Env::with_settings(SETTINGS).await;
    let first = env.item("gh:acme/widgets#1").await;
    let second = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.bob, first, "bob/1").await;
    let other = session(&env.deps, &env.bob.actor, "desktop", "desktop/second").await;
    ClaimItem::new(&env.deps)
        .execute(
            &env.bob.actor,
            ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item: second,
                session: other,
                areas: Vec::new(),
                kind: None,
            },
        )
        .await
        .unwrap();
    Heartbeat::new(&env.deps)
        .execute(
            &env.bob.actor,
            other,
            Some(&in_checkout('b', Some("bob/1"))),
        )
        .await
        .unwrap();
    report(&env, footprint(first, "bob/1", "abcd", &["a.rs"])).await;
    report(&env, footprint(second, "bob/1", "abcd", &["a.rs"])).await;
    assert_eq!(stored_footprints(&env).await, 2);
    assert!(overlaps(&env).await.is_empty());
}

#[tokio::test]
async fn a_contract_change_reaches_the_member_working_in_an_area_that_depends_on_it() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(
        &env,
        footprint(bobs, "bob/2", "abcd", &["packages/search/src/Index.php"]),
    )
    .await;
    report(
        &env,
        footprint(
            alices,
            "alice/1",
            "abcd",
            &["packages/billing/src/Api/Money.php"],
        ),
    )
    .await;
    let stored = overlaps(&env).await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].kind(), OverlapKind::Contract);
    assert_eq!(
        stored[0].paths(),
        &[path("packages/billing/src/Api/Money.php")].into()
    );
    let subject = "Overlap: gh:acme/widgets#1 and gh:acme/widgets#2 meet at a published contract";
    assert_eq!(
        told(&env, &env.bob).await,
        [(subject.to_owned(), Importance::High)]
    );
}

#[tokio::test]
async fn a_settings_change_that_declares_a_contract_finds_the_overlaps_it_makes() {
    let env = Env::with_settings(
        r#"{"areas": [{"name": "billing", "paths": ["packages/billing/**"]},
                      {"name": "search", "paths": ["packages/search/**"]}]}"#,
    )
    .await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(
        &env,
        footprint(
            alices,
            "alice/1",
            "abcd",
            &["packages/billing/src/Api/Money.php"],
        ),
    )
    .await;
    report(
        &env,
        footprint(bobs, "bob/2", "abcd", &["packages/search/src/Index.php"]),
    )
    .await;
    assert!(overlaps(&env).await.is_empty());
    ApplySettings::new(&env.deps)
        .execute(&env.alice.actor, document(SETTINGS))
        .await
        .unwrap();
    let stored = overlaps(&env).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].kind(), OverlapKind::Contract);
    ApplySettings::new(&env.deps)
        .execute(
            &env.alice.actor,
            document(r#"{"areas": [{"name": "billing", "paths": ["packages/billing/**"]}]}"#),
        )
        .await
        .unwrap();
    assert!(
        overlaps(&env).await.is_empty(),
        "without the contract the overlap is gone"
    );
}

/// Claims `alices` and `bobs` on their own branches, each changing only `composer.lock`.
async fn both_change_the_lock_file(env: &Env, alices: ItemId, bobs: ItemId) {
    claim_on(env, &env.alice, alices, "alice/1").await;
    claim_on(env, &env.bob, bobs, "bob/2").await;
    report(
        env,
        footprint(alices, "alice/1", "abcd", &["composer.lock"]),
    )
    .await;
    report(env, footprint(bobs, "bob/2", "abcd", &["composer.lock"])).await;
}

#[tokio::test]
async fn a_settings_change_that_declares_a_file_regenerated_ends_the_overlap_it_alone_made() {
    let env = Env::with_settings(HAND_MERGED).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    both_change_the_lock_file(&env, alices, bobs).await;
    let stored = overlaps(&env).await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].kind(), OverlapKind::SameFile);
    assert_eq!(stored[0].paths(), &[path("composer.lock")].into());
    env.advance(time::Duration::minutes(1));
    merged(
        &env,
        merge(
            &env,
            "acme/widgets#9",
            "main",
            Some(&["composer.lock"]),
            None,
        ),
    )
    .await;
    let mut moved = moved_items(&overlaps(&env).await);
    moved.sort();
    assert_eq!(moved, [alices, bobs]);

    ApplySettings::new(&env.deps)
        .execute(&env.alice.actor, document(REGENERATED))
        .await
        .unwrap();
    let left = overlaps(&env).await;
    assert!(
        left.iter()
            .all(|overlap| overlap.kind() == OverlapKind::MainMoved),
        "nobody merges composer.lock by hand any more: {left:?}"
    );
    let mut kept = moved_items(&left);
    kept.sort();
    assert_eq!(kept, [alices, bobs], "the merge already happened: {left:?}");
}

#[tokio::test]
async fn a_settings_change_that_drops_a_regenerate_rule_finds_and_announces_the_overlap_once() {
    let env = Env::with_settings(REGENERATED).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    both_change_the_lock_file(&env, alices, bobs).await;
    assert!(overlaps(&env).await.is_empty());

    let before = env.events().await.len();
    let apply = ApplySettings::new(&env.deps);
    apply
        .execute(&env.alice.actor, document(HAND_MERGED))
        .await
        .unwrap();
    let stored = overlaps(&env).await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].kind(), OverlapKind::SameFile);
    assert_eq!(stored[0].items(), &[alices, bobs].into());
    assert_eq!(stored[0].paths(), &[path("composer.lock")].into());
    assert_eq!(found(&env.kinds_after(before).await), 1);
    let subject = "Overlap: gh:acme/widgets#1 and gh:acme/widgets#2 change the same files";
    for who in [&env.alice, &env.bob] {
        let texts = told_texts(&env, who).await;
        assert_eq!(texts.len(), 1, "{texts:?}");
        let (told_subject, text) = &texts[0];
        assert_eq!(told_subject, subject);
        assert!(
            text.contains("change the same files: default:composer.lock. "),
            "{text}"
        );
    }

    let again = env.events().await.len();
    let applied = apply
        .execute(&env.alice.actor, document(HAND_MERGED))
        .await
        .unwrap();
    assert!(!applied.changed);
    assert_eq!(
        overlaps(&env).await,
        stored,
        "the same settings change nothing"
    );
    assert!(
        env.kinds_after(again).await.is_empty(),
        "and append nothing"
    );
    for who in [&env.alice, &env.bob] {
        assert_eq!(told_texts(&env, who).await.len(), 1, "told once");
    }
}

/// A merge of `pr` into `base` now, with `files` and `head` when given.
fn merge(env: &Env, pr: &str, base: &str, files: Option<&[&str]>, head: Option<&str>) -> PrReport {
    PrReport {
        pr: PrRef::parse(pr).unwrap(),
        end: ReportedEnd::Merged {
            base: base.to_owned(),
            merged_at: env.now(),
            files: files.map(|files| {
                files
                    .iter()
                    .map(|text| RelPath::parse(text).unwrap())
                    .collect::<BTreeSet<_>>()
            }),
            head: head.map(|text| BranchName::parse(text).unwrap()),
        },
    }
}

async fn merged(env: &Env, report: PrReport) -> crate::use_cases::PrReportOutcome {
    ReportPr::new(&env.deps)
        .execute(&env.importer, report)
        .await
        .unwrap()
}

async fn review(env: &Env, who: &Person, item: ItemId, pr: &str, more: bool) {
    StartReview::new(&env.deps)
        .execute(
            &who.actor,
            StartReviewInput {
                head: None,
                item,
                pr: PrRef::parse(pr).unwrap(),
                more,
            },
        )
        .await
        .unwrap();
}

fn moved_items(stored: &[Overlap]) -> Vec<ItemId> {
    stored
        .iter()
        .filter(|overlap| overlap.kind() == OverlapKind::MainMoved)
        .flat_map(|overlap| overlap.items().iter().copied())
        .collect()
}

#[tokio::test]
async fn a_merge_moves_the_base_under_every_branch_behind_it_its_authors_other_work_included() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    let alices_other = env.item("gh:acme/widgets#3").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    let second = session(&env.deps, &env.alice.actor, "laptop", "laptop/second").await;
    ClaimItem::new(&env.deps)
        .execute(
            &env.alice.actor,
            ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item: alices_other,
                session: second,
                areas: Vec::new(),
                kind: None,
            },
        )
        .await
        .unwrap();
    Heartbeat::new(&env.deps)
        .execute(
            &env.alice.actor,
            second,
            Some(&in_checkout('b', Some("alice/3"))),
        )
        .await
        .unwrap();
    report(
        &env,
        footprint(alices, "alice/1", "abcd", &["a.rs", "b.rs"]),
    )
    .await;
    report(&env, footprint(bobs, "bob/2", "abcd", &["b.rs", "c.rs"])).await;
    report(&env, footprint(alices_other, "alice/3", "abcd", &["a.rs"])).await;
    review(&env, &env.alice, alices, "acme/widgets#7", false).await;
    env.advance(time::Duration::minutes(1));
    assert_eq!(
        merged(&env, merge(&env, "acme/widgets#7", "main", None, None)).await,
        crate::use_cases::PrReportOutcome::Completed(alices)
    );
    let stored = overlaps(&env).await;
    assert_eq!(
        moved_items(&stored),
        [bobs, alices_other],
        "without files, the merged branch's own footprint says what moved; alice's other branch \
         is behind the merge too: {stored:?}"
    );
    assert!(
        !stored
            .iter()
            .any(|overlap| overlap.kind() == OverlapKind::SameFile && overlap.involves(alices)),
        "alice's claim completed, and its overlaps went with it"
    );
    assert_eq!(stored_footprints(&env).await, 2, "and its footprint");
    let base_moved = told(&env, &env.bob).await;
    assert!(
        base_moved.contains(&(
            "Base moved: acme/widgets#7 changed files of gh:acme/widgets#2".to_owned(),
            Importance::Normal
        )),
        "{base_moved:?}"
    );
    let events = env.events().await.len();
    merged(
        &env,
        merge(&env, "acme/widgets#7", "main", Some(&["b.rs"]), None),
    )
    .await;
    assert_eq!(
        env.events().await.len(),
        events,
        "a repeated report finds nothing new"
    );
    let merge_time = env.now();
    report(
        &env,
        footprint_from(bobs, "bob/2", "abcd", START, &["b.rs", "c.rs", "d.rs"]),
    )
    .await;
    assert_eq!(
        moved_items(&overlaps(&env).await),
        [bobs, alices_other],
        "a branch still measured from a base older than the merge stays behind"
    );
    report(
        &env,
        footprint_from(bobs, "bob/2", "beef", merge_time, &["b.rs", "c.rs"]),
    )
    .await;
    assert_eq!(
        moved_items(&overlaps(&env).await),
        [alices_other],
        "a branch measured from a base no older than the merge has caught up"
    );
}

#[tokio::test]
async fn the_next_branch_of_an_item_merged_in_steps_is_behind_the_merge_of_the_one_before() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    claim_on(&env, &env.alice, alices, "alice/1-part-one").await;
    report(
        &env,
        footprint(alices, "alice/1-part-one", "abcd", &["a.rs"]),
    )
    .await;
    review(&env, &env.alice, alices, "acme/widgets#7", true).await;
    named(&env, alices, "alice/1-part-two").await;
    assert_eq!(
        stored_footprints(&env).await,
        0,
        "the footprint of the branch the claim left is gone"
    );
    report(
        &env,
        footprint(alices, "alice/1-part-two", "abcd", &["a.rs", "b.rs"]),
    )
    .await;
    env.advance(time::Duration::minutes(1));
    merged(
        &env,
        merge(
            &env,
            "acme/widgets#7",
            "main",
            Some(&["a.rs"]),
            Some("alice/1-part-one"),
        ),
    )
    .await;
    assert_eq!(
        moved_items(&overlaps(&env).await),
        [alices],
        "the next branch, based before the merge, is told to rebase"
    );
    assert_eq!(
        stored_footprints(&env).await,
        1,
        "the next branch's footprint stays"
    );
}

#[tokio::test]
async fn a_merge_that_leaves_its_claim_live_takes_the_merged_branchs_footprint() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(
        &env,
        footprint(alices, "alice/1", "abcd", &["config/app.yaml"]),
    )
    .await;
    report(&env, footprint(bobs, "bob/2", "abcd", &["config/app.yaml"])).await;
    assert_eq!(overlaps(&env).await.len(), 1);
    review(&env, &env.alice, alices, "acme/widgets#7", true).await;
    env.advance(time::Duration::minutes(1));
    assert_eq!(
        merged(
            &env,
            merge(&env, "acme/widgets#7", "main", None, Some("alice/1"))
        )
        .await,
        crate::use_cases::PrReportOutcome::Working(alices)
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.footprint(env.project, alices, &repo())
            .await
            .unwrap()
            .is_none(),
        "its work is in the base"
    );
    drop(tx);
    let stored = overlaps(&env).await;
    assert_eq!(
        stored.iter().map(Overlap::kind).collect::<Vec<_>>(),
        [OverlapKind::MainMoved],
        "the shared file ended with the merged branch; bob's base moved: {stored:?}"
    );
}

#[tokio::test]
async fn a_merge_into_another_branch_than_a_footprints_base_moves_nothing_under_it() {
    let env = Env::with_settings(SETTINGS).await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(&env, footprint(bobs, "bob/2", "abcd", &["b.rs"])).await;
    env.advance(time::Duration::minutes(1));
    for base in ["release/2", "alice/9-stacked"] {
        merged(
            &env,
            merge(&env, "acme/widgets#9", base, Some(&["b.rs"]), None),
        )
        .await;
        assert!(overlaps(&env).await.is_empty(), "{base}");
    }
    merged(
        &env,
        merge(
            &env,
            "acme/widgets#9",
            "refs/heads/main",
            Some(&["b.rs"]),
            None,
        ),
    )
    .await;
    assert_eq!(moved_items(&overlaps(&env).await), [bobs]);
}

#[tokio::test]
async fn a_merge_nobody_recorded_moves_the_base_under_the_branches_that_change_its_files() {
    let env = Env::with_settings(SETTINGS).await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(&env, footprint(bobs, "bob/2", "abcd", &["b.rs"])).await;
    env.advance(time::Duration::minutes(5));
    assert_eq!(
        merged(
            &env,
            merge(&env, "acme/widgets#9", "main", Some(&["b.rs"]), None)
        )
        .await,
        crate::use_cases::PrReportOutcome::Unmatched
    );
    let stored = overlaps(&env).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].kind(), OverlapKind::MainMoved);
    let closed = ReportPr::new(&env.deps)
        .execute(
            &env.importer,
            PrReport {
                pr: PrRef::parse("acme/widgets#10").unwrap(),
                end: ReportedEnd::Closed,
            },
        )
        .await
        .unwrap();
    assert_eq!(closed, crate::use_cases::PrReportOutcome::Unmatched);
    assert_eq!(overlaps(&env).await.len(), 1, "a close moves no base");
}

#[tokio::test]
async fn a_branch_change_takes_the_old_branchs_footprint_and_what_it_made() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(
        &env,
        footprint(alices, "alice/1", "abcd", &["packages/billing/a.php"]),
    )
    .await;
    report(
        &env,
        footprint(bobs, "bob/2", "abcd", &["packages/billing/a.php"]),
    )
    .await;
    assert_eq!(overlaps(&env).await.len(), 1);
    EditCheck::new(&env.deps)
        .execute(
            &env.bob.actor,
            EditCheckInput {
                session: env.bob.session,
                paths: vec![path("packages/search/a.php")],
                reserve: true,
                checkout: Some(on("bob/2-again")),
                worktree: None,
            },
        )
        .await
        .unwrap();
    assert!(
        overlaps(&env).await.is_empty(),
        "bob's old branch's overlap ended"
    );
    assert_eq!(stored_footprints(&env).await, 1);
}

#[tokio::test]
async fn two_sessions_of_one_machine_on_two_branches_never_move_each_others_claims() {
    let env = Env::with_settings(SETTINGS).await;
    let first = env.item("gh:acme/widgets#1").await;
    let second = env.item("gh:acme/widgets#2").await;
    let other = session(&env.deps, &env.bob.actor, "desktop", "desktop/second").await;
    for (item, at) in [(first, env.bob.session), (second, other)] {
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    branches: Default::default(),
                    wide: None,
                    item,
                    session: at,
                    areas: Vec::new(),
                    kind: None,
                },
            )
            .await
            .unwrap();
    }
    let heartbeat = Heartbeat::new(&env.deps);
    for _ in 0..3 {
        heartbeat
            .execute(&env.bob.actor, env.bob.session, Some(&on("bob/1")))
            .await
            .unwrap();
        heartbeat
            .execute(&env.bob.actor, other, Some(&in_checkout('b', Some("main"))))
            .await
            .unwrap();
    }
    let mut tx = env.deps.store.begin().await.unwrap();
    let branch_of = |claim: Option<dibs_domain::claim::Claim>| {
        claim.and_then(|claim| claim.branch(&repo()).map(ToString::to_string))
    };
    assert_eq!(
        branch_of(tx.claim(env.project, first).await.unwrap()).as_deref(),
        Some("bob/1")
    );
    assert_eq!(
        branch_of(tx.claim(env.project, second).await.unwrap()).as_deref(),
        Some("main")
    );
    drop(tx);
    let recorded = env
        .events()
        .await
        .iter()
        .filter(|event| matches!(event.kind, EventKind::BranchRecorded(_)))
        .count();
    assert_eq!(recorded, 2, "each claim's branch once, never flipped");
}

#[tokio::test]
async fn going_stale_ends_no_overlap_and_coming_back_announces_none_again() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(
        &env,
        footprint(alices, "alice/1", "abcd", &["config/app.yaml"]),
    )
    .await;
    report(&env, footprint(bobs, "bob/2", "abcd", &["config/app.yaml"])).await;
    assert_eq!(told(&env, &env.alice).await.len(), 1);
    env.advance(time::Duration::hours(20));
    Heartbeat::new(&env.deps)
        .execute(&env.alice.actor, env.alice.session, None)
        .await
        .unwrap();
    env.advance(time::Duration::hours(5));
    report(
        &env,
        footprint(alices, "alice/1", "abcd", &["config/app.yaml", "a.rs"]),
    )
    .await;
    assert_eq!(
        overlaps(&env).await.len(),
        1,
        "bob's stale claim ends nothing"
    );
    let status = Status::new(&env.deps)
        .execute(&env.alice.actor)
        .await
        .unwrap();
    assert!(
        status.overlaps.is_empty(),
        "while it is stale it is not shown"
    );
    claim(&env, &env.bob, bobs).await;
    report(
        &env,
        footprint(alices, "alice/1", "abcd", &["config/app.yaml"]),
    )
    .await;
    assert_eq!(overlaps(&env).await.len(), 1);
    assert_eq!(
        told(&env, &env.alice).await.len(),
        1,
        "never announced again"
    );
    assert_eq!(told(&env, &env.bob).await.len(), 1);
}

#[tokio::test]
async fn a_takeover_clears_the_previous_holders_footprints_and_overlaps() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(
        &env,
        footprint(alices, "alice/1", "abcd", &["config/app.yaml"]),
    )
    .await;
    report(&env, footprint(bobs, "bob/2", "abcd", &["config/app.yaml"])).await;
    env.advance(time::Duration::hours(20));
    Heartbeat::new(&env.deps)
        .execute(&env.alice.actor, env.alice.session, None)
        .await
        .unwrap();
    env.advance(time::Duration::hours(5));
    TakeOver::new(&env.deps)
        .execute(
            &env.alice.actor,
            TakeOverInput {
                item: bobs,
                session: env.alice.session,
                areas: Vec::new(),
                reason: dibs_domain::message::Body::parse("bob is away this week").unwrap(),
                kind: None,
            },
        )
        .await
        .unwrap();
    assert!(overlaps(&env).await.is_empty());
    let mut tx = env.deps.store.begin().await.unwrap();
    assert!(
        tx.footprint(env.project, bobs, &repo())
            .await
            .unwrap()
            .is_none()
    );
    let taken = tx.claim(env.project, bobs).await.unwrap().unwrap();
    assert!(
        taken.branches.is_empty(),
        "the new holder's branch is still to be seen"
    );
}

#[tokio::test]
async fn a_repository_the_settings_no_longer_list_takes_its_footprints_and_overlaps() {
    let two = r#"{"repos": [
        {"id": "api", "remote": "github.com/acme/api"},
        {"id": "web", "remote": "github.com/acme/web"}]}"#;
    let env = Env::with_settings(two).await;
    let alices = env.item("gh:acme/api#1").await;
    let bobs = env.item("gh:acme/api#2").await;
    for (who, item, branch) in [(&env.alice, alices, "alice/1"), (&env.bob, bobs, "bob/2")] {
        claim(&env, who, item).await;
        for repo in ["api", "web"] {
            Heartbeat::new(&env.deps)
                .execute(
                    &who.actor,
                    who.session,
                    Some(&FromCheckout {
                        token: checkout('a'),
                        branch: Some(OnBranch {
                            repo: RepoId::parse(repo).unwrap(),
                            branch: BranchName::parse(branch).unwrap(),
                        }),
                    }),
                )
                .await
                .unwrap();
            report(
                &env,
                FootprintInput {
                    repo: repo.to_owned(),
                    ..footprint(item, branch, "abcd", &[&format!("{repo}.rs")])
                },
            )
            .await;
        }
    }
    let before = overlaps(&env).await;
    assert_eq!(
        before.len(),
        1,
        "one overlap per pair of claims, on paths of both"
    );
    assert_eq!(before[0].paths().len(), 2);
    ApplySettings::new(&env.deps)
        .execute(
            &env.alice.actor,
            document(r#"{"repos": [{"id": "api", "remote": "github.com/acme/api"}]}"#),
        )
        .await
        .unwrap();
    let left = overlaps(&env).await;
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(
        left[0]
            .paths()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["api:api.rs"],
        "only the listed repository's path is left"
    );
    let mut tx = env.deps.store.begin().await.unwrap();
    let repos: Vec<String> = tx
        .footprints(env.project)
        .await
        .unwrap()
        .iter()
        .map(|footprint| footprint.repo.to_string())
        .collect();
    assert_eq!(repos, ["api", "api"]);
}

#[tokio::test]
async fn a_released_claim_takes_its_footprints_and_overlaps_with_it_and_status_shows_live_ones() {
    let env = Env::with_settings(SETTINGS).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(&env, footprint(alices, "alice/1", "abcd", &["a.rs"])).await;
    report(&env, footprint(bobs, "bob/2", "abcd", &["a.rs"])).await;
    let status = Status::new(&env.deps)
        .execute(&env.alice.actor)
        .await
        .unwrap();
    assert_eq!(status.overlaps.len(), 1);
    let handles: Vec<&str> = status.overlaps[0]
        .items
        .iter()
        .map(|(_, holder)| holder.as_str())
        .collect();
    assert_eq!(handles, ["alice", "bob"]);
    Release::new(&env.deps)
        .execute(&env.bob.actor, bobs)
        .await
        .unwrap();
    assert!(overlaps(&env).await.is_empty());
    assert_eq!(stored_footprints(&env).await, 1);
    let status = Status::new(&env.deps)
        .execute(&env.alice.actor)
        .await
        .unwrap();
    assert!(status.overlaps.is_empty());
}

#[tokio::test]
async fn branches_sharing_only_a_regenerated_file_do_not_overlap_and_a_hand_merged_one_does() {
    let env = Env::with_settings(REGENERATED).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(
        &env,
        footprint(alices, "alice/1", "abcd", &["composer.lock"]),
    )
    .await;
    let before = env.events().await.len();
    report(&env, footprint(bobs, "bob/2", "abcd", &["composer.lock"])).await;
    assert!(
        overlaps(&env).await.is_empty(),
        "nobody merges composer.lock by hand"
    );
    assert_eq!(found(&env.kinds_after(before).await), 0);
    for who in [&env.alice, &env.bob] {
        assert!(told_texts(&env, who).await.is_empty());
    }

    let both = ["composer.lock", "src/billing/round.rs"];
    report(&env, footprint(alices, "alice/1", "abcd", &both)).await;
    report(&env, footprint(bobs, "bob/2", "abcd", &both)).await;
    let stored = overlaps(&env).await;
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].kind(), OverlapKind::SameFile);
    assert_eq!(stored[0].paths(), &[path("src/billing/round.rs")].into());
    for who in [&env.alice, &env.bob] {
        let texts = told_texts(&env, who).await;
        assert_eq!(texts.len(), 1, "{texts:?}");
        let (_, text) = &texts[0];
        assert!(
            text.contains("change the same files: default:src/billing/round.rs. "),
            "{text}"
        );
        assert!(!text.contains("composer.lock"), "{text}");
    }
}

#[tokio::test]
async fn a_merge_through_a_regenerated_file_alone_moves_no_base_and_names_only_hand_merged_paths() {
    let env = Env::with_settings(REGENERATED).await;
    let alices = env.item("gh:acme/widgets#1").await;
    let bobs = env.item("gh:acme/widgets#2").await;
    claim_on(&env, &env.alice, alices, "alice/1").await;
    claim_on(&env, &env.bob, bobs, "bob/2").await;
    report(
        &env,
        footprint(
            alices,
            "alice/1",
            "abcd",
            &["composer.lock", "src/search/index.rs"],
        ),
    )
    .await;
    report(
        &env,
        footprint(
            bobs,
            "bob/2",
            "abcd",
            &["composer.lock", "src/billing/round.rs"],
        ),
    )
    .await;
    assert!(overlaps(&env).await.is_empty());
    env.advance(time::Duration::minutes(1));
    let before = env.events().await.len();
    merged(
        &env,
        merge(
            &env,
            "acme/widgets#9",
            "main",
            Some(&["composer.lock"]),
            None,
        ),
    )
    .await;
    assert!(
        overlaps(&env).await.is_empty(),
        "nobody merges composer.lock by hand, so no branch is behind"
    );
    assert_eq!(found(&env.kinds_after(before).await), 0);
    for who in [&env.alice, &env.bob] {
        assert!(told_texts(&env, who).await.is_empty());
    }

    merged(
        &env,
        merge(
            &env,
            "acme/widgets#10",
            "main",
            Some(&[
                "composer.lock",
                "src/billing/round.rs",
                "src/search/index.rs",
            ]),
            None,
        ),
    )
    .await;
    let stored = overlaps(&env).await;
    assert_eq!(moved_items(&stored), [alices, bobs], "{stored:?}");
    for (overlap, only) in stored
        .iter()
        .zip(["src/search/index.rs", "src/billing/round.rs"])
    {
        assert_eq!(overlap.paths(), &[path(only)].into());
    }
    for (who, only) in [
        (&env.alice, "default:src/search/index.rs"),
        (&env.bob, "default:src/billing/round.rs"),
    ] {
        let texts = told_texts(&env, who).await;
        assert_eq!(texts.len(), 1, "{texts:?}");
        let (subject, text) = &texts[0];
        assert!(
            subject.starts_with("Base moved: acme/widgets#10"),
            "{subject}"
        );
        assert!(
            text.contains(&format!("changed files that branch changes too: {only}. ")),
            "{text}"
        );
        assert!(!text.contains("composer.lock"), "{text}");
    }
}

#[tokio::test]
async fn a_report_left_by_a_session_gone_quiet_stops_blocking_its_checkout_after_a_few_beats() {
    let env = Env::with_settings(SETTINGS).await;
    let item = env.item("gh:acme/widgets#9").await;
    let crashed = session(&env.deps, &env.bob.actor, "desktop", "desktop/crashed").await;
    let heartbeat = Heartbeat::new(&env.deps);
    heartbeat
        .execute(&env.bob.actor, crashed, Some(&on("bob/8-totals")))
        .await
        .unwrap();
    env.advance(time::Duration::minutes(1));
    claim(&env, &env.bob, item).await;
    let before = env.events().await.len();
    heartbeat
        .execute(&env.bob.actor, env.bob.session, Some(&on("bob/9-search")))
        .await
        .unwrap();
    let branch_of = || async {
        let mut tx = env.deps.store.begin().await.unwrap();
        let claim = tx.claim(env.project, item).await.unwrap().unwrap();
        claim.branch(&repo()).map(ToString::to_string)
    };
    assert_eq!(
        branch_of().await,
        None,
        "a report a minute old may still be a session sharing the checkout"
    );
    env.advance(time::Duration::minutes(3));
    heartbeat
        .execute(&env.bob.actor, env.bob.session, Some(&on("bob/9-search")))
        .await
        .unwrap();
    assert_eq!(branch_of().await.as_deref(), Some("bob/9-search"));
    let recorded: Vec<String> = env
        .kinds_after(before)
        .await
        .into_iter()
        .filter_map(|kind| match kind {
            EventKind::BranchRecorded(recorded) => Some(recorded.branch.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(recorded, ["bob/9-search"]);
}
