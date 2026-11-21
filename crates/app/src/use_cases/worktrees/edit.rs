//! What an edit made in a pool worktree teaches the worktree's hold: the work item it is for.

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::edit::Attribution;
use dibs_domain::ids::{ItemId, MemberId, SessionId};
use dibs_domain::radar::WorktreeRef;
use dibs_domain::worktree::{
    Attributed, EditInWorktree, PoolKey, Silence, decide_edit_in_worktree,
};

use crate::error::AppError;
use crate::use_cases::worktrees::scene::PoolScene;
use crate::work::Work;

/// Applies an edit `session` of `member` made in `worktree` of the member's pool, attributed to
/// the member's claims `touched`, each with how the edit came to it, and answers the item the
/// worktree's hold names after it. When the hold is held under that session, by the session or one
/// of its agents, has not gone silent, and the edit was attributed to exactly one claim, which that
/// session took or last renewed and which the edit's area named, a hold whose take named an item
/// keeps it and any other hold learns that claim's item, replacing an item it learned before; the
/// pool is saved when it changed. An edit attributed to no claim, to several, to another
/// session's, or to a claim it reached only as the member's target teaches nothing. A free
/// worktree, a hold under another session or a silent one answers `None` and changes nothing.
///
/// # Errors
/// [`AppError::Store`] when the store fails.
pub(crate) async fn edit_in_worktree<'a>(
    work: &mut Work,
    member: MemberId,
    session: SessionId,
    worktree: &WorktreeRef,
    touched: impl IntoIterator<Item = (&'a Claim, Attribution)>,
    now: OffsetDateTime,
) -> Result<Option<ItemId>, AppError> {
    let edit = EditInWorktree {
        worktree: worktree.worktree.clone(),
        session,
        attributed: touched
            .into_iter()
            .map(|(claim, by)| {
                let of = Attributed {
                    session: claim.session,
                    by,
                };
                (claim.item, of)
            })
            .collect(),
    };
    let key = PoolKey {
        member,
        machine: worktree.machine.clone(),
        pool: worktree.pool.clone(),
    };
    let mut scene = PoolScene::load(work, &key, now).await?;
    let item = scene.apply(decide_edit_in_worktree(
        &scene.pool,
        &edit,
        now,
        Silence::DEFAULT,
    ));
    scene.save(work, now).await?;
    Ok(item)
}
