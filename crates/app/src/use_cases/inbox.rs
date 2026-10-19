//! The threads waiting for the caller.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedSummary, Names};
use crate::ports::Store;
use crate::use_cases::readable::readable_summaries;
use crate::work::Work;

/// Lists the threads the caller may read, newest activity first, with what they have not read; only
/// the ones with unread messages when asked.
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

    /// The caller's threads.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        unread_only: bool,
    ) -> Result<Vec<NamedSummary>, AppError> {
        actor.require_person()?;
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let summaries = work
            .thread_summaries(actor.project, actor.member, None)
            .await?;
        let names = Names::load(&mut work).await?;
        readable_summaries(actor, summaries)
            .into_iter()
            .filter(|summary| !unread_only || summary.unread > 0)
            .map(|summary| names.summary(summary))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{Addressees, MarkRead, StartThread, StartThreadInput};

    fn input(to: Addressees, subject: &str) -> StartThreadInput {
        StartThreadInput {
            subject: Subject::parse(subject).unwrap(),
            to,
            item: None,
            label: SessionLabel::parse("web").unwrap(),
            body: Body::parse("hello").unwrap(),
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
        let bobs = inbox.execute(&env.bob.actor, false).await.unwrap();
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
        let waiting = inbox.execute(&env.bob.actor, true).await.unwrap();
        let ids: Vec<_> = waiting.iter().map(|s| s.thread.record.id).collect();
        assert_eq!(ids, [to_all.thread.record.id]);
        let authors: Vec<&str> = bobs.iter().map(|s| s.thread.author.as_str()).collect();
        assert_eq!(authors, ["bob", "alice", "alice"]);
        assert_eq!(bobs[0].thread.recipients, [Handle::parse("alice").unwrap()]);
        assert!(bobs[1].thread.recipients.is_empty());
        assert_eq!(
            inbox
                .execute(&env.importer, false)
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }
}
