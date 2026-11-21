//! Marking many messages read at once: every notice dibs wrote, or every message about one work
//! item.

use std::collections::BTreeSet;
use std::sync::Arc;

use dibs_domain::ids::{ItemId, ThreadId};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store};
use crate::records::{ThreadSummary, WrittenBy};
use crate::use_cases::common::existing_item;
use crate::use_cases::readable::readable_summaries;
use crate::work::Work;

/// Which messages to mark read together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadTogether {
    /// Every message dibs wrote, in every conversation the caller may read.
    FromDibs,
    /// Every message of every conversation filed under this work item that the caller may read.
    Item(ItemId),
}

impl ReadTogether {
    /// The work item whose conversations are looked at; `None` for every conversation.
    const fn item(self) -> Option<ItemId> {
        match self {
            Self::FromDibs => None,
            Self::Item(item) => Some(item),
        }
    }

    /// Whose messages are marked.
    const fn written_by(self) -> WrittenBy {
        match self {
            Self::FromDibs => WrittenBy::Dibs,
            Self::Item(_) => WrittenBy::Anyone,
        }
    }

    /// Whether a conversation, as the caller sees it, has a message this choice would mark.
    const fn has_unread(self, summary: &ThreadSummary) -> bool {
        match self.written_by() {
            WrittenBy::Dibs => summary.unread_from_dibs > 0,
            WrittenBy::Anyone => summary.unread > 0,
        }
    }
}

/// Records many messages as read by the caller at once, never their own nor what dibs wrote on
/// their behalf. Which conversations they may read is decided here, by the rule every reading
/// follows; the store only marks the messages of the conversations it is handed. Read state is the
/// caller's own: the messages do not change, and nothing is appended to the event log.
pub struct MarkReadTogether {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
}

