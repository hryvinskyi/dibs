use std::collections::BTreeSet;

use time::Duration;

use super::*;
use crate::claim::Claim;
use crate::ids::{ItemId, MemberId, SessionId};
use crate::message::Importance;
use crate::paths::{RelPath, RepoPath};
use crate::repo::RepoId;
use crate::review::PrRef;
use crate::settings::ProjectSettings;
use crate::testing::{ALICE, BOB, CAROL, area, at, claim, settings};

/// Two repositories: `api` with `billing` and `search` (search depends on billing), a contract
/// over every package's `src/Api/` and one over `schema/` consumed by all of `web`; `web` with
/// `checkout`, which depends on nothing.
fn radar_settings() -> ProjectSettings {
    settings(
        r#"{"repos":[
            {"id":"api","remote":"example.com/acme/api",
             "areas":[{"name":"billing","paths":["packages/billing/**"]},
                      {"name":"search","paths":["packages/search/**"]}],
             "contracts":[{"paths":["packages/*/src/Api/**"]},
                          {"paths":["schema/**"],"consumers":["web:*"]}],
             "area_deps":{"search":["billing"]}},
            {"id":"web","remote":"example.com/acme/web",
             "areas":[{"name":"checkout","paths":["src/checkout/**"]}]}]}"#,
    )
}

fn repo(text: &str) -> RepoId {
    RepoId::parse(text).unwrap()
}

fn branch(text: &str) -> BranchName {
    BranchName::parse(text).unwrap()
}

fn sha(text: &str) -> CommitSha {
    CommitSha::parse(text).unwrap()
}

fn rel(text: &str) -> RelPath {
    RelPath::parse(text).unwrap()
}

fn qualified(text: &str) -> RepoPath {
    RepoPath::parse(text).unwrap()
}

fn rels(texts: &[&str]) -> BTreeSet<RelPath> {
    texts.iter().map(|text| rel(text)).collect()
}

fn qualified_set(texts: &[&str]) -> BTreeSet<RepoPath> {
    texts.iter().map(|text| qualified(text)).collect()
}

fn pr(text: &str) -> PrRef {
    PrRef::parse(text).unwrap()
}

/// A claim of `member` on `item`, last seen at minute 0, its work on `branch` in `api`.
fn on_branch(item: u64, member: MemberId, name: &str) -> Claim {
    let mut made = claim(item, member, &[], 0);
    made.branches.insert(repo("api"), branch(name));
    made
}

/// A base on `main` whose merge base was committed at `minutes`.
fn main_at(minutes: i64) -> FootprintBase {
    FootprintBase {
        branch: branch("main"),
        sha: sha("abcd"),
        at: at(minutes),
    }
}

/// The footprint of `item`'s branch `work-<item>` in `repo` over `paths`, measured against `main`
/// from a merge base committed at `minutes`.
fn footprint(
    item: u64,
    in_repo: &str,
    paths: &[&str],
    minutes: i64,
) -> (Footprint, Vec<FootprintEntry>) {
    let paths = rels(paths);
    let settings = radar_settings();
    let made = Footprint {
        item: ItemId(item),
        repo: repo(in_repo),
        branch: branch(&format!("work-{item}")),
        base: main_at(minutes),
        truncated: false,
        path_count: paths.len(),
        digest: FootprintDigest::of(&repo(in_repo), &paths, &settings),
    };
    let entries = paths
        .into_iter()
        .map(|path| FootprintEntry {
            item: ItemId(item),
            path: RepoPath {
                repo: repo(in_repo),
                path,
            },
        })
        .collect();
    (made, entries)
}

fn report(item: u64, name: &str, base: &str, paths: &[&str]) -> FootprintReport {
    FootprintReport {
        item: ItemId(item),
        repo: repo("api"),
        branch: branch(name),
        base: FootprintBase {
            sha: sha(base),
            ..main_at(0)
        },
        paths: rels(paths),
        truncated: false,
    }
}

