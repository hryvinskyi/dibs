//! The worktree use cases over the in-memory store: each ends the silent holds before it decides,
//! records one event per change, keeps a member's pools to the member, and is refused to whoever
//! may not do it.

use time::{Duration, OffsetDateTime};

use dibs_domain::event::{EventKind, WorktreeReleaseCause, WorktreeReleased, WorktreeTaken};
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::message::{Author, Body};
use dibs_domain::report::MachineId;
use dibs_domain::session::HostName;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{AgentKey, Holder, HolderKind, PoolId, WorktreeHolder, WorktreeName};

use super::*;
use crate::AppError;
use crate::actor::Actor;
use crate::use_cases::testing::{Env, Person, session};
use crate::use_cases::{ReadThread, RemoveMember, Status};

const LAPTOP: &str = "0123456789abcdef0123456789abcdef";
const DESKTOP: &str = "fedcba9876543210fedcba9876543210";
const CHECKOUT: &str = "a1b2c3";
const NAMES: [&str; 3] = ["worktree1", "worktree2", "worktree3"];

fn machine(id: &str) -> MachineId {
    MachineId::parse(id).unwrap()
}

fn pool_id(id: &str) -> PoolId {
    PoolId::parse(id).unwrap()
}

fn name(text: &str) -> WorktreeName {
    WorktreeName::parse(text).unwrap()
}

fn agent(key: &str) -> AgentKey {
    AgentKey::parse(key).unwrap()
}

/// A take in a pool: which machine and checkout, which worktrees, and for whom.
#[derive(Clone)]
struct Ask<'a> {
    actor: &'a Actor,
    session: SessionId,
    agent: Option<&'a str>,
    machine: &'a str,
    pool: &'a str,
    names: &'a [&'a str],
    item: Option<ItemId>,
}

impl<'a> Ask<'a> {
    fn of(who: &'a Person) -> Self {
        Self {
            actor: &who.actor,
            session: who.session,
            agent: None,
            machine: LAPTOP,
            pool: CHECKOUT,
            names: &NAMES,
            item: None,
        }
    }

    fn agent(self, key: &'a str) -> Self {
        Self {
            agent: Some(key),
            ..self
        }
    }

    fn session(self, session: SessionId) -> Self {
        Self { session, ..self }
    }

    async fn take(&self, env: &Env) -> Result<TookWorktree, AppError> {
        TakeWorktree::new(&env.deps)
            .execute(
                self.actor,
                TakeWorktreeInput {
                    session: self.session,
                    machine: machine(self.machine),
                    pool: pool_id(self.pool),
                    names: self.names.iter().map(|n| name(n)).collect(),
                    agent: self.agent.map(agent),
                    purpose: Purpose::parse("fix the search index").unwrap(),
                    item: self.item,
                },
            )
            .await
    }

    /// Takes and answers the worktree and whether it was held already.
    async fn took(&self, env: &Env) -> (String, bool) {
        match self.take(env).await.unwrap() {
            TookWorktree::Took { worktree, again } => (worktree.to_string(), again),
            TookWorktree::Busy { held } => panic!("busy: {held:?}"),
        }
    }

    async fn release(&self, env: &Env, release: WorktreeRelease) -> Result<HeldWorktree, AppError> {
        ReleaseWorktree::new(&env.deps)
            .execute(
                self.actor,
                ReleaseWorktreeInput {
                    session: self.session,
                    machine: machine(self.machine),
                    pool: pool_id(self.pool),
                    release,
                },
            )
            .await
    }

    async fn beat(
        &self,
        env: &Env,
        beats: Vec<WorktreeBeatInput>,
    ) -> Result<dibs_domain::worktree::Beaten, AppError> {
        BeatWorktrees::new(&env.deps)
            .execute(
                self.actor,
                BeatWorktreesInput {
                    session: self.session,
                    machine: machine(self.machine),
                    pool: pool_id(self.pool),
                    beats,
                },
            )
            .await
    }
}

