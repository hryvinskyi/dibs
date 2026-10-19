//! The threads waiting for the caller, and the messages waiting for their acknowledgement.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedSummary, Names};
use crate::ports::Store;
use crate::use_cases::acks::{awaiting, awaiting_per_thread};
use crate::use_cases::readable::readable_summaries;
use crate::use_cases::unacked::{UnackedMessage, unacked};
use crate::work::Work;

/// What the inbox is asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboxQuery {
    /// Only the threads with unread messages.
    pub unread_only: bool,
    /// The messages awaiting the caller's acknowledgement too.
    pub unacked: bool,
}

/// The caller's inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxView {
    /// The threads, newest activity first.
    pub threads: Vec<NamedSummary>,
    /// The messages awaiting the caller's acknowledgement, newest first, when they were asked for.
    pub unacked: Option<Vec<UnackedMessage>>,
}

/// Lists the threads the caller may read, newest activity first, with what they have not read and
/// how many messages await their acknowledgement; only the ones with unread messages when asked;
/// and, when asked, the messages awaiting their acknowledgement — all read in one transaction.
pub struct Inbox {
    store: Arc<dyn Store>,
}

impl Inbox {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The caller's inbox.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, query: InboxQuery) -> Result<InboxView, AppError> {
        actor.require_person()?;
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let summaries = work
            .thread_summaries(actor.project, actor.member, None)
            .await?;
        let waiting = awaiting_per_thread(&awaiting(&mut work, actor).await?);
        let names = Names::load(&mut work).await?;
        let threads = readable_summaries(actor, summaries)
            .into_iter()
            .filter(|summary| !query.unread_only || summary.unread > 0)
            .map(|summary| {
                let count = waiting.get(&summary.thread.id).copied().unwrap_or(0);
                names.summary(summary, count)
            })
            .collect::<Result<_, _>>()?;
        let unacked = if query.unacked {
            Some(unacked(&mut work, actor).await?)
        } else {
            None
        };
        Ok(InboxView { threads, unacked })
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use dibs_domain::message::Importance;

    use crate::use_cases::{AckMessage, Addressees, MarkRead, StartThread, StartThreadInput};

    fn input(to: Addressees, subject: &str) -> StartThreadInput {
        StartThreadInput {
            subject: Subject::parse(subject).unwrap(),
            to,
            item: None,
            label: SessionLabel::parse("web").unwrap(),
            body: Body::parse("hello").unwrap(),
            importance: Importance::Normal,
            ack_required: false,
        }
    }

    #[tokio::test]
    async fn the_inbox_holds_what_the_caller_may_read_and_counts_the_unread() {
        let env = Env::new().await;
        let start = StartThread::new(&env.deps);
        let to_bob = start
            .execute(
                &env.alice.actor,
                input(
                    Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                    "To bob",
                ),
            )
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        let to_all = start
            .execute(&env.alice.actor, input(Addressees::Project, "To all"))
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        start
            .execute(
                &env.bob.actor,
                input(
                    Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    "Bob writes",
                ),
            )
            .await
            .unwrap();
        let inbox = Inbox::new(&env.deps);
        let bobs = inbox
            .execute(&env.bob.actor, InboxQuery::default())
            .await
            .unwrap()
            .threads;
        let subjects: Vec<&str> = bobs
            .iter()
            .map(|s| s.thread.record.subject.as_str())
            .collect();
        assert_eq!(subjects, ["Bob writes", "To all", "To bob"]);
        let unread: Vec<u64> = bobs.iter().map(|s| s.unread).collect();
        assert_eq!(unread, [0, 1, 1]);
        MarkRead::new(&env.deps)
            .execute(&env.bob.actor, to_bob.thread.record.id)
            .await
            .unwrap();
        let waiting = inbox
            .execute(
                &env.bob.actor,
                InboxQuery {
                    unread_only: true,
                    unacked: false,
                },
            )
            .await
            .unwrap()
            .threads;
        let ids: Vec<_> = waiting.iter().map(|s| s.thread.record.id).collect();
        assert_eq!(ids, [to_all.thread.record.id]);
        let authors: Vec<&str> = bobs.iter().map(|s| s.thread.author.as_str()).collect();
        assert_eq!(authors, ["bob", "alice", "alice"]);
        assert_eq!(bobs[0].thread.recipients, [Handle::parse("alice").unwrap()]);
        assert!(bobs[1].thread.recipients.is_empty());
        assert_eq!(
            inbox
                .execute(&env.importer, InboxQuery::default())
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }

    #[tokio::test]
    async fn the_inbox_counts_and_lists_what_awaits_an_ack_when_asked() {
        let env = Env::new().await;
        let asked = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    importance: Importance::Urgent,
                    ack_required: true,
                    ..input(Addressees::Project, "Freeze")
                },
            )
            .await
            .unwrap();
        let inbox = Inbox::new(&env.deps);
        let plain = inbox
            .execute(&env.bob.actor, InboxQuery::default())
            .await
            .unwrap();
        assert_eq!(plain.unacked, None, "not asked for");
        assert_eq!(plain.threads[0].awaiting_ack, 1);
        assert_eq!(plain.threads[0].importance, Importance::Urgent);
        let full = inbox
            .execute(
                &env.bob.actor,
                InboxQuery {
                    unread_only: false,
                    unacked: true,
                },
            )
            .await
            .unwrap();
        let ids: Vec<_> = full
            .unacked
            .unwrap()
            .iter()
            .map(|u| u.message.record.id)
            .collect();
        assert_eq!(ids, [asked.message.record.id]);
        let author = inbox
            .execute(&env.alice.actor, InboxQuery::default())
            .await
            .unwrap();
        assert_eq!(author.threads[0].awaiting_ack, 0, "never for its author");
        AckMessage::new(&env.deps)
            .execute(&env.bob.actor, asked.message.record.id)
            .await
            .unwrap();
        let after = inbox
            .execute(
                &env.bob.actor,
                InboxQuery {
                    unread_only: false,
                    unacked: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(after.threads[0].awaiting_ack, 0);
        assert_eq!(after.unacked, Some(Vec::new()));
    }
}
