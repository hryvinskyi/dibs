//! The requests and answers of the worktree routes: the worktrees a member's machine lends to its
//! sessions and to the agents those sessions start.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, SessionId, ThreadId};
use dibs_domain::message::SessionLabel;
use dibs_domain::report::MachineId;
use dibs_domain::session::HostName;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{AgentKey, HolderKind, PoolId, WorktreeName};

/// A hold of one of the caller's own pools, with its holder named in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnWorktreeHold {
    /// The worktree.
    pub worktree: WorktreeName,
    /// Whether a session or one of its agents holds it.
    pub holder: HolderKind,
    /// The session holding it, or whose agent holds it.
    pub session_id: SessionId,
    /// That session's label.
    pub session: SessionLabel,
    /// The agent holding it; `None` when the session itself does.
    pub agent: Option<AgentKey>,
    /// The host of the holder's session.
    pub host: HostName,
    /// Why.
    pub purpose: Purpose,
    /// The work item it is held for, when one was named.
    pub item: Option<ItemId>,
    /// When it was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub taken_at: OffsetDateTime,
    /// When its holder was last seen using it.
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
}

/// One of the caller's own pools with a hold: the machine and the pool its client named it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnWorktreePool {
    /// The machine the pool lives on.
    pub machine: MachineId,
    /// The pool, on that machine.
    pub pool: PoolId,
    /// Its holds, ordered by worktree.
    pub holds: Vec<OwnWorktreeHold>,
}

/// A hold as every member of the project sees it: no machine, pool, session or agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardWorktreeHold {
    /// The member whose pool it is.
    pub member: Handle,
    /// The host of the holder's session.
    pub host: HostName,
    /// The worktree.
    pub worktree: WorktreeName,
    /// Whether a session or one of its agents holds it.
    pub holder: HolderKind,
    /// Why.
    pub purpose: Purpose,
    /// The work item it is held for, when one was named.
    pub item: Option<ItemId>,
    /// When it was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

/// `GET /v1/worktrees`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktrees {
    /// The caller's own pools with a hold.
    pub pools: Vec<OwnWorktreePool>,
    /// Every member's holds, the caller's included.
    pub board: Vec<BoardWorktreeHold>,
}

/// `POST /v1/worktrees/take`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TakeWorktreeRequest {
    /// The session taking, or whose agent takes.
    pub session_id: SessionId,
    /// The machine the pool lives on.
    pub machine: MachineId,
    /// The pool.
    pub pool: PoolId,
    /// The worktrees that may be handed out, best first.
    pub names: Vec<WorktreeName>,
    /// The agent taking, for an agent of the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKey>,
    /// Why.
    pub purpose: Purpose,
    /// The work item, by local id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
}

/// What a take came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeOutcome {
    /// The holder holds a worktree now.
    Took,
    /// Every worktree named is held.
    Busy,
}

/// The answer of a take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TookWorktree {
    /// What it came to.
    pub outcome: WorktreeOutcome,
    /// The worktree the holder holds now; `None` when busy.
    pub worktree: Option<WorktreeName>,
    /// Whether the holder held it before asking.
    pub again: bool,
    /// When busy, the hold of each worktree named, in the order named.
    #[serde(default)]
    pub held: Vec<OwnWorktreeHold>,
}

/// Which hold a release ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum WorktreeRelease {
    /// The hold of this worktree, whichever of the caller's sessions or agents holds it.
    Worktree {
        /// The worktree.
        worktree: WorktreeName,
    },
    /// The hold of this holder.
    Holder {
        /// The holder's session.
        holder_session: SessionId,
        /// The holder's agent; absent for the session itself.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<AgentKey>,
    },
}

/// `POST /v1/worktrees/release`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseWorktreeRequest {
    /// The caller's session.
    pub session_id: SessionId,
    /// The machine the pool lives on.
    pub machine: MachineId,
    /// The pool.
    pub pool: PoolId,
    /// Which hold ends.
    #[serde(flatten)]
    pub release: WorktreeRelease,
}

