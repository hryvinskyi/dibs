//! Who the caller is, items, claims, status, edit checks and grants.

use time::Duration;

use dibs_client::ClientError;
use dibs_client::api::{
    ClaimOutcome, ClaimRequest, EditCheckRequest, GrantRequest, ItemFilter, NewItem,
    TakeOverRequest, Verdict,
};
use dibs_client::verdicts::edit_verdicts;
use dibs_domain::ids::ItemId;
use dibs_domain::item::{ExternalRef, ItemKey, ItemStatus};
use dibs_domain::member::Role;
use dibs_domain::paths::RelPath;
use dibs_domain::repo::RepoId;

use crate::support::{World, client, run, session};

fn new_item(reference: &str, title: &str) -> NewItem {
    NewItem {
        external_ref: Some(ExternalRef::parse(reference).unwrap()),
        title: title.parse().unwrap(),
    }
}

fn paths(texts: &[&str]) -> Vec<RelPath> {
    texts
        .iter()
        .map(|text| RelPath::parse(text).unwrap())
        .collect()
}

#[test]
fn me_names_the_caller_their_project_and_settings_that_parse() {
    let world = World::new();
    let me = run(world.client(&world.bob).me()).unwrap();
    assert_eq!(me.member.handle.as_str(), "bob");
    assert_eq!(me.member.role, Role::Member);
    assert_eq!(me.project.slug.as_str(), "acme");
    let ids: Vec<&str> = me.repos.iter().map(|repo| repo.id.as_str()).collect();
    assert_eq!(ids, ["api", "web"]);
    assert_eq!(
        me.repos[0].remote.as_ref().unwrap().as_str(),
        "github.com/acme/api"
    );
    let settings = me.project_settings().unwrap();
    assert_eq!(settings.stale_after(), Duration::hours(1));
    let members = run(world.client(&world.bob).members()).unwrap();
    let handles: Vec<&str> = members.iter().map(|m| m.handle.as_str()).collect();
    assert_eq!(handles, ["alice", "bob"]);
}

#[test]
fn an_item_is_created_once_found_by_its_reference_and_claimed() {
    let world = World::new();
    let alice = world.client(&world.alice);
    run(async {
        let created = alice
            .create_item(&new_item("gh:acme/api#1", "Fix billing"))
            .await
            .unwrap();
        assert!(created.created);
        assert_eq!(created.item.repo.as_ref().map(RepoId::as_str), Some("api"));
        let again = alice
            .create_item(&new_item("gh:acme/api#1", "Fix billing"))
            .await
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.item.id, created.item.id);

        let key = ItemKey::parse("gh:acme/api#1").unwrap();
        assert_eq!(alice.resolve(&key).await.unwrap(), created.item.id);
        assert_eq!(
            alice.resolve(&ItemKey::Local(ItemId(77))).await.unwrap(),
            ItemId(77)
        );
        let missing = alice
            .resolve(&ItemKey::parse("gh:acme/api#99").unwrap())
            .await
            .unwrap_err();
        assert_eq!(missing.code(), "not_found");
        assert!(missing.to_string().contains("dibs new gh:acme/api#99"));

        let laptop = session(&alice, "laptop/cli").await;
        let claimed = alice
            .claim(
                created.item.id,
                &ClaimRequest {
                    session_id: laptop.session_id,
                    areas: vec!["billing".to_owned()],
                },
            )
            .await
            .unwrap();
        assert_eq!(claimed.outcome, ClaimOutcome::New);
        assert_eq!(claimed.claim.holder.as_str(), "alice");
        let areas: Vec<String> = claimed
            .claim
            .areas
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(areas, ["api:billing"]);

        let mine = alice
            .items(&ItemFilter {
                holder: Some("alice".parse().unwrap()),
                ..ItemFilter::default()
            })
            .await
            .unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].status, ItemStatus::Claimed);
        let open = alice
            .items(&ItemFilter {
                status: Some(ItemStatus::Open),
                ..ItemFilter::default()
            })
            .await
            .unwrap();
        assert!(open.is_empty());
        let detail = alice.item(created.item.id).await.unwrap();
        assert_eq!(detail.claim.unwrap().session_id, laptop.session_id);
    });
}

