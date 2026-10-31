//! The pools as an answer shows them: a member's own pools in full, each hold with the label of the
//! session holding it, and every member's holds as the board shows them to the whole project.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::message::SessionLabel;
use dibs_domain::session::HostName;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{HolderKind, PoolKey, WorktreeHold, WorktreeName, WorktreePool};

use crate::error::AppError;
use crate::named::Names;
use crate::ports::StoreError;
use crate::work::Work;

/// A hold of one of the caller's own pools, with the label of the session that holds it or whose
/// agent does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldWorktree {
    /// The hold.
    pub hold: WorktreeHold,
    /// The label of `hold.holder.session()`.
    pub session: SessionLabel,
}

/// One of the caller's pools: which, and its holds, ordered by worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnPool {
    /// The pool.
    pub key: PoolKey,
    /// Its holds.
    pub holds: Vec<HeldWorktree>,
}

/// A hold as the board shows it to every member of the project: whose pool, on which host, which
/// worktree, whether a session or an agent holds it, why and since when. It never names the
/// session or the agent holding it, the machine's id or the pool's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardHold {
    /// The handle of the member whose pool it is.
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
    pub since: OffsetDateTime,
}

impl BoardHold {
    /// The hold of a pool of the member of this handle.
    pub(crate) fn of(member: Handle, hold: &WorktreeHold) -> Self {
        Self {
            member,
            host: hold.host.clone(),
            worktree: hold.worktree.clone(),
            holder: hold.holder.kind(),
            purpose: hold.purpose.clone(),
            item: hold.item,
            since: hold.taken_at,
        }
    }
}

/// The pools as one member reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreesView {
    /// The caller's own pools with a hold, ordered by key.
    pub mine: Vec<OwnPool>,
    /// Every hold of every member's pools, the caller's included, ordered by the member's handle,
    /// the host and the worktree.
    pub board: Vec<BoardHold>,
}

/// The labels of the sessions holds name, read once each inside one transaction.
#[derive(Default)]
pub(crate) struct Labels {
    known: BTreeMap<SessionId, SessionLabel>,
}

impl Labels {
    /// The hold with the label of its holder's session.
    ///
    /// # Errors
    /// [`AppError::Store`] when the session does not exist: the hold refers to nobody.
    pub(crate) async fn held(
        &mut self,
        work: &mut Work,
        hold: WorktreeHold,
    ) -> Result<HeldWorktree, AppError> {
        let id = hold.holder.session();
        let session = match self.known.get(&id) {
            Some(label) => label.clone(),
            None => {
                let project = work.project_id();
                let label = work
                    .session(project, id)
                    .await?
                    .ok_or_else(|| StoreError::Corrupt {
                        what: format!("a worktree hold names session {id}, which does not exist"),
                    })?
                    .label;
                self.known.insert(id, label.clone());
                label
            }
        };
        Ok(HeldWorktree { hold, session })
    }

    /// Every hold, each with its session's label, in the order given.
    pub(crate) async fn all(
        &mut self,
        work: &mut Work,
        holds: impl IntoIterator<Item = WorktreeHold>,
    ) -> Result<Vec<HeldWorktree>, AppError> {
        let mut held = Vec::new();
        for hold in holds {
            held.push(self.held(work, hold).await?);
        }
        Ok(held)
    }
}

/// The pools in full, each hold with its session's label, in the order given.
pub(crate) async fn own_pools<'a>(
    work: &mut Work,
    pools: impl IntoIterator<Item = &'a WorktreePool>,
) -> Result<Vec<OwnPool>, AppError> {
    let mut labels = Labels::default();
    let mut own = Vec::new();
    for pool in pools {
        own.push(OwnPool {
            key: pool.key().clone(),
            holds: labels.all(work, pool.holds().iter().cloned()).await?,
        });
    }
    Ok(own)
}

/// Every hold of the pools as the board shows it, ordered by handle, host and worktree.
pub(crate) fn board<'a>(
    names: &Names,
    pools: impl IntoIterator<Item = &'a WorktreePool>,
) -> Result<Vec<BoardHold>, AppError> {
    let mut board = Vec::new();
    for pool in pools {
        let member = names.handle(pool.key().member)?;
        board.extend(
            pool.holds()
                .iter()
                .map(|hold| BoardHold::of(member.clone(), hold)),
        );
    }
    board.sort_by(|a, b| {
        (&a.member, &a.host, &a.worktree, a.since).cmp(&(&b.member, &b.host, &b.worktree, b.since))
    });
    Ok(board)
}