#[test]
fn a_branch_is_recorded_only_when_it_is_another_one() {
    let mut made = claim(1, ALICE, &[], 0);
    let first = record_branch(&made, &repo("api"), &branch("alice/1-x")).unwrap();
    assert_eq!(first.branch(&repo("api")), Some(&branch("alice/1-x")));
    assert_eq!(
        record_branch(&first, &repo("api"), &branch("alice/1-x")),
        None
    );
    let moved = record_branch(&first, &repo("api"), &branch("alice/1-y")).unwrap();
    assert_eq!(moved.branch(&repo("api")), Some(&branch("alice/1-y")));
    made.branches.insert(repo("web"), branch("main"));
    let both = record_branch(&made, &repo("api"), &branch("alice/1-x")).unwrap();
    assert_eq!(both.branches.len(), 2, "one branch per repository");
}

#[test]
fn a_heartbeat_records_its_branch_on_the_claims_of_its_own_session_only() {
    let mut first = on_branch(1, ALICE, "old");
    first.session = SessionId(1);
    let mut second = claim(2, ALICE, &[], 0);
    second.session = SessionId(1);
    let mut other_worktree = on_branch(3, ALICE, "alice/3-search");
    other_worktree.session = SessionId(2);
    let kept = [first, second, other_worktree];
    let set = heartbeat_branches(&kept, SessionId(1), &repo("api"), &branch("alice/1-rates"));
    let items: Vec<ItemId> = set.iter().map(|claim| claim.item).collect();
    assert_eq!(
        items,
        [ItemId(1), ItemId(2)],
        "every claim of the session, none of another session on the same machine"
    );
    assert!(
        set.iter()
            .all(|claim| claim.branch(&repo("api")) == Some(&branch("alice/1-rates")))
    );
    let theirs = heartbeat_branches(&kept, SessionId(2), &repo("api"), &branch("alice/3-search"));
    assert!(
        theirs.is_empty(),
        "the other session's claim is on its branch already"
    );
    let moved = heartbeat_branches(&kept, SessionId(2), &repo("api"), &branch("alice/3-next"));
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].item, ItemId(3));
    assert!(heartbeat_branches(&kept, SessionId(9), &repo("api"), &branch("x")).is_empty());
}

#[test]
fn a_digest_knows_the_areas_the_unzoned_paths_and_the_contract_paths() {
    let settings = radar_settings();
    let digest = FootprintDigest::of(
        &repo("api"),
        &rels(&[
            "packages/billing/src/Api/Money.php",
            "packages/billing/src/Service/Rate.php",
            "packages/search/src/Index.php",
            "README.md",
            "schema/invoice.json",
        ]),
        &settings,
    );
    assert_eq!(
        digest.areas,
        [area("api:billing"), area("api:search")].into()
    );
    assert!(digest.unzoned);
    assert_eq!(
        digest.contracts,
        rels(&["packages/billing/src/Api/Money.php", "schema/invoice.json"])
    );
    let elsewhere = FootprintDigest::of(&repo("other"), &rels(&["a.rs"]), &settings);
    assert!(elsewhere.areas.is_empty() && elsewhere.contracts.is_empty() && elsewhere.unzoned);
    let nothing = FootprintDigest::of(&repo("api"), &BTreeSet::new(), &settings);
    assert_eq!(nothing, FootprintDigest::default());
}

#[test]
fn a_footprint_is_stored_only_for_a_live_claim_on_the_reported_branch() {
    let settings = radar_settings();
    let ask = report(1, "alice/1-x", "abcd", &["a.rs"]);
    assert_eq!(
        decide_footprint(None, None, &ask, &settings, at(1)),
        FootprintOutcome::NotStored(NotStored::NoLiveClaim)
    );
    let live = on_branch(1, ALICE, "alice/1-x");
    let late = at(0) + settings.stale_after() + Duration::minutes(1);
    assert_eq!(
        decide_footprint(Some(&live), None, &ask, &settings, late),
        FootprintOutcome::NotStored(NotStored::NoLiveClaim)
    );
    let elsewhere = on_branch(1, ALICE, "alice/1-other");
    assert_eq!(
        decide_footprint(Some(&elsewhere), None, &ask, &settings, at(1)),
        FootprintOutcome::NotStored(NotStored::OtherBranch)
    );
    assert_eq!(
        decide_footprint(Some(&claim(1, ALICE, &[], 0)), None, &ask, &settings, at(1)),
        FootprintOutcome::NotStored(NotStored::OtherBranch),
        "a claim with no branch recorded there"
    );
    assert_eq!(NotStored::NoLiveClaim.code(), "no_live_claim");
    assert_eq!(NotStored::OtherBranch.code(), "other_branch");
}

