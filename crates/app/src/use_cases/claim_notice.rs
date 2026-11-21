//! What dibs writes to the holder of one claim about that claim: every such message goes into the
//! claim's own conversation, which dibs starts at the first one.

#[cfg(test)]
mod tests;

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::ids::{MessageId, ThreadId};
use dibs_domain::message::{Author, Body, Importance, Recipients, Subject};

use crate::error::AppError;
use crate::named::ItemName;
use crate::ports::IdGen;
use crate::records::ClaimThread;
use crate::use_cases::writing::{Posted, SystemPost, SystemReply, fit_subject};
use crate::work::Work;

/// A message dibs writes to a claim's holder about that claim. It goes into the claim's
/// conversation: addressed to the holder alone, filed under the claim's item, started by dibs in
/// nobody's name. When dibs never wrote about the claim, or the conversation it started is gone
/// (deleted by an admin or pruned), the message starts a new one and the claim is linked to it.
/// A renewal keeps the claim and so its conversation; a take-over or a new claim on the same item
/// is another claim, with a conversation of its own. The message is written by dibs under the
/// label `dibs` and asks no acknowledgement.
pub(crate) struct ClaimNotice<'a> {
    /// The claim it is about, as it was when the message was written; it may have ended since.
    pub(crate) claim: &'a Claim,
    /// The claim's item, as the subject of a new conversation names it.
    pub(crate) item: &'a ItemName,
    /// The text.
    pub(crate) body: Body,
    /// How much it asks for attention.
    pub(crate) importance: Importance,
}

impl ClaimNotice<'_> {
    /// Stores the message, and the conversation and its link when the claim has none, and records
    /// `thread_started` for a new conversation and `message_posted` for the message. Answers the
    /// conversation.
    pub(crate) async fn send(
        self,
        work: &mut Work,
        ids: &dyn IdGen,
        now: OffsetDateTime,
    ) -> Result<ThreadId, AppError> {
        let link = claim_link(work, self.claim).await?;
        self.send_linked(work, ids, now, link).await
    }

    /// Sends the message as [`ClaimNotice::send`] does, into the conversation of `link`, which
    /// [`claim_link`] read for the same claim in the same transaction: a caller that decides from
    /// the link whether to write at all reads it once.
    pub(crate) async fn send_linked(
        self,
        work: &mut Work,
        ids: &dyn IdGen,
        now: OffsetDateTime,
        link: Option<ClaimThread>,
    ) -> Result<ThreadId, AppError> {
        let project = work.project_id();
        let instance = self.claim.instance();
        if let Some(thread) = link.map(|link| link.thread) {
            SystemReply {
                thread,
                message: MessageId(ids.next()),
                body: self.body,
                importance: self.importance,
                ack_required: false,
            }
            .send(work, now)
            .await?;
            return Ok(thread);
        }
        let posted = SystemPost {
            ids: Posted::next(ids),
            subject: claim_subject(&self.item.label(), self.item.title.as_str())?
                .as_str()
                .to_owned(),
            item: Some(self.claim.item),
            on_behalf_of: Author::System,
            to: Recipients::Members([self.claim.member].into()),
            body: self.body,
            importance: self.importance,
            ack_required: false,
        }
        .send(work, now)
        .await?;
        work.insert_claim_thread(
            project,
            &ClaimThread {
                thread: posted.thread,
                claim: instance,
                base_moved_told_at: None,
            },
        )
        .await?;
        Ok(posted.thread)
    }
}

/// The claim's link to its conversation, with when a moved base was last told in it, when dibs
/// started one and it still exists; `None` when dibs never wrote about the claim or the
/// conversation is gone.
pub(crate) async fn claim_link(
    work: &mut Work,
    claim: &Claim,
) -> Result<Option<ClaimThread>, AppError> {
    let project = work.project_id();
    let Some(link) = work.claim_thread(project, &claim.instance()).await? else {
        return Ok(None);
    };
    Ok(work
        .thread(project, link.thread)
        .await?
        .is_some()
        .then_some(link))
}

/// The subject of a claim's conversation: `Your claim on <reference> "<title>"`, cut at a
/// character boundary to the characters a subject holds.
pub(crate) fn claim_subject(reference: &str, title: &str) -> Result<Subject, AppError> {
    fit_subject(&format!("Your claim on {reference} \"{title}\""))
}