fn beat(
    worktree: &str,
    session: SessionId,
    key: Option<&str>,
    seen_at: OffsetDateTime,
) -> WorktreeBeatInput {
    WorktreeBeatInput {
        worktree: name(worktree),
        session,
        agent: key.map(agent),
        seen_at,
    }
}

async fn mine(env: &Env, who: &Actor) -> Vec<(String, String)> {
    Worktrees::new(&env.deps)
        .execute(who, None)
        .await
        .unwrap()
        .mine
        .iter()
        .flat_map(|pool| pool.holds.iter())
        .map(|held| {
            let holder = match held.hold.holder.agent() {
                Some(agent) => format!("{}/{agent}", held.session),
                None => held.session.to_string(),
            };
            (held.hold.worktree.to_string(), holder)
        })
        .collect()
}

fn pair(worktree: &str, holder: Holder) -> WorktreeHolder {
    WorktreeHolder {
        worktree: name(worktree),
        holder,
    }
}

#[tokio::test]
async fn a_take_gets_the_first_free_worktree_the_same_one_again_and_busy_when_none_is_free() {
    let env = Env::new().await;
    let before = env.events().await.len();
    let alice = Ask::of(&env.alice);
    assert_eq!(alice.took(&env).await, ("worktree1".to_owned(), false));
    assert_eq!(
        alice.took(&env).await,
        ("worktree1".to_owned(), true),
        "a holder that asks again gets its own worktree"
    );
    let two = Ask {
        names: &NAMES[..1],
        ..alice.clone().agent("agent-7")
    };
    let TookWorktree::Busy { held } = two.take(&env).await.unwrap() else {
        panic!("worktree1 is held");
    };
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].hold.worktree, name("worktree1"));
    assert_eq!(held[0].hold.holder, Holder::Session(env.alice.session));
    assert_eq!(held[0].hold.host.as_str(), "laptop");
    assert_eq!(held[0].session.as_str(), "laptop/4f2a91c0");
    let kinds = env.kinds_after(before).await;
    assert_eq!(
        kinds,
        [EventKind::WorktreeTaken(WorktreeTaken {
            member: env.alice.actor.member,
            host: HostName::parse("laptop").unwrap(),
            worktree: name("worktree1"),
            holder: HolderKind::Session,
            purpose: Purpose::parse("fix the search index").unwrap(),
        })],
        "only the first take changed the pool"
    );
}

#[tokio::test]
async fn sessions_and_agents_are_each_a_holder_of_their_own() {
    let env = Env::new().await;
    let other = session(&env.deps, &env.alice.actor, "laptop", "laptop/second").await;
    let alice = Ask::of(&env.alice);
    assert_eq!(alice.took(&env).await.0, "worktree1");
    assert_eq!(
        alice.clone().session(other).took(&env).await,
        ("worktree2".to_owned(), false),
        "two sessions of one member get two worktrees"
    );
    assert_eq!(
        alice.clone().agent("agent-1").took(&env).await,
        ("worktree3".to_owned(), false),
        "an agent does not share its session's worktree"
    );
    let TookWorktree::Busy { held } = alice.clone().agent("agent-2").take(&env).await.unwrap()
    else {
        panic!("every worktree is held");
    };
    assert_eq!(
        held.iter()
            .map(|h| h.hold.worktree.to_string())
            .collect::<Vec<_>>(),
        NAMES,
        "the busy answer names every holder in the order asked"
    );
    assert_eq!(
        alice.clone().agent("agent-1").took(&env).await,
        ("worktree3".to_owned(), true)
    );
    assert_eq!(
        mine(&env, &env.alice.actor).await,
        [
            ("worktree1".to_owned(), "laptop/4f2a91c0".to_owned()),
            ("worktree2".to_owned(), "laptop/second".to_owned()),
            ("worktree3".to_owned(), "laptop/4f2a91c0/agent-1".to_owned()),
        ]
    );
}

#[tokio::test]
async fn two_agents_of_one_session_hold_two_worktrees() {
    let env = Env::new().await;
    let alice = Ask::of(&env.alice);
    assert_eq!(
        alice.clone().agent("agent-1").took(&env).await.0,
        "worktree1"
    );
    assert_eq!(
        alice.clone().agent("agent-2").took(&env).await.0,
        "worktree2"
    );
    assert_eq!(
        alice.took(&env).await.0,
        "worktree3",
        "the session itself holds apart from both"
    );
}

