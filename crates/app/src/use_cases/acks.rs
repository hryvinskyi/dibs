//! What acknowledgements rest on: the people a message reaches, and the messages that still await
//! one member's acknowledgement, read inside the caller's transaction.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use dibs_domain::ids::{MemberId, ThreadId};
use dibs_domain::message::{Person, message_recipients};

use crate::actor::Actor;
use crate::error::AppError;
use crate::records::{MessageRecord, ThreadRecord};
use crate::use_cases::readable::may_read_thread;
use crate::work::Work;

/// Every member of the transaction's project as recipients are told from.
pub(crate) async fn people(work: &mut Work) -> Result<Vec<Person>, AppError> {
    let project = work.project_id();
    Ok(work
        .members(project)
        .await?
        .into_iter()
        .map(|member| Person {
            member: member.id,
            role: member.role,
            joined_at: member.created_at,
        })
        .collect())
}

/// The members a message of this thread reaches.
pub(crate) fn reached(
    thread: &ThreadRecord,
    message: &MessageRecord,
    people: &[Person],
) -> BTreeSet<MemberId> {
    message_recipients(
        thread.author,
        &thread.recipients,
        message.author,
        message.created_at,
        people,
    )
}

/// The messages that ask the actor for an acknowledgement they have not given, each with its
/// thread, newest first: only messages of threads the actor may read, and only those the actor is
/// a recipient of.
pub(crate) async fn awaiting(
    work: &mut Work,
    actor: &Actor,
) -> Result<Vec<(MessageRecord, ThreadRecord)>, AppError> {
    let project = work.project_id();
    let asked = work.ack_requests(project, actor.member).await?;
    if asked.is_empty() {
        return Ok(Vec::new());
    }
    let people = people(work).await?;
    let mut threads: BTreeMap<ThreadId, Option<ThreadRecord>> = BTreeMap::new();
    let mut awaiting = Vec::new();
    for message in asked {
        let thread = match threads.entry(message.thread) {
            Entry::Occupied(known) => known.into_mut(),
            Entry::Vacant(slot) => slot.insert(
                work.thread(project, message.thread)
                    .await?
                    .filter(|thread| may_read_thread(actor, thread)),
            ),
        };
        let Some(thread) = thread else {
            continue;
        };
        if reached(thread, &message, &people).contains(&actor.member) {
            awaiting.push((message, thread.clone()));
        }
    }
    Ok(awaiting)
}

/// How many messages of each thread await the actor's acknowledgement.
pub(crate) fn awaiting_per_thread(
    awaiting: &[(MessageRecord, ThreadRecord)],
) -> BTreeMap<ThreadId, u64> {
    let mut counts = BTreeMap::new();
    for (message, _) in awaiting {
        *counts.entry(message.thread).or_insert(0) += 1;
    }
    counts
}