#[test]
fn a_footprint_replaces_the_one_before_and_a_repeat_changes_nothing() {
    let settings = radar_settings();
    let live = on_branch(1, ALICE, "alice/1-x");
    let first = report(1, "alice/1-x", "abcd", &["packages/billing/a.php"]);
    let FootprintOutcome::Replaced { footprint, member } =
        decide_footprint(Some(&live), None, &first, &settings, at(1))
    else {
        panic!("the first report is stored");
    };
    assert_eq!(member, ALICE);
    assert_eq!(footprint.base, first.base);
    assert_eq!(footprint.path_count, 1);
    assert_eq!(footprint.digest.areas, [area("api:billing")].into());
    let paths = first.paths.clone();
    let current = Some((&footprint, &paths));
    assert_eq!(
        decide_footprint(Some(&live), current, &first, &settings, at(2)),
        FootprintOutcome::Unchanged
    );
    let changes = [
        report(1, "alice/1-x", "abcd", &["packages/billing/a.php", "b.rs"]),
        FootprintReport {
            truncated: true,
            ..first.clone()
        },
        report(1, "alice/1-x", "beef", &["packages/billing/a.php"]),
        FootprintReport {
            base: main_at(30),
            ..first.clone()
        },
        FootprintReport {
            base: FootprintBase {
                branch: branch("release/2"),
                ..main_at(0)
            },
            ..first.clone()
        },
    ];
    for changed in changes {
        assert!(
            matches!(
                decide_footprint(Some(&live), current, &changed, &settings, at(3)),
                FootprintOutcome::Replaced { .. }
            ),
            "{changed:?}"
        );
    }
}

#[test]
fn a_base_is_behind_a_later_merge_into_its_own_branch_only() {
    let base = main_at(10);
    assert!(base.behind(&branch("main"), at(11)));
    assert!(
        !base.behind(&branch("main"), at(10)),
        "a merge at the base's own time is in it"
    );
    assert!(!base.behind(&branch("main"), at(9)));
    assert!(
        !base.behind(&branch("release/2"), at(11)),
        "a merge into another branch"
    );
}

#[test]
fn two_members_changing_one_path_overlap_on_the_shared_paths_only() {
    let settings = radar_settings();
    let claims = [
        claim(1, ALICE, &[], 0),
        claim(2, BOB, &[], 0),
        claim(3, ALICE, &[], 0),
    ];
    let (a, a_entries) = footprint(1, "api", &["config/app.yaml", "packages/billing/a.php"], 0);
    let (b, b_entries) = footprint(2, "api", &["config/app.yaml", "README.md"], 0);
    let (c, c_entries) = footprint(3, "api", &["packages/billing/a.php"], 0);
    let (w, w_entries) = footprint(2, "web", &["packages/billing/a.php"], 0);
    let footprints = [a, b, c, w];
    let scene = RadarScene {
        claims: &claims,
        footprints: &footprints,
        settings: &settings,
        now: at(1),
    };
    let others: Vec<FootprintEntry> = [b_entries, c_entries, w_entries].concat();
    let mine: BTreeSet<RepoPath> = a_entries.iter().map(|entry| entry.path.clone()).collect();
    let found = scene.same_file(ItemId(1), &mine, &others);
    let expected = Overlap::new(
        OverlapKind::SameFile,
        [ItemId(1), ItemId(2)].into(),
        None,
        qualified_set(&["api:config/app.yaml"]),
    )
    .unwrap();
    assert_eq!(
        found,
        [expected].into(),
        "not with alice's own other claim, and a path of another repository is another path"
    );
    assert!(scene.all_live(&expected_overlap()));
    let late = at(0) + settings.stale_after() + Duration::minutes(1);
    let mut renewed = claims.clone();
    renewed[0].last_seen = late;
    let stale_other = RadarScene {
        claims: &renewed,
        now: late,
        ..scene
    };
    assert_eq!(
        stale_other.same_file(ItemId(1), &mine, &others).len(),
        1,
        "the overlap holds while bob's claim is stale"
    );
    assert!(
        !stale_other.all_live(&expected_overlap()),
        "and is not announced while it is"
    );
}

