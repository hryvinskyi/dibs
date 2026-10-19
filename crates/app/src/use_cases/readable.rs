//! Which threads a member may read.

use dibs_domain::ids::ThreadId;
use dibs_domain::message::may_read;

use crate::actor::Actor;
use crate::error::{AppError, Missing};
use crate::records::{ThreadRecord, ThreadSummary};
use crate::work::Work;

/// Whether the actor may read the thread.
pub(crate) fn may_read_thread(actor: &Actor, thread: &ThreadRecord) -> bool {
    may_read(thread.author, &thread.recipients, actor.member, actor.role)
}

/// The thread, when the actor may read it. A thread they may not read is reported as missing, so
/// the answer never confirms that it exists.
pub(crate) async fn readable_thread(
    work: &mut Work,
    actor: &Actor,
    id: ThreadId,
) -> Result<ThreadRecord, AppError> {
    let project = work.project_id();
    work.thread(project, id)
        .await?
        .filter(|thread| may_read_thread(actor, thread))
        .ok_or(AppError::NotFound(Missing::Thread(id)))
}

/// The summaries the actor may read, in the order given.
pub(crate) fn readable_summaries(
    actor: &Actor,
    summaries: Vec<ThreadSummary>,
) -> Vec<ThreadSummary> {
    summaries
        .into_iter()
        .filter(|summary| may_read_thread(actor, &summary.thread))
        .collect()
}
