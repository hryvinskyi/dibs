//! Worktree pools: taking, taking again, a busy answer, releasing by worktree and by holder,
//! beats, the end of a session, an admin's forced release and the status, each answer parsed into
//! its type; and a server older than the routes, whose `404` is told apart from a refusal.

use dibs_client::ClientError;
use dibs_client::api::{
    BeatWorktreesRequest, ForceReleaseWorktreeRequest, ReleaseWorktreeRequest, TakeWorktreeRequest,
    WorktreeBeat, WorktreeOutcome, WorktreeRelease,
};
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::report::MachineId;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{AgentKey, HolderKind, PoolId, WorktreeName};

use crate::support::{World, client, run, session};

fn machine() -> MachineId {
    MachineId::parse("0123456789abcdef0123456789abcdef").unwrap()
}

fn pool() -> PoolId {
    PoolId::parse("c0ffee42").unwrap()
}

fn name(text: &str) -> WorktreeName {
    WorktreeName::parse(text).unwrap()
}

fn agent(text: &str) -> AgentKey {
    AgentKey::parse(text).unwrap()
}

fn take(session: SessionId, agent: Option<AgentKey>) -> TakeWorktreeRequest {
    TakeWorktreeRequest {
        session_id: session,
        machine: machine(),
        pool: pool(),
        names: vec![name("worktree1"), name("worktree2")],
        agent,
        purpose: Purpose::parse("fix the invoice totals").unwrap(),
        item: None,
    }
}

fn release(session: SessionId, release: WorktreeRelease) -> ReleaseWorktreeRequest {
    ReleaseWorktreeRequest {
        session_id: session,
        machine: machine(),
        pool: pool(),
        release,
    }
}

