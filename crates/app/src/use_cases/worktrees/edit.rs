//! What an edit made in a pool worktree teaches the worktree's hold: the work item it is for.

use std::collections::BTreeSet;

use time::OffsetDateTime;

use dibs_domain::ids::{ItemId, MemberId, SessionId};
use dibs_domain::radar::WorktreeRef;
use dibs_domain::worktree::{EditInWorktree, PoolKey, Silence, decide_edit_in_worktree};

use crate::error::AppError;
use crate::use_cases::worktrees::scene::PoolScene;
use crate::work::Work;

/// Applies an edit `session` of `member` made in `worktree` of the member's pool, attributed to
/// the member's claims on `items`, and answers the item the worktree's hold names after it. When
/// the hold is held under that session, by the session or one of its agents, and has not gone
/// silent, a hold that names no item takes the edits' item when they were attributed to the claims
/// of exactly one, and a hold that names one keeps it; the pool is saved when it changed. A free
/// worktree, a hold under another session or a silent one answers `None` and changes nothing.
///
/// # Errors
/// [`AppError::Store`] when the store fails.
pub(crate) async fn edit_in_worktree(
    work: &mut Work,
    member: MemberId,
    session: SessionId,
    worktree: &WorktreeRef,
    items: &BTreeSet<ItemId>,
    now: OffsetDateTime,
) -> Result<Option<ItemId>, AppError> {
    let key = PoolKey {
        member,
        machine: worktree.machine.clone(),
        pool: worktree.pool.clone(),
    };
    let mut scene = PoolScene::load(work, &key, now).await?;
    let mut only = items.iter().copied();
    let edit = EditInWorktree {
        worktree: worktree.worktree.clone(),
        session,
        item: only.next().filter(|_| only.next().is_none()),
    };
    let item = scene.apply(decide_edit_in_worktree(
        &scene.pool,
        &edit,
        now,
        Silence::DEFAULT,
    ));
    scene.save(work, now).await?;
    Ok(item)
}
