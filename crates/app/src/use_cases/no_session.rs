//! The recipients of a message whom no session of theirs will show it soon.

use std::collections::BTreeSet;

use time::{Duration, OffsetDateTime};

use dibs_domain::handle::Handle;
use dibs_domain::ids::MemberId;
use dibs_domain::message::Recipients;
use dibs_domain::settings::ProjectSettings;

use crate::error::AppError;
use crate::named::Names;
use crate::records::{MessageRecord, ThreadRecord};
use crate::use_cases::acks::{people, reached};
use crate::work::Work;

/// How recently one of a member's open sessions must have been seen for the member to have a
/// session at work. A session is seen when it registers or sends a heartbeat, and heartbeats come
/// from the syncs its host's hooks start, so a session left open and idle counts as unseen after
/// this long. A session that was ended is not at work however recently it was seen, until it
/// registers again or sends a heartbeat.
pub const AT_WORK_WITHIN: Duration = Duration::hours(1);

/// A recipient none of whose open sessions was seen within [`AT_WORK_WITHIN`]: what they were sent
/// waits for their next session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoSessionAtWork {
    /// The recipient.
    pub member: Handle,
    /// Whether they hold a live claim, so that an area they hold may be asked of them with a grant
    /// request rather than waited for.
    pub holds_live_claim: bool,
}

/// The members `message` of `thread` reaches as one it names or as the thread's author, none of
/// whose open sessions was seen within [`AT_WORK_WITHIN`] of `now`, ordered by handle. A thread
/// addressed to the whole project names nobody, so its messages list nobody.
pub(crate) async fn no_session_at_work(
    work: &mut Work,
    thread: &ThreadRecord,
    message: &MessageRecord,
    settings: &ProjectSettings,
    names: &Names,
    now: OffsetDateTime,
) -> Result<Vec<NoSessionAtWork>, AppError> {
    if !matches!(thread.recipients, Recipients::Members(_)) {
        return Ok(Vec::new());
    }
    let project = work.project_id();
    let people = people(work).await?;
    let seen = work
        .members_seen_since(project, now - AT_WORK_WITHIN)
        .await?;
    let unseen: BTreeSet<MemberId> = reached(thread, message, &people)
        .into_iter()
        .filter(|member| !seen.contains(member))
        .collect();
    if unseen.is_empty() {
        return Ok(Vec::new());
    }
    let holding: BTreeSet<MemberId> = work
        .claims(project)
        .await?
        .into_iter()
        .filter(|claim| claim.is_live(now, settings.stale_after()))
        .map(|claim| claim.member)
        .collect();
    let mut listed = unseen
        .into_iter()
        .map(|member| {
            Ok(NoSessionAtWork {
                member: names.handle(member)?,
                holds_live_claim: holding.contains(&member),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    listed.sort_by(|a, b| a.member.cmp(&b.member));
    Ok(listed)
}