#[tokio::test]
async fn two_pools_of_one_machine_and_two_machines_stay_apart() {
    let env = Env::new().await;
    let alice = Ask::of(&env.alice);
    assert_eq!(alice.took(&env).await.0, "worktree1");
    let other_checkout = Ask {
        pool: "d4e5f6",
        ..alice.clone().agent("agent-1")
    };
    let other_machine = Ask {
        machine: DESKTOP,
        ..alice.clone().agent("agent-2")
    };
    assert_eq!(other_checkout.took(&env).await.0, "worktree1");
    assert_eq!(other_machine.took(&env).await.0, "worktree1");
    let view = Worktrees::new(&env.deps)
        .execute(&env.alice.actor, Some(&machine(LAPTOP)))
        .await
        .unwrap();
    let pools: Vec<(String, usize)> = view
        .mine
        .iter()
        .map(|pool| (pool.key.pool.to_string(), pool.holds.len()))
        .collect();
    assert_eq!(
        pools,
        [("a1b2c3".to_owned(), 1), ("d4e5f6".to_owned(), 1)],
        "the machine named filters the caller's pools"
    );
    assert_eq!(view.board.len(), 3, "the board shows every hold");
    let released = other_checkout
        .release(&env, WorktreeRelease::Name(name("worktree1")))
        .await
        .unwrap();
    assert_eq!(released.hold.holder.agent(), Some(&agent("agent-1")));
    assert_eq!(
        mine(&env, &env.alice.actor).await.len(),
        2,
        "a release by name ends the hold of that pool only"
    );
}