#[test]
fn another_member_in_the_way_is_a_conflict_naming_them() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let first = alice
            .create_item(&new_item("gh:acme/api#1", "Billing"))
            .await
            .unwrap()
            .item
            .id;
        let second = bob
            .create_item(&new_item("gh:acme/api#2", "Also billing"))
            .await
            .unwrap()
            .item
            .id;
        let alice_session = session(&alice, "laptop/cli").await.session_id;
        let bob_session = session(&bob, "desktop/cli").await.session_id;
        alice
            .claim(
                first,
                &ClaimRequest {
                    session_id: alice_session,
                    areas: vec!["api:billing".to_owned()],
                },
            )
            .await
            .unwrap();

        let held = bob
            .claim(
                first,
                &ClaimRequest {
                    session_id: bob_session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap_err();
        let ClientError::Conflict { code, details, .. } = &held else {
            panic!("{held:?}");
        };
        assert_eq!(code, "held");
        assert_eq!(details.holder.as_ref().unwrap().as_str(), "alice");
        assert!(details.last_seen.is_some());

        let area = bob
            .claim(
                second,
                &ClaimRequest {
                    session_id: bob_session,
                    areas: vec!["billing".to_owned()],
                },
            )
            .await
            .unwrap_err();
        assert_eq!(area.code(), "area_held");
        let details = area.details().unwrap();
        assert_eq!(details.item, Some(first));
        assert_eq!(details.area.as_ref().unwrap().to_string(), "api:billing");

        let unknown = bob
            .claim(
                second,
                &ClaimRequest {
                    session_id: bob_session,
                    areas: vec!["nowhere".to_owned()],
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(unknown, ClientError::Refused { status: 422, .. }));
        assert_eq!(unknown.code(), "unknown_area");

        let status = bob.status().await.unwrap();
        assert_eq!(status.live.len(), 1);
        assert_eq!(status.holds[0].holder.as_str(), "alice");
        assert_eq!(status.holds[0].area.to_string(), "api:billing");
    });
}

#[test]
fn edit_checks_answer_per_path_and_ungoverned_paths_need_no_server() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let item = alice
            .create_item(&new_item("gh:acme/api#1", "Billing"))
            .await
            .unwrap()
            .item
            .id;
        let alice_session = session(&alice, "laptop/cli").await.session_id;
        alice
            .claim(
                item,
                &ClaimRequest {
                    session_id: alice_session,
                    areas: vec!["billing".to_owned()],
                },
            )
            .await
            .unwrap();
        let bob_session = session(&bob, "desktop/cli").await.session_id;
        let request = EditCheckRequest {
            session_id: bob_session,
            repo: RepoId::parse("api").unwrap(),
            paths: paths(&["README.md", "src/billing/a.rs", "src/search/b.rs"]),
            reserve: false,
        };
        let verdicts = bob.edit_check(&request).await.unwrap().verdicts;
        assert_eq!(verdicts.len(), 3);
        assert_eq!(verdicts[0].verdict, Verdict::Allowed);
        assert!(matches!(
            &verdicts[1].verdict,
            Verdict::HeldBy { holder: Some(holder), item: held, .. }
                if holder.as_str() == "alice" && *held == item
        ));
        assert_eq!(verdicts[2].verdict, Verdict::NeedsClaim);

        let settings = bob.me().await.unwrap().project_settings().unwrap();
        let merged = edit_verdicts(&bob, &settings, &request).await.unwrap();
        assert_eq!(merged, verdicts);

        let offline = client(&dibs_test_server::unreachable().unwrap(), &world.bob.token);
        let ungoverned = EditCheckRequest {
            paths: paths(&["README.md", "docs/guide.md"]),
            ..request.clone()
        };
        let local = edit_verdicts(&offline, &settings, &ungoverned)
            .await
            .unwrap();
        assert!(local.iter().all(|v| v.verdict == Verdict::Allowed));
        assert_eq!(local[1].path.as_str(), "docs/guide.md");
        let governed = edit_verdicts(&offline, &settings, &request)
            .await
            .unwrap_err();
        assert!(governed.is_unavailable());
    });
}

#[test]
fn a_holder_grants_another_member_edits_for_their_item() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let alice_item = alice
            .create_item(&new_item("gh:acme/api#1", "Billing"))
            .await
            .unwrap()
            .item
            .id;
        let bob_item = bob
            .create_item(&new_item("gh:acme/api#2", "Search"))
            .await
            .unwrap()
            .item
            .id;
        let alice_session = session(&alice, "laptop/cli").await.session_id;
        let bob_session = session(&bob, "desktop/cli").await.session_id;
        for (who, item, session, area) in [
            (&alice, alice_item, alice_session, "billing"),
            (&bob, bob_item, bob_session, "search"),
        ] {
            who.claim(
                item,
                &ClaimRequest {
                    session_id: session,
                    areas: vec![area.to_owned()],
                },
            )
            .await
            .unwrap();
        }
        let granted = alice
            .grant(&GrantRequest {
                item: bob_item,
                area: "billing".to_owned(),
                to: "bob".parse().unwrap(),
            })
            .await
            .unwrap();
        assert!(granted.created);
        assert_eq!(granted.grant.from.as_str(), "alice");
        let check = bob
            .edit_check(&EditCheckRequest {
                session_id: bob_session,
                repo: RepoId::parse("api").unwrap(),
                paths: paths(&["src/billing/a.rs"]),
                reserve: false,
            })
            .await
            .unwrap();
        assert!(matches!(
            &check.verdicts[0].verdict,
            Verdict::Granted { by: Some(by), .. } if by.as_str() == "alice"
        ));
        let revoked = alice.revoke_grant(granted.grant.id).await.unwrap();
        assert_eq!(revoked.grant.id, granted.grant.id);
        let self_grant = bob
            .grant(&GrantRequest {
                item: bob_item,
                area: "search".to_owned(),
                to: "bob".parse().unwrap(),
            })
            .await
            .unwrap_err();
        assert_eq!(self_grant.code(), "self_grant");
    });
}

