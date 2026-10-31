//! Sending the signs of life this machine's holders left in the worktrees of its pools.
//!
//! The server ends a hold whose holder stays silent too long, and a holder working in a worktree
//! leaves only a beat file behind: whatever notices the work changes the file, without the network.
//! A sending reads, in every pool of the project on this machine, each worktree this machine holds
//! a record of whose beat file changed since the last sending and since the record's take, and
//! sends one beat per such worktree, for the holder the record names, dated by the file's change.
//!
//! A beat the server answers as unknown names a hold the server ended while the beats could not
//! reach it. The worktree is then taken again, by its name alone, for the same holder and the same
//! purpose; when another holder has it by then, this machine forgets its record of the earlier
//! holder and touches nothing else.
//!
//! A server without the worktree routes is older than this client: the signs of life are dropped
//! without a word, and that server is not asked again for a day — unless a take, a release or a
//! listing of worktrees is answered after it was found so, which shows the server serves them now.
//! Every sending that ends, with beats or without, is remembered as the last one, so the same signs
//! of life are not sent twice.

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use dibs_agent_host::upkeep::is_fresh;
use dibs_client::api::{
    BeatWorktreesRequest, BeatenWorktree, TakeWorktreeRequest, WorktreeBeat, WorktreeOutcome,
};
use dibs_client::state::{StateDir, WorktreeHoldRecord, WorktreeState, read_json, write_json};
use dibs_client::{Client, ClientError};
use dibs_domain::ids::SessionId;
use dibs_domain::report::MachineId;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{PoolId, WorktreeName};

use crate::commands::worktree::DEFAULT_PURPOSE;

/// How long a server found to serve no worktree pools is not asked again.
pub const UNSERVED_FOR: Duration = Duration::days(1);

/// The file of the project's cache that says when the server last answered a worktree route.
const SERVED: &str = "worktrees-served";

#[derive(Serialize, Deserialize)]
struct Served {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

/// Remembers that the server answered a worktree route at `at`, when it had been found before then
/// to serve no worktree pools: from now on the signs of life are sent again.
pub fn served(state: &StateDir, at: OffsetDateTime) {
    let Some(unserved) = state.worktrees_unserved_at() else {
        return;
    };
    if served_at(state).is_some_and(|served| served >= unserved) {
        return;
    }
    if let Err(error) = write_json(&state.json_file(SERVED), &Served { at }) {
        tracing::debug!(error = %error, "a server serving worktree pools could not be remembered");
    }
}

/// When the server last answered a worktree route, as far as this machine remembers.
fn served_at(state: &StateDir) -> Option<OffsetDateTime> {
    read_json::<Served>(&state.json_file(SERVED)).map(|served| served.at)
}

/// Whether the server was found, within a day of `now` and since it last answered a worktree
/// route, to serve no worktree pools.
pub fn unserved(state: &StateDir, now: OffsetDateTime) -> bool {
    let Some(unserved) = state.worktrees_unserved_at() else {
        return false;
    };
    is_fresh(unserved, now, UNSERVED_FOR) && served_at(state).is_none_or(|served| served < unserved)
}

/// What a sending did.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct Sent {
    /// How many signs of life reached the server.
    pub beats: usize,
    /// The worktrees taken again for the holder their record names, the server having let them go.
    pub retaken: Vec<WorktreeName>,
    /// The worktrees whose record this machine forgot, another holder having them now.
    pub forgotten: Vec<WorktreeName>,
    /// Whether the server serves no worktree pools, so that nothing was sent.
    pub unserved: bool,
}

/// What a sending works with.
pub struct Beats<'a> {
    /// The server.
    pub client: &'a Client,
    /// What this machine remembers of the project.
    pub state: &'a StateDir,
    /// The member's session the beats are sent from.
    pub sender: SessionId,
    /// This machine.
    pub machine: &'a MachineId,
}