fn expected_overlap() -> Overlap {
    Overlap::new(
        OverlapKind::SameFile,
        [ItemId(1), ItemId(2)].into(),
        None,
        qualified_set(&["api:config/app.yaml"]),
    )
    .unwrap()
}

#[test]
fn a_contract_change_overlaps_with_another_members_work_in_an_area_that_depends_on_it() {
    let settings = radar_settings();
    let claims = [
        claim(1, ALICE, &[], 0),
        claim(2, BOB, &[], 0),
        claim(3, CAROL, &[], 0),
        claim(4, ALICE, &[], 0),
    ];
    let (contract, _) = footprint(1, "api", &["packages/billing/src/Api/Money.php"], 0);
    let (dependent, _) = footprint(2, "api", &["packages/search/src/Index.php"], 0);
    let (same_area, _) = footprint(3, "api", &["packages/billing/src/Service/Rate.php"], 0);
    let (own, _) = footprint(4, "api", &["packages/search/src/Query.php"], 0);
    let footprints = [contract, dependent, same_area, own];
    let scene = RadarScene {
        claims: &claims,
        footprints: &footprints,
        settings: &settings,
        now: at(1),
    };
    let expected = Overlap::new(
        OverlapKind::Contract,
        [ItemId(1), ItemId(2)].into(),
        None,
        qualified_set(&["api:packages/billing/src/Api/Money.php"]),
    )
    .unwrap();
    assert_eq!(
        scene.contracts(),
        [expected].into(),
        "work inside the contract's own area is not a consumer, and alice's own work is hers"
    );
    let reversed = settings_without_dependency();
    let independent = RadarScene {
        settings: &reversed,
        ..scene
    };
    assert!(
        independent.contracts().is_empty(),
        "without the dependency the search area consumes nothing"
    );
}

fn settings_without_dependency() -> ProjectSettings {
    settings(
        r#"{"repos":[
            {"id":"api","remote":"example.com/acme/api",
             "areas":[{"name":"billing","paths":["packages/billing/**"]},
                      {"name":"search","paths":["packages/search/**"]}],
             "contracts":[{"paths":["packages/*/src/Api/**"]}]},
            {"id":"web","remote":"example.com/acme/web"}]}"#,
    )
}

#[test]
fn a_contract_consumed_by_another_repository_overlaps_with_any_work_there() {
    let settings = radar_settings();
    let claims = [
        claim(1, ALICE, &[], 0),
        claim(2, BOB, &[], 0),
        claim(3, CAROL, &[], 0),
    ];
    let (schema, _) = footprint(1, "api", &["schema/invoice.json", "README.md"], 0);
    let (checkout, _) = footprint(2, "web", &["src/checkout/pay.ts"], 0);
    let (unzoned, _) = footprint(3, "web", &["package.json"], 0);
    let footprints = [schema, checkout, unzoned];
    let scene = RadarScene {
        claims: &claims,
        footprints: &footprints,
        settings: &settings,
        now: at(1),
    };
    let paths = qualified_set(&["api:schema/invoice.json"]);
    let found = scene.contracts();
    assert_eq!(found.len(), 2, "{found:?}");
    for (overlap, other) in found.iter().zip([2, 3]) {
        assert_eq!(overlap.items(), &[ItemId(1), ItemId(other)].into());
        assert_eq!(overlap.paths(), &paths);
        assert_eq!(overlap.kind().importance(), Importance::High);
    }
    let (api_work, _) = footprint(2, "api", &["packages/search/src/Index.php"], 0);
    let only_api = [footprints[0].clone(), api_work];
    let scene = RadarScene {
        footprints: &only_api,
        ..scene
    };
    assert!(
        scene.contracts().is_empty(),
        "the schema's consumers are in web; search depends on billing, not on schema/"
    );
}