impl MarkReadTogether {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
        }
    }

    /// Marks the messages `which` names read; answers how many were unread.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a work item that
    /// does not exist; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, which: ReadTogether) -> Result<u64, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        if let Some(item) = which.item() {
            existing_item(&mut work, item).await?;
        }
        let summaries = work
            .thread_summaries(actor.project, actor.member, which.item())
            .await?;
        let threads: BTreeSet<ThreadId> = readable_summaries(actor, summaries)
            .iter()
            .filter(|summary| which.has_unread(summary))
            .map(|summary| summary.thread.id)
            .collect();
        let newly = work
            .mark_read_in(
                actor.project,
                actor.member,
                &threads,
                which.written_by(),
                now,
            )
            .await?;
        work.commit_quietly().await?;
        Ok(newly)
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{MemberId, MessageId};
    use dibs_domain::message::{Author, Body, Importance, Recipients, SessionLabel, Subject};

    use super::*;
    use crate::records::{MessageRecord, ThreadRecord};
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        Addressees, ListThreads, PostMessage, PostMessageInput, StartThread, StartThreadInput,
    };

    /// Stores a conversation dibs started, addressed to `to`, filed under `item`, with one message
    /// of its own.
    async fn notice(env: &Env, id: u64, to: &[MemberId], item: Option<ItemId>) -> ThreadId {
        let thread = ThreadRecord {
            id: ThreadId(id),
            subject: Subject::parse(&format!("Notice {id}")).unwrap(),
            item,
            author: Author::System,
            recipients: Recipients::Members(to.iter().copied().collect()),
            created_at: env.now(),
        };
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.insert_thread(env.project, &thread).await.unwrap();
        tx.commit().await.unwrap();
        dibs_writes(env, thread.id, id).await;
        thread.id
    }

    /// Stores a message dibs wrote in a conversation that exists.
    async fn dibs_writes(env: &Env, thread: ThreadId, id: u64) {
        let message = MessageRecord {
            id: MessageId(id),
            thread,
            author: Author::System,
            label: SessionLabel::parse("dibs").unwrap(),
            body: Body::parse("Something happened to your claim.").unwrap(),
            importance: Importance::Normal,
            ack_required: false,
            created_at: env.now(),
        };
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.insert_message(env.project, &message).await.unwrap();
        tx.commit().await.unwrap();
    }

    async fn talk(env: &Env, from: &Actor, to: &str, item: Option<ItemId>) -> ThreadId {
        StartThread::new(&env.deps)
            .execute(
                from,
                StartThreadInput {
                    subject: Subject::parse("Rates").unwrap(),
                    to: Addressees::Members(vec![Handle::parse(to).unwrap()]),
                    item,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Can you look?").unwrap(),
                    importance: Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap()
            .thread
            .record
            .id
    }

    /// The unread counts of every conversation `actor` may read, from people and from dibs,
    /// ordered by thread id.
    async fn unread(env: &Env, actor: &Actor) -> Vec<(ThreadId, u64, u64)> {
        let mut counts: Vec<_> = ListThreads::new(&env.deps)
            .execute(actor, None)
            .await
            .unwrap()
            .into_iter()
            .map(|summary| {
                (
                    summary.thread.record.id,
                    summary.unread_from_people,
                    summary.unread_from_dibs,
                )
            })
            .collect();
        counts.sort_unstable();
        counts
    }

    #[tokio::test]
    async fn marking_dibs_read_marks_only_what_dibs_wrote_that_the_caller_may_read() {
        let env = Env::new().await;
        let alice = env.alice.actor.member;
        let bob = env.bob.actor.member;
        let to_alice = notice(&env, 9001, &[alice], None).await;
        let to_bob = notice(&env, 9002, &[bob], None).await;
        let talk_with_bob = talk(&env, &env.bob.actor, "alice", None).await;
        dibs_writes(&env, talk_with_bob, 9003).await;
        let alices_own = talk(&env, &env.alice.actor, "bob", None).await;
        dibs_writes(&env, alices_own, 9004).await;
        let mark = MarkReadTogether::new(&env.deps);
        let events = env.events().await.len();

        assert_eq!(
            mark.execute(&env.alice.actor, ReadTogether::FromDibs)
                .await
                .unwrap(),
            2,
            "the notice to her and dibs's message in bob's conversation"
        );
        assert_eq!(
            unread(&env, &env.alice.actor).await,
            [(talk_with_bob, 1, 0), (alices_own, 0, 0), (to_alice, 0, 0)],
            "bob's message stays unread; what dibs wrote in her own conversation never was"
        );
        assert_eq!(
            unread(&env, &env.bob.actor).await,
            [(talk_with_bob, 0, 0), (alices_own, 1, 1), (to_bob, 0, 1)],
            "nothing of bob's changed, and the notice to him was never hers to read"
        );
        assert_eq!(
            mark.execute(&env.alice.actor, ReadTogether::FromDibs)
                .await
                .unwrap(),
            0
        );
        assert_eq!(env.events().await.len(), events, "nothing is logged");
    }

    #[tokio::test]
    async fn marking_an_item_read_marks_its_conversations_only() {
        let env = Env::new().await;
        let alice = env.alice.actor.member;
        let rates = env.item("gh:acme/widgets#1").await;
        let search = env.item("gh:acme/widgets#2").await;
        let about_rates = talk(&env, &env.bob.actor, "alice", Some(rates)).await;
        PostMessage::new(&env.deps)
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread: about_rates,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("And the totals.").unwrap(),
                    importance: Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        let rates_notice = notice(&env, 9001, &[alice], Some(rates)).await;
        let about_search = talk(&env, &env.bob.actor, "alice", Some(search)).await;
        let unfiled = talk(&env, &env.bob.actor, "alice", None).await;
        let mark = MarkReadTogether::new(&env.deps);

        assert_eq!(
            mark.execute(&env.alice.actor, ReadTogether::Item(rates))
                .await
                .unwrap(),
            3
        );
        assert_eq!(
            unread(&env, &env.alice.actor).await,
            [
                (about_rates, 0, 0),
                (about_search, 1, 0),
                (unfiled, 1, 0),
                (rates_notice, 0, 0),
            ]
        );
        assert_eq!(
            mark.execute(&env.alice.actor, ReadTogether::Item(ItemId(9999)))
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(
            mark.execute(&env.importer, ReadTogether::FromDibs)
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }
}