impl Beats<'_> {
    /// Sends the signs of life left since the last sending, as of `now`.
    ///
    /// # Errors
    /// The [`ClientError`] of a server that gave no decision; the signs of life are then left to
    /// the next sending. A refusal of one pool's beats is logged and the other pools are sent.
    pub async fn send(&self, now: OffsetDateTime) -> Result<Sent, ClientError> {
        let mut sent = Sent::default();
        if unserved(self.state, now) {
            sent.unserved = true;
            self.remember_sent(now);
            return Ok(sent);
        }
        let since = self.state.worktree_beats_sent_at();
        for (pool, files) in self.state.worktree_pools() {
            let beats = due(&files.holds(), |name| files.beat_at(name), since);
            if beats.is_empty() {
                continue;
            }
            let count = beats.len();
            let request = BeatWorktreesRequest {
                session_id: self.sender,
                machine: self.machine.clone(),
                pool: pool.clone(),
                beats,
            };
            let answer = match self.client.beat_worktrees(&request).await {
                Ok(answer) => answer,
                Err(ClientError::ServerTooOld { .. }) => {
                    if let Err(error) = self.state.remember_worktrees_unserved(now) {
                        tracing::debug!(error = %error, "a server without worktree pools could not be remembered");
                    }
                    sent.unserved = true;
                    self.remember_sent(now);
                    return Ok(sent);
                }
                Err(error) if error.is_unavailable() => return Err(error),
                Err(error) => {
                    tracing::debug!(
                        code = error.code(),
                        "the signs of life of a pool were refused"
                    );
                    continue;
                }
            };
            sent.beats += count;
            for unknown in &answer.unknown {
                self.take_again(&pool, &files, unknown, &mut sent).await?;
            }
        }
        self.remember_sent(now);
        Ok(sent)
    }

    /// Takes the worktree a beat named in vain again for the holder this machine's record names;
    /// forgets the record when another holder has the worktree.
    async fn take_again(
        &self,
        pool: &PoolId,
        files: &WorktreeState,
        unknown: &BeatenWorktree,
        sent: &mut Sent,
    ) -> Result<(), ClientError> {
        let name = &unknown.worktree;
        let is_holder = |record: &WorktreeHoldRecord| {
            record.is_of(unknown.holder_session, unknown.agent.as_ref())
        };
        let Some(record) = files.hold(name).filter(is_holder) else {
            return Ok(());
        };
        let Some(purpose) = record
            .purpose
            .clone()
            .or_else(|| Purpose::parse(DEFAULT_PURPOSE).ok())
        else {
            return Ok(());
        };
        let request = TakeWorktreeRequest {
            session_id: record.session_id,
            machine: self.machine.clone(),
            pool: pool.clone(),
            names: vec![name.clone()],
            agent: record.agent.clone(),
            purpose,
            item: record.item,
        };
        match self.client.take_worktree(&request).await {
            Ok(took)
                if took.outcome == WorktreeOutcome::Took
                    && took.worktree.as_ref() == Some(name) =>
            {
                sent.retaken.push(name.clone());
            }
            Ok(_) => {
                if files.hold(name).is_some_and(|record| is_holder(&record)) {
                    if let Err(error) = files.forget_hold(name) {
                        tracing::debug!(error = %error, "a worktree's holder could not be forgotten");
                    }
                    sent.forgotten.push(name.clone());
                }
            }
            Err(error) if error.is_unavailable() => return Err(error),
            Err(error) => {
                tracing::debug!(
                    code = error.code(),
                    "a worktree the server let go of could not be taken again"
                );
            }
        }
        Ok(())
    }

    fn remember_sent(&self, at: OffsetDateTime) {
        if let Err(error) = self.state.remember_worktree_beats_sent(at) {
            tracing::debug!(error = %error, "the sending of signs of life could not be remembered");
        }
    }
}

/// The beats due for the holds this machine records in one pool: one for each worktree whose
/// sign of life, read by `beat_at`, is newer than the last sending `since` and no older than the
/// record's take, for the holder the record names.
pub fn due(
    holds: &[(WorktreeName, WorktreeHoldRecord)],
    beat_at: impl Fn(&WorktreeName) -> Option<OffsetDateTime>,
    since: Option<OffsetDateTime>,
) -> Vec<WorktreeBeat> {
    holds
        .iter()
        .filter_map(|(name, record)| {
            let seen_at = beat_at(name)?;
            let new = since.is_none_or(|since| seen_at > since);
            (new && seen_at >= record.taken_at).then(|| WorktreeBeat {
                worktree: name.clone(),
                holder_session: record.session_id,
                agent: record.agent.clone(),
                seen_at,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use dibs_domain::worktree::AgentKey;

    use super::*;

    fn name(text: &str) -> WorktreeName {
        WorktreeName::parse(text).unwrap()
    }

    fn at(minutes: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000) + Duration::minutes(minutes)
    }

    fn record(session: u64, agent: Option<&str>, taken_at: OffsetDateTime) -> WorktreeHoldRecord {
        WorktreeHoldRecord {
            session_id: SessionId(session),
            agent: agent.map(|agent| AgentKey::parse(agent).unwrap()),
            purpose: None,
            item: None,
            taken_at,
            reset_done: true,
        }
    }

    #[test]
    fn a_beat_is_sent_for_the_recorded_holder_once_per_new_sign_of_life() {
        let holds = vec![
            (name("worktree1"), record(4, Some("a1"), at(0))),
            (name("worktree2"), record(4, None, at(0))),
            (name("worktree3"), record(5, Some("b2"), at(0))),
        ];
        let beats = |worktree: &WorktreeName| match worktree.as_str() {
            "worktree1" => Some(at(12)),
            "worktree2" => Some(at(8)),
            _ => None,
        };
        let sent = due(&holds, beats, Some(at(10)));
        assert_eq!(
            sent,
            vec![WorktreeBeat {
                worktree: name("worktree1"),
                holder_session: SessionId(4),
                agent: Some(AgentKey::parse("a1").unwrap()),
                seen_at: at(12),
            }],
            "only what changed since the last sending, and only where a beat was left"
        );
        let never_sent = due(&holds, beats, None);
        assert_eq!(never_sent.len(), 2);
        assert_eq!(never_sent[1].agent, None, "a session holds worktree2");
        assert!(due(&holds, beats, Some(at(12))).is_empty());
    }

    #[test]
    fn a_sign_of_life_older_than_the_take_is_not_the_holders() {
        let holds = vec![(name("worktree1"), record(4, Some("a1"), at(30)))];
        assert!(due(&holds, |_| Some(at(20)), None).is_empty());
        assert_eq!(due(&holds, |_| Some(at(30)), None).len(), 1);
    }
}