#[test]
fn a_merge_moves_the_base_under_every_live_branch_behind_it_that_changes_its_files() {
    let settings = radar_settings();
    let claims = [
        claim(1, ALICE, &[], 0),
        claim(2, BOB, &[], 0),
        claim(3, CAROL, &[], 0),
        claim(4, ALICE, &[], 0),
        claim(5, BOB, &[], 0),
        claim(6, CAROL, &[], 0),
    ];
    let (merged, merged_entries) = footprint(1, "api", &["a.rs"], 0);
    let (behind, behind_entries) = footprint(2, "api", &["a.rs", "b.rs"], 5);
    let (rebased, rebased_entries) = footprint(3, "api", &["a.rs"], 20);
    let (own, own_entries) = footprint(4, "api", &["a.rs"], 5);
    let (apart, apart_entries) = footprint(5, "api", &["c.rs"], 5);
    let (release, release_entries) = footprint(6, "api", &["a.rs"], 5);
    let release = Footprint {
        base: FootprintBase {
            branch: branch("release/2"),
            ..release.base
        },
        ..release
    };
    let footprints = [merged, behind, rebased, own, apart, release];
    let hits: Vec<FootprintEntry> = [
        merged_entries,
        behind_entries,
        rebased_entries,
        own_entries,
        apart_entries,
        release_entries,
    ]
    .concat();
    let scene = RadarScene {
        claims: &claims,
        footprints: &footprints,
        settings: &settings,
        now: at(15),
    };
    let files = rels(&["a.rs"]);
    let merge = Merge {
        pr: &pr("acme/api#12"),
        repo: &repo("api"),
        base: &branch("main"),
        merged_at: at(10),
        files: &files,
        head: None,
        author: Some(ItemId(1)),
    };
    let items = |merge: &Merge<'_>| -> Vec<ItemId> {
        scene
            .main_moved(merge, &hits)
            .iter()
            .flat_map(|overlap| overlap.items().iter().copied())
            .collect()
    };
    assert_eq!(
        items(&merge),
        [ItemId(2), ItemId(4)],
        "the author's other item is behind too; not the merged branch, not a branch based after \
         the merge, not one apart, not one against another base"
    );
    let found = scene.main_moved(&merge, &hits);
    let first = found.first().unwrap();
    assert_eq!(first.paths(), &qualified_set(&["api:a.rs"]));
    assert_eq!(
        first.merge(),
        Some(&MergeMark {
            pr: pr("acme/api#12"),
            at: at(10)
        })
    );
    assert_eq!(OverlapKind::MainMoved.importance(), Importance::Normal);
    let into_release = Merge {
        base: &branch("release/2"),
        ..merge
    };
    assert_eq!(
        items(&into_release),
        [ItemId(6)],
        "only a footprint against that base"
    );
    let stacked = Merge {
        base: &branch("work-9"),
        ..merge
    };
    assert!(
        items(&stacked).is_empty(),
        "a merge into a feature branch moves no base"
    );
    let other_repo = Merge {
        repo: &repo("web"),
        ..merge
    };
    assert!(scene.main_moved(&other_repo, &hits).is_empty());
    let late = at(0) + settings.stale_after() + Duration::minutes(1);
    let mut gone_stale = claims.clone();
    gone_stale[1].last_seen = at(-10_000);
    gone_stale[3].last_seen = late;
    let stale = RadarScene {
        claims: &gone_stale,
        now: late,
        ..scene
    };
    assert_eq!(
        stale
            .main_moved(&merge, &hits)
            .iter()
            .flat_map(|overlap| overlap.items().iter().copied())
            .collect::<Vec<_>>(),
        [ItemId(4)],
        "a stale claim is not found anew"
    );
}