#[test]
fn a_stale_claim_is_taken_over_released_and_done() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    let item = run(async {
        let item = alice
            .create_item(&new_item("gh:acme/web#3", "Checkout"))
            .await
            .unwrap()
            .item
            .id;
        let alice_session = session(&alice, "laptop/cli").await.session_id;
        alice
            .claim(
                item,
                &ClaimRequest {
                    session_id: alice_session,
                    areas: vec!["checkout".to_owned()],
                },
            )
            .await
            .unwrap();
        let bob_session = session(&bob, "desktop/cli").await.session_id;
        let not_stale = bob
            .take_over(
                item,
                &TakeOverRequest {
                    session_id: bob_session,
                    reason: "picking it up".parse().unwrap(),
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(not_stale, ClientError::Conflict { .. }));
        assert_eq!(not_stale.code(), "not_stale");
        item
    });
    world.server.advance(Duration::hours(2));
    run(async {
        let bob_session = session(&bob, "desktop/cli").await.session_id;
        let taken = bob
            .take_over(
                item,
                &TakeOverRequest {
                    session_id: bob_session,
                    reason: "alice is away".parse().unwrap(),
                    areas: vec!["checkout".to_owned()],
                },
            )
            .await
            .unwrap();
        assert_eq!(taken.previous_holder.as_str(), "alice");
        assert_eq!(taken.claim.holder.as_str(), "bob");
        let not_holder = alice.release(item).await.unwrap_err();
        assert!(matches!(
            not_holder,
            ClientError::Refused { status: 403, .. }
        ));
        let released = bob.release(item).await.unwrap();
        assert_eq!(released.claim.item, item);
        let no_claim = bob.done(item).await.unwrap_err();
        assert_eq!(no_claim.code(), "no_claim");
        bob.claim(
            item,
            &ClaimRequest {
                session_id: bob_session,
                areas: Vec::new(),
            },
        )
        .await
        .unwrap();
        bob.done(item).await.unwrap();
        assert_eq!(bob.item(item).await.unwrap().item.status, ItemStatus::Done);
        let heartbeat = bob.heartbeat(bob_session).await.unwrap();
        assert!(heartbeat.refreshed.is_empty());
    });
}

#[test]
fn the_ready_list_and_the_dependencies_are_read_and_written() {
    let world = World::new();
    let bob = world.client(&world.bob);
    run(async {
        let waiting = bob
            .create_item(&new_item("gh:acme/api#3", "Waits"))
            .await
            .unwrap()
            .item
            .id;
        let blocker = bob
            .create_item(&new_item("gh:acme/api#4", "Blocks"))
            .await
            .unwrap()
            .item
            .id;
        let added = bob.add_dependency(waiting, blocker).await.unwrap();
        assert!(added.changed);
        assert_eq!(added.blocked_by.id, blocker);
        let cycle = bob.add_dependency(blocker, waiting).await.unwrap_err();
        assert_eq!(cycle.code(), "dependency_cycle");
        assert!(matches!(cycle, ClientError::Conflict { .. }));
        let ready = bob.ready().await.unwrap();
        let ready_ids: Vec<ItemId> = ready.ready.iter().map(|entry| entry.item.id).collect();
        assert_eq!(ready_ids, [blocker]);
        assert_eq!(
            ready.ready[0].link.as_deref(),
            Some("https://github.com/acme/api/issues/4")
        );
        let session = session(&bob, "laptop/ready").await.session_id;
        let claimed = bob
            .claim(
                waiting,
                &ClaimRequest {
                    session_id: session,
                    areas: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(claimed.blocked_by[0].label(), "gh:acme/api#4");
        let status = bob.status().await.unwrap();
        assert_eq!(status.ready.ready.len(), 1);
        let removed = bob.remove_dependency(waiting, blocker).await.unwrap();
        assert!(removed.changed);
        assert!(
            !bob.remove_dependency(waiting, blocker)
                .await
                .unwrap()
                .changed
        );
    });
}