#[test]
fn worktrees_are_taken_again_answered_busy_released_and_listed() {
    let world = World::new();
    let alice = world.client(&world.alice);
    run(async {
        let cli = session(&alice, "laptop/cli").await.session_id;
        let first = alice
            .take_worktree(&take(cli, Some(agent("a1"))))
            .await
            .unwrap();
        assert_eq!(first.outcome, WorktreeOutcome::Took);
        assert_eq!(first.worktree, Some(name("worktree1")));
        assert!(!first.again);
        let again = alice
            .take_worktree(&take(cli, Some(agent("a1"))))
            .await
            .unwrap();
        assert_eq!(
            (again.worktree.clone(), again.again),
            (Some(name("worktree1")), true)
        );
        let second = alice
            .take_worktree(&take(cli, Some(agent("a2"))))
            .await
            .unwrap();
        assert_eq!(second.worktree, Some(name("worktree2")));
        let busy = alice.take_worktree(&take(cli, None)).await.unwrap();
        assert_eq!(busy.outcome, WorktreeOutcome::Busy);
        assert_eq!(busy.worktree, None);
        let held: Vec<_> = busy
            .held
            .iter()
            .map(|hold| (hold.worktree.as_str(), hold.agent.clone(), hold.holder))
            .collect();
        assert_eq!(
            held,
            [
                ("worktree1", Some(agent("a1")), HolderKind::Agent),
                ("worktree2", Some(agent("a2")), HolderKind::Agent)
            ]
        );
        assert_eq!(busy.held[0].session.as_str(), "laptop/cli");

        let listed = alice.worktrees(Some(&machine())).await.unwrap();
        assert_eq!(listed.pools.len(), 1);
        assert_eq!(listed.pools[0].pool, pool());
        assert_eq!(listed.pools[0].holds.len(), 2);
        assert_eq!(listed.board.len(), 2);
        assert_eq!(listed.board[0].member.as_str(), "alice");
        let elsewhere = alice
            .worktrees(Some(&MachineId::from_bits(7)))
            .await
            .unwrap();
        assert!(elsewhere.pools.is_empty());
        assert_eq!(elsewhere.board.len(), 2, "the board is the whole project's");
        let status = alice.status().await.unwrap();
        assert_eq!(status.worktrees.len(), 1);

        let by_agent = alice
            .release_worktree(&release(
                cli,
                WorktreeRelease::Holder {
                    holder_session: cli,
                    agent: Some(agent("a2")),
                },
            ))
            .await
            .unwrap();
        assert_eq!(by_agent.released.worktree, name("worktree2"));
        let by_name = alice
            .release_worktree(&release(
                cli,
                WorktreeRelease::Worktree {
                    worktree: name("worktree1"),
                },
            ))
            .await
            .unwrap();
        assert_eq!(by_name.released.agent, Some(agent("a1")));
        let nothing = alice
            .release_worktree(&release(
                cli,
                WorktreeRelease::Worktree {
                    worktree: name("worktree1"),
                },
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(&nothing, ClientError::Conflict { code, .. } if code == "worktree_not_held"),
            "{nothing:?}"
        );
    });
}

#[test]
fn beats_keep_holds_a_session_end_releases_them_and_an_admin_forces_one() {
    let world = World::new();
    let alice = world.client(&world.alice);
    let bob = world.client(&world.bob);
    run(async {
        let a = session(&alice, "laptop/cli").await.session_id;
        let b = session(&bob, "desktop/cli").await.session_id;
        alice
            .take_worktree(&take(a, Some(agent("a1"))))
            .await
            .unwrap();
        let beaten = alice
            .beat_worktrees(&BeatWorktreesRequest {
                session_id: a,
                machine: machine(),
                pool: pool(),
                beats: vec![
                    WorktreeBeat {
                        worktree: name("worktree1"),
                        holder_session: a,
                        agent: Some(agent("a1")),
                        seen_at: dibs_test_server::START,
                    },
                    WorktreeBeat {
                        worktree: name("worktree2"),
                        holder_session: a,
                        agent: Some(agent("a1")),
                        seen_at: dibs_test_server::START,
                    },
                ],
            })
            .await
            .unwrap();
        assert_eq!(beaten.kept.len(), 1);
        assert_eq!(beaten.kept[0].worktree, name("worktree1"));
        assert_eq!(beaten.unknown.len(), 1);
        assert_eq!(beaten.unknown[0].worktree, name("worktree2"));

        let ended = alice.end_session(a).await.unwrap();
        assert_eq!(ended.released.len(), 1);
        assert_eq!(alice.end_session(a).await.unwrap().released, []);

        let mut item_take = take(b, None);
        item_take.item = Some(ItemId(424_242));
        let unknown_item = bob.take_worktree(&item_take).await.unwrap_err();
        assert!(
            matches!(unknown_item, ClientError::Refused { status: 404, .. }),
            "an unknown item is a refusal, not a sign of the server's age: {unknown_item:?}"
        );

        bob.take_worktree(&take(b, None)).await.unwrap();
        let forced = alice
            .force_release_worktree(&ForceReleaseWorktreeRequest {
                session_id: a,
                member: "bob".parse().unwrap(),
                host: "laptop".parse().unwrap(),
                worktree: name("worktree1"),
                reason: "the release needs the machine back".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(forced.released.len(), 1);
        assert_eq!(forced.released[0].member.as_str(), "bob");
        assert!(forced.thread_id.is_some());
    });
}

#[test]
fn a_server_without_the_routes_is_older_than_this_client() {
    let url = dibs_test_server::fixed_answer(
        404,
        "application/json",
        r#"{"error":{"code":"not_found","message":"there is no such route"}}"#,
    )
    .unwrap();
    let old = client(&url, "dibs_token");
    run(async {
        let session = SessionId(3);
        for error in [
            old.take_worktree(&take(session, None)).await.unwrap_err(),
            old.worktrees(None).await.unwrap_err(),
            old.end_session(session).await.unwrap_err(),
            old.release_worktree(&release(
                session,
                WorktreeRelease::Worktree {
                    worktree: name("worktree1"),
                },
            ))
            .await
            .unwrap_err(),
            old.beat_worktrees(&BeatWorktreesRequest {
                session_id: session,
                machine: machine(),
                pool: pool(),
                beats: Vec::new(),
            })
            .await
            .unwrap_err(),
        ] {
            assert_eq!(
                error,
                ClientError::ServerTooOld {
                    server: url.clone()
                }
            );
            assert!(error.is_unavailable());
            assert!(
                error.to_string().contains("older than this client"),
                "{error}"
            );
        }
    });
}