#[test]
fn the_merged_branch_is_known_by_its_head_so_the_next_branch_of_the_same_item_is_behind() {
    let settings = radar_settings();
    let mut next = claim(1, ALICE, &[], 0);
    next.branches
        .insert(repo("api"), branch("alice/1-part-two"));
    let claims = [next];
    let (footprint_two, entries) = footprint(1, "api", &["a.rs"], 5);
    let footprint_two = Footprint {
        branch: branch("alice/1-part-two"),
        ..footprint_two
    };
    let footprints = [footprint_two.clone()];
    let scene = RadarScene {
        claims: &claims,
        footprints: &footprints,
        settings: &settings,
        now: at(15),
    };
    let files = rels(&["a.rs"]);
    let head = branch("alice/1-part-one");
    let merge = Merge {
        pr: &pr("acme/api#12"),
        repo: &repo("api"),
        base: &branch("main"),
        merged_at: at(10),
        files: &files,
        head: Some(&head),
        author: Some(ItemId(1)),
    };
    assert!(!merge.merged(&footprint_two));
    assert_eq!(
        scene.main_moved(&merge, &entries).len(),
        1,
        "the item's next branch, based before the merge, is behind it"
    );
    let without_head = Merge {
        head: None,
        ..merge
    };
    assert!(
        without_head.merged(&footprint_two),
        "without the head, the claim's footprint stands for the merged branch"
    );
    assert!(scene.main_moved(&without_head, &entries).is_empty());
    let named = Merge {
        head: Some(&footprint_two.branch),
        ..merge
    };
    assert!(named.merged(&footprint_two));
}

#[test]
fn a_footprint_from_a_base_no_older_than_the_merge_has_caught_up() {
    let moved = Overlap::new(
        OverlapKind::MainMoved,
        [ItemId(2)].into(),
        Some(MergeMark {
            pr: pr("acme/api#12"),
            at: at(10),
        }),
        qualified_set(&["api:a.rs"]),
    )
    .unwrap();
    let (old, _) = footprint(2, "api", &["a.rs"], 5);
    let (new, _) = footprint(2, "api", &["a.rs"], 10);
    let (elsewhere, _) = footprint(2, "web", &["a.rs"], 30);
    let (other_item, _) = footprint(3, "api", &["a.rs"], 30);
    assert!(!moved.caught_up_by(&old));
    assert!(moved.caught_up_by(&new));
    assert!(!moved.caught_up_by(&elsewhere), "another repository");
    assert!(!moved.caught_up_by(&other_item));
    assert!(moved.in_repo(&repo("api")) && !moved.in_repo(&repo("web")));
}

#[test]
fn an_overlap_is_announced_once_for_as_long_as_it_lasts_and_anew_when_it_comes_back() {
    let pair = |paths: &[&str]| {
        Overlap::new(
            OverlapKind::SameFile,
            [ItemId(1), ItemId(2)].into(),
            None,
            qualified_set(paths),
        )
        .unwrap()
    };
    let elsewhere = Overlap::new(
        OverlapKind::SameFile,
        [ItemId(3), ItemId(4)].into(),
        None,
        qualified_set(&["api:x.rs"]),
    )
    .unwrap();
    let mine = |overlap: &Overlap| overlap.involves(ItemId(1));
    let live = |_: &Overlap| true;
    let first = reconcile(
        std::slice::from_ref(&elsewhere),
        &[pair(&["api:a.rs"])].into(),
        mine,
        live,
    );
    assert_eq!(first.found, [pair(&["api:a.rs"])]);
    assert!(first.moved.is_empty() && first.ended.is_empty());
    let stored = [elsewhere.clone(), pair(&["api:a.rs"])];
    let again = reconcile(&stored, &[pair(&["api:a.rs"])].into(), mine, live);
    assert!(again.is_empty(), "a repeat announces nothing");
    let grown = reconcile(
        &stored,
        &[pair(&["api:a.rs", "api:b.rs"])].into(),
        mine,
        live,
    );
    assert!(grown.found.is_empty(), "more paths do not make it new");
    assert_eq!(grown.moved, [pair(&["api:a.rs", "api:b.rs"])]);
    let gone = reconcile(&stored, &BTreeSet::new(), mine, live);
    assert_eq!(
        gone.ended,
        [pair(&["api:a.rs"])],
        "the other pair is not covered"
    );
    let back = reconcile(
        std::slice::from_ref(&elsewhere),
        &[pair(&["api:a.rs"])].into(),
        mine,
        live,
    );
    assert_eq!(
        back.found.len(),
        1,
        "an overlap that ended is new when it returns"
    );
    let stale = |_: &Overlap| false;
    let unannounced = reconcile(&[elsewhere], &[pair(&["api:a.rs"])].into(), mine, stale);
    assert!(
        unannounced.is_empty(),
        "one whose claim is stale is not found until it is live again"
    );
    let kept = reconcile(&stored, &[pair(&["api:a.rs"])].into(), mine, stale);
    assert!(
        kept.is_empty(),
        "a stored one stays while its claim is stale"
    );
}