/// The answer of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeReleased {
    /// The hold that ended, as it was.
    pub released: OwnWorktreeHold,
}

/// One holder's sign of life in a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeBeat {
    /// The worktree.
    pub worktree: WorktreeName,
    /// The holder's session.
    pub holder_session: SessionId,
    /// The holder's agent; absent for the session itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKey>,
    /// When the holder was last seen using it.
    #[serde(with = "time::serde::rfc3339")]
    pub seen_at: OffsetDateTime,
}

/// `POST /v1/worktrees/beat`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BeatWorktreesRequest {
    /// The caller's session sending the beats.
    pub session_id: SessionId,
    /// The machine the pool lives on.
    pub machine: MachineId,
    /// The pool.
    pub pool: PoolId,
    /// The beats.
    pub beats: Vec<WorktreeBeat>,
}

/// A worktree and the holder a beat named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeatenWorktree {
    /// The worktree.
    pub worktree: WorktreeName,
    /// The holder's session.
    pub holder_session: SessionId,
    /// The holder's agent; `None` for the session itself.
    pub agent: Option<AgentKey>,
}

/// The answer of the beats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreesBeaten {
    /// The holds the beats kept.
    pub kept: Vec<BeatenWorktree>,
    /// The pairs that hold nothing: the worktree is free, or another holder has it.
    pub unknown: Vec<BeatenWorktree>,
}

/// The answer of `POST /v1/sessions/{id}/end`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnded {
    /// The holds that ended with the session, the holds of its agents included.
    pub released: Vec<OwnWorktreeHold>,
}

/// `POST /v1/admin/worktrees/force-release`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForceReleaseWorktreeRequest {
    /// The admin's session.
    pub session_id: SessionId,
    /// The member whose hold ends.
    pub member: Handle,
    /// The host the board names the hold under.
    pub host: HostName,
    /// The worktree.
    pub worktree: WorktreeName,
    /// Why, as told to the member.
    pub reason: String,
}

/// The answer of a forced release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeForced {
    /// The holds that ended, as the board showed them.
    pub released: Vec<BoardWorktreeHold>,
    /// The thread that told the member; `None` when the admin's own hold ended.
    pub thread_id: Option<ThreadId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> WorktreeName {
        WorktreeName::parse(text).unwrap()
    }

    #[test]
    fn a_release_names_either_a_worktree_or_a_holder() {
        let request = |release| ReleaseWorktreeRequest {
            session_id: SessionId(9),
            machine: MachineId::from_bits(1),
            pool: PoolId::parse("c0ffee").unwrap(),
            release,
        };
        let by_name = serde_json::to_value(request(WorktreeRelease::Worktree {
            worktree: name("worktree2"),
        }))
        .unwrap();
        assert_eq!(by_name["worktree"], "worktree2");
        assert!(by_name.get("holder_session").is_none());
        let by_agent = serde_json::to_value(request(WorktreeRelease::Holder {
            holder_session: SessionId(9),
            agent: Some(AgentKey::parse("a1b2").unwrap()),
        }))
        .unwrap();
        assert_eq!(
            (&by_agent["holder_session"], &by_agent["agent"]),
            (&serde_json::json!(9), &serde_json::json!("a1b2"))
        );
        assert!(by_agent.get("worktree").is_none());
        let by_session = serde_json::to_value(request(WorktreeRelease::Holder {
            holder_session: SessionId(9),
            agent: None,
        }))
        .unwrap();
        assert!(by_session.get("agent").is_none());
    }

    #[test]
    fn a_busy_take_reads_without_a_worktree() {
        let busy: TookWorktree =
            serde_json::from_str(r#"{"outcome":"busy","worktree":null,"again":false,"held":[]}"#)
                .unwrap();
        assert_eq!(busy.outcome, WorktreeOutcome::Busy);
        assert_eq!(busy.worktree, None);
    }
}