#[tokio::test]
async fn a_member_releases_by_name_or_by_holder_any_of_their_own_holds() {
    let env = Env::new().await;
    let other = session(&env.deps, &env.alice.actor, "laptop", "laptop/second").await;
    let alice = Ask::of(&env.alice);
    alice.took(&env).await;
    alice.clone().agent("agent-1").took(&env).await;
    alice
        .clone()
        .session(other)
        .agent("agent-9")
        .took(&env)
        .await;
    let before = env.events().await.len();
    let by_agent = alice
        .release(
            &env,
            WorktreeRelease::Holder {
                session: env.alice.session,
                agent: Some(agent("agent-1")),
            },
        )
        .await
        .unwrap();
    assert_eq!(by_agent.hold.worktree, name("worktree2"));
    let by_name = alice
        .release(&env, WorktreeRelease::Name(name("worktree3")))
        .await
        .unwrap();
    assert_eq!(
        by_name.hold.holder,
        Holder::Agent {
            session: other,
            agent: agent("agent-9"),
        },
        "a hold of another of the member's sessions is theirs to release"
    );
    assert_eq!(by_name.session.as_str(), "laptop/second");
    let own = alice
        .release(
            &env,
            WorktreeRelease::Holder {
                session: env.alice.session,
                agent: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(own.hold.worktree, name("worktree1"));
    let causes: Vec<(String, WorktreeReleaseCause, Option<_>)> = env
        .kinds_after(before)
        .await
        .into_iter()
        .map(|kind| match kind {
            EventKind::WorktreeReleased(e) => (e.worktree.to_string(), e.cause, e.by),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        causes,
        [
            ("worktree2".to_owned(), WorktreeReleaseCause::Released, None),
            ("worktree3".to_owned(), WorktreeReleaseCause::Released, None),
            ("worktree1".to_owned(), WorktreeReleaseCause::Released, None),
        ]
    );
    let again = alice
        .release(&env, WorktreeRelease::Name(name("worktree1")))
        .await
        .unwrap_err();
    assert_eq!(again.code(), "worktree_not_held");
}

#[tokio::test]
async fn another_members_pool_is_never_reached() {
    let env = Env::new().await;
    Ask::of(&env.alice).took(&env).await;
    let bob = Ask::of(&env.bob);
    let refused = bob
        .release(&env, WorktreeRelease::Name(name("worktree1")))
        .await
        .unwrap_err();
    assert_eq!(
        refused.code(),
        "worktree_not_held",
        "bob's pool of the same ids is his own, and empty"
    );
    let holder = bob
        .release(
            &env,
            WorktreeRelease::Holder {
                session: env.alice.session,
                agent: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(holder.code(), "not_found", "alice's session is not bob's");
    let beat = bob
        .beat(
            &env,
            vec![beat("worktree1", env.alice.session, None, env.now())],
        )
        .await
        .unwrap_err();
    assert_eq!(beat.code(), "not_found");
    let foreign = Ask::of(&env.bob)
        .session(env.alice.session)
        .take(&env)
        .await
        .unwrap_err();
    assert_eq!(foreign.code(), "not_found");
    assert_eq!(
        bob.took(&env).await,
        ("worktree1".to_owned(), false),
        "two members' pools never share a hold"
    );
    assert_eq!(mine(&env, &env.alice.actor).await.len(), 1);
}

#[tokio::test]
async fn beats_keep_an_agents_hold_and_silence_ends_it() {
    let env = Env::new().await;
    let alice = Ask::of(&env.alice);
    alice.clone().agent("busy").took(&env).await;
    alice.clone().agent("quiet").took(&env).await;
    for _ in 0..2 {
        env.advance(Duration::minutes(10));
        let beaten = alice
            .beat(
                &env,
                vec![
                    beat("worktree1", env.alice.session, Some("busy"), env.now()),
                    beat("worktree1", env.alice.session, Some("busy"), env.now()),
                ],
            )
            .await
            .unwrap();
        assert_eq!(
            beaten.kept,
            [pair(
                "worktree1",
                Holder::Agent {
                    session: env.alice.session,
                    agent: agent("busy"),
                }
            )],
            "a pair named twice is answered once"
        );
    }
    env.advance(Duration::minutes(10));
    let before = env.events().await.len();
    let report = TickWorktrees::new(&env.deps).execute().await.unwrap();
    assert_eq!(report.pools, 1);
    assert!(report.failed.is_empty());
    assert_eq!(report.events, 1);
    assert_eq!(
        env.kinds_after(before).await,
        [EventKind::WorktreeReleased(WorktreeReleased {
            member: env.alice.actor.member,
            host: HostName::parse("laptop").unwrap(),
            worktree: name("worktree2"),
            holder: HolderKind::Agent,
            purpose: Purpose::parse("fix the search index").unwrap(),
            cause: WorktreeReleaseCause::Silent,
            by: None,
        })],
        "the agent that beat for thirty minutes keeps its hold, the silent one loses it"
    );
    assert_eq!(
        mine(&env, &env.alice.actor).await,
        [("worktree1".to_owned(), "laptop/4f2a91c0/busy".to_owned())]
    );
    let swept = TickWorktrees::new(&env.deps).execute().await.unwrap();
    assert_eq!((swept.pools, swept.events), (1, 0));
}

#[tokio::test]
async fn a_sessions_hold_waits_for_its_person_for_hours() {
    let env = Env::new().await;
    let alice = Ask::of(&env.alice);
    alice.took(&env).await;
    alice.clone().agent("agent-1").took(&env).await;
    env.advance(Duration::minutes(21));
    assert_eq!(
        mine(&env, &env.alice.actor).await,
        [("worktree1".to_owned(), "laptop/4f2a91c0".to_owned())],
        "after 21 silent minutes the agent's hold ended and the session's stands"
    );
    env.advance(Duration::hours(12) - Duration::minutes(21));
    assert_eq!(
        mine(&env, &env.alice.actor).await.len(),
        1,
        "a hold silent exactly twelve hours stands"
    );
    env.advance(Duration::seconds(1));
    assert!(mine(&env, &env.alice.actor).await.is_empty());
    assert_eq!(
        alice.took(&env).await,
        ("worktree1".to_owned(), false),
        "the session takes anew"
    );
}

#[tokio::test]
async fn a_beat_for_a_freed_hold_is_unknown_and_a_beat_from_the_future_counts_as_now() {
    let env = Env::new().await;
    let alice = Ask::of(&env.alice);
    alice.clone().agent("agent-1").took(&env).await;
    let beaten = alice
        .beat(
            &env,
            vec![beat(
                "worktree1",
                env.alice.session,
                Some("agent-1"),
                env.now() + Duration::hours(1),
            )],
        )
        .await
        .unwrap();
    assert_eq!(beaten.kept.len(), 1);
    env.advance(Duration::minutes(21));
    let beaten = alice
        .beat(
            &env,
            vec![
                beat("worktree1", env.alice.session, Some("agent-1"), env.now()),
                beat("worktree2", env.alice.session, None, env.now()),
            ],
        )
        .await
        .unwrap();
    assert!(
        beaten.kept.is_empty(),
        "the beat from the future was taken as now, so the hold went silent"
    );
    assert_eq!(
        beaten.unknown,
        [
            pair(
                "worktree1",
                Holder::Agent {
                    session: env.alice.session,
                    agent: agent("agent-1"),
                }
            ),
            pair("worktree2", Holder::Session(env.alice.session)),
        ]
    );
}

#[tokio::test]
async fn a_session_that_ends_ends_only_its_own_holds_and_its_agents() {
    let env = Env::new().await;
    let other = session(&env.deps, &env.alice.actor, "laptop", "laptop/second").await;
    let alice = Ask::of(&env.alice);
    alice.took(&env).await;
    alice.clone().agent("agent-1").took(&env).await;
    alice.clone().session(other).took(&env).await;
    Ask {
        pool: "d4e5f6",
        ..alice.clone()
    }
    .took(&env)
    .await;
    Ask::of(&env.bob).took(&env).await;
    let before = env.events().await.len();
    let ended = EndSession::new(&env.deps)
        .execute(&env.alice.actor, env.alice.session)
        .await
        .unwrap();
    let ended: Vec<(String, Option<&AgentKey>)> = ended
        .iter()
        .map(|h| (h.hold.worktree.to_string(), h.hold.holder.agent()))
        .collect();
    let agent_1 = agent("agent-1");
    assert_eq!(
        ended,
        [
            ("worktree1".to_owned(), None),
            ("worktree2".to_owned(), Some(&agent_1)),
            ("worktree1".to_owned(), None),
        ],
        "the session's own hold and its agent's, in both its pools, by pool and worktree"
    );
    let causes: Vec<WorktreeReleaseCause> = env
        .kinds_after(before)
        .await
        .into_iter()
        .map(|kind| match kind {
            EventKind::WorktreeReleased(e) => e.cause,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(causes, [WorktreeReleaseCause::SessionEnded; 3]);
    assert_eq!(
        mine(&env, &env.alice.actor).await,
        [("worktree3".to_owned(), "laptop/second".to_owned())]
    );
    assert_eq!(mine(&env, &env.bob.actor).await.len(), 1);
    assert!(
        EndSession::new(&env.deps)
            .execute(&env.alice.actor, env.alice.session)
            .await
            .unwrap()
            .is_empty(),
        "a session that holds nothing ends nothing"
    );
    let foreign = EndSession::new(&env.deps)
        .execute(&env.alice.actor, env.bob.session)
        .await
        .unwrap_err();
    assert_eq!(foreign.code(), "not_found");
}

#[tokio::test]
async fn an_admin_ends_a_members_hold_and_tells_them_why() {
    let env = Env::new().await;
    Ask::of(&env.bob).agent("agent-1").took(&env).await;
    let input = || ForceReleaseWorktreeInput {
        session: env.alice.session,
        member: Handle::parse("bob").unwrap(),
        host: HostName::parse("desktop").unwrap(),
        worktree: name("worktree1"),
        reason: Body::parse("the machine is being rebuilt").unwrap(),
    };
    let force = ForceReleaseWorktree::new(&env.deps);
    assert_eq!(
        force
            .execute(&env.bob.actor, input())
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
    let wrong_host = ForceReleaseWorktreeInput {
        host: HostName::parse("laptop").unwrap(),
        ..input()
    };
    assert_eq!(
        force
            .execute(&env.alice.actor, wrong_host)
            .await
            .unwrap_err()
            .code(),
        "worktree_not_held"
    );
    let before = env.events().await.len();
    let forced = force.execute(&env.alice.actor, input()).await.unwrap();
    assert_eq!(forced.ended.len(), 1);
    assert_eq!(forced.ended[0].member.as_str(), "bob");
    assert_eq!(forced.ended[0].holder, HolderKind::Agent);
    let kinds = env.kinds_after(before).await;
    assert_eq!(
        kinds[0],
        EventKind::WorktreeReleased(WorktreeReleased {
            member: env.bob.actor.member,
            host: HostName::parse("desktop").unwrap(),
            worktree: name("worktree1"),
            holder: HolderKind::Agent,
            purpose: Purpose::parse("fix the search index").unwrap(),
            cause: WorktreeReleaseCause::Released,
            by: Some(env.alice.actor.member),
        })
    );
    let thread = ReadThread::new(&env.deps)
        .execute(&env.bob.actor, forced.thread.unwrap())
        .await
        .unwrap();
    assert_eq!(thread.messages[0].record.author, Author::System);
    assert_eq!(
        thread.messages[0].record.body.as_str(),
        "alice, an admin of this project, ended your hold on the worktree worktree1 on desktop. The next take on that machine may hand it out again.\nTheir reason: the machine is being rebuilt"
    );
    assert!(mine(&env, &env.bob.actor).await.is_empty());
    assert_eq!(
        force
            .execute(&env.alice.actor, input())
            .await
            .unwrap_err()
            .code(),
        "worktree_not_held"
    );
    Ask::of(&env.alice).took(&env).await;
    let own = force
        .execute(
            &env.alice.actor,
            ForceReleaseWorktreeInput {
                member: Handle::parse("alice").unwrap(),
                host: HostName::parse("laptop").unwrap(),
                ..input()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        own.thread, None,
        "an admin is not told of their own release"
    );
}

#[tokio::test]
async fn the_status_lists_the_callers_holds_only() {
    let env = Env::new().await;
    Ask::of(&env.alice).agent("agent-1").took(&env).await;
    Ask::of(&env.bob).took(&env).await;
    let status = Status::new(&env.deps)
        .execute(&env.alice.actor)
        .await
        .unwrap();
    assert_eq!(status.worktrees.len(), 1);
    let pool = &status.worktrees[0];
    assert_eq!(pool.key.member, env.alice.actor.member);
    assert_eq!(pool.key.machine, machine(LAPTOP));
    assert_eq!(pool.key.pool, pool_id(CHECKOUT));
    assert_eq!(pool.holds.len(), 1);
    assert_eq!(pool.holds[0].hold.holder.agent(), Some(&agent("agent-1")));
    env.advance(Duration::minutes(21));
    let status = Status::new(&env.deps)
        .execute(&env.alice.actor)
        .await
        .unwrap();
    assert!(
        status.worktrees.is_empty(),
        "the status ends the silent holds before it shows them"
    );
}

#[tokio::test]
async fn a_removed_members_pools_are_freed() {
    let env = Env::new().await;
    let bob = Ask::of(&env.bob);
    bob.took(&env).await;
    Ask {
        machine: DESKTOP,
        ..bob.clone().agent("agent-1")
    }
    .took(&env)
    .await;
    Ask::of(&env.alice).took(&env).await;
    let before = env.events().await.len();
    let removed = RemoveMember::new(&env.deps)
        .execute(&env.alice.actor, &Handle::parse("bob").unwrap())
        .await
        .unwrap();
    assert_eq!(removed.worktrees, 2);
    let freed: Vec<(WorktreeReleaseCause, Option<_>)> = env
        .kinds_after(before)
        .await
        .into_iter()
        .filter_map(|kind| match kind {
            EventKind::WorktreeReleased(e) => Some((e.cause, e.by)),
            _ => None,
        })
        .collect();
    let by = Some(env.alice.actor.member);
    assert_eq!(
        freed,
        [
            (WorktreeReleaseCause::MemberRemoved, by),
            (WorktreeReleaseCause::MemberRemoved, by),
        ]
    );
    let board = Worktrees::new(&env.deps)
        .execute(&env.alice.actor, None)
        .await
        .unwrap()
        .board;
    let members: Vec<&str> = board.iter().map(|h| h.member.as_str()).collect();
    assert_eq!(members, ["alice"]);
}

#[tokio::test]
async fn the_board_shows_every_members_holds_without_their_sessions() {
    let env = Env::new().await;
    let item = env.item("gh:acme/widgets#7").await;
    Ask {
        item: Some(item),
        ..Ask::of(&env.bob).agent("agent-1")
    }
    .took(&env)
    .await;
    Ask::of(&env.alice).took(&env).await;
    env.advance(Duration::minutes(5));
    Ask::of(&env.alice).agent("agent-2").took(&env).await;
    let view = Worktrees::new(&env.deps)
        .execute(&env.importer, None)
        .await
        .unwrap();
    assert!(
        view.mine.is_empty(),
        "an integration has no pool of its own"
    );
    let board: Vec<(String, String, String, HolderKind, Option<ItemId>)> = view
        .board
        .iter()
        .map(|h| {
            (
                h.member.to_string(),
                h.host.to_string(),
                h.worktree.to_string(),
                h.holder,
                h.item,
            )
        })
        .collect();
    assert_eq!(
        board,
        [
            (
                "alice".to_owned(),
                "laptop".to_owned(),
                "worktree1".to_owned(),
                HolderKind::Session,
                None
            ),
            (
                "alice".to_owned(),
                "laptop".to_owned(),
                "worktree2".to_owned(),
                HolderKind::Agent,
                None
            ),
            (
                "bob".to_owned(),
                "desktop".to_owned(),
                "worktree1".to_owned(),
                HolderKind::Agent,
                Some(item)
            ),
        ]
    );
    assert_eq!(view.board[1].since, env.now());
}

#[tokio::test]
async fn takes_name_their_worktrees_once_and_an_existing_item() {
    let env = Env::new().await;
    let alice = Ask::of(&env.alice);
    let codes = [
        Ask {
            names: &[],
            ..alice.clone()
        }
        .take(&env)
        .await,
        Ask {
            names: &["worktree1", "worktree2", "worktree1"],
            ..alice.clone()
        }
        .take(&env)
        .await,
        Ask {
            item: Some(ItemId(9_999)),
            ..alice.clone()
        }
        .take(&env)
        .await,
    ]
    .map(|result| result.unwrap_err().code());
    assert_eq!(
        codes,
        [
            "worktree_names_invalid",
            "worktree_names_invalid",
            "not_found"
        ]
    );
    assert!(mine(&env, &env.alice.actor).await.is_empty());
}

#[tokio::test]
async fn an_integration_takes_releases_beats_and_ends_nothing() {
    let env = Env::new().await;
    let bot = &env.importer;
    let session = env.alice.session;
    let as_bot = Ask {
        actor: bot,
        ..Ask::of(&env.alice)
    };
    let refusals = [
        as_bot.take(&env).await.map(|_| ()),
        as_bot
            .release(&env, WorktreeRelease::Name(name("worktree1")))
            .await
            .map(|_| ()),
        as_bot.beat(&env, Vec::new()).await.map(|_| ()),
        EndSession::new(&env.deps)
            .execute(bot, session)
            .await
            .map(|_| ()),
        ForceReleaseWorktree::new(&env.deps)
            .execute(
                bot,
                ForceReleaseWorktreeInput {
                    session,
                    member: Handle::parse("alice").unwrap(),
                    host: HostName::parse("laptop").unwrap(),
                    worktree: name("worktree1"),
                    reason: Body::parse("x").unwrap(),
                },
            )
            .await
            .map(|_| ()),
    ];
    for (n, refusal) in refusals.into_iter().enumerate() {
        assert_eq!(refusal.unwrap_err().code(), "forbidden", "case {n}");
    }
}