#[test]
fn an_overlap_fits_its_kind_and_has_one_key_text() {
    let two: BTreeSet<ItemId> = [ItemId(3), ItemId(7)].into();
    let one: BTreeSet<ItemId> = [ItemId(3)].into();
    let paths = qualified_set(&["api:a.rs"]);
    let merged = Some(MergeMark {
        pr: pr("acme/api#12"),
        at: at(10),
    });
    assert!(Overlap::new(OverlapKind::SameFile, two.clone(), None, paths.clone()).is_ok());
    assert!(Overlap::new(OverlapKind::Contract, two.clone(), None, paths.clone()).is_ok());
    assert!(
        Overlap::new(
            OverlapKind::MainMoved,
            one.clone(),
            merged.clone(),
            paths.clone()
        )
        .is_ok()
    );
    for (kind, items, pr, paths) in [
        (OverlapKind::SameFile, one.clone(), None, paths.clone()),
        (
            OverlapKind::SameFile,
            two.clone(),
            merged.clone(),
            paths.clone(),
        ),
        (OverlapKind::Contract, two.clone(), None, BTreeSet::new()),
        (OverlapKind::MainMoved, one.clone(), None, paths.clone()),
        (
            OverlapKind::MainMoved,
            two.clone(),
            merged.clone(),
            paths.clone(),
        ),
    ] {
        assert_eq!(
            Overlap::new(kind, items, pr, paths),
            Err(OverlapError::Shape)
        );
    }
    let pair = Overlap::new(OverlapKind::SameFile, two, None, paths.clone()).unwrap();
    assert_eq!(pair.key().to_string(), "same_file 3 7");
    let moved = Overlap::new(OverlapKind::MainMoved, one, merged, paths).unwrap();
    assert_eq!(moved.key().to_string(), "main_moved 3 acme/api#12");
    for kind in OverlapKind::ALL {
        assert_eq!(OverlapKind::parse(kind.code()), Some(kind));
    }
    assert_eq!(OverlapKind::parse("other"), None);
}

#[test]
fn hundreds_of_claims_with_thousands_of_paths_each_are_derived_from_shared_paths_and_groups() {
    let settings = radar_settings();
    let claims: Vec<Claim> = (1..=300)
        .map(|item| claim(item, MemberId(item), &[], 0))
        .collect();
    let mut footprints = Vec::new();
    let mut entries = Vec::new();
    for item in 1..=300_u64 {
        let paths: Vec<String> = (0..3000)
            .map(|n| format!("packages/search/src/f{item}_{n}.php"))
            .chain([format!("packages/billing/src/Api/V{item}.php")])
            .collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let (made, made_entries) = footprint(item, "api", &refs, 0);
        footprints.push(made);
        entries.extend(made_entries);
    }
    let scene = RadarScene {
        claims: &claims,
        footprints: &footprints,
        settings: &settings,
        now: at(1),
    };
    let contracts = scene.contracts();
    assert_eq!(
        contracts.len(),
        300 * 299 / 2,
        "every claim changes billing's contract and works in search, which depends on it"
    );
    let mine: BTreeSet<RepoPath> = entries
        .iter()
        .filter(|entry| entry.item == ItemId(1))
        .map(|entry| entry.path.clone())
        .collect();
    assert!(scene.same_file(ItemId(1), &mine, &entries).is_empty());
}
