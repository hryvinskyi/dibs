//! Marking a conversation read.

use std::sync::Arc;

use dibs_domain::ids::ThreadId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, Store};
use crate::use_cases::readable::readable_thread;
use crate::work::Work;

/// Records every message of a thread as read by the caller. Read state is the caller's own: the
/// messages do not change, and nothing is appended to the event log.
pub struct MarkRead {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
}

impl MarkRead {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
        }
    }

    /// Marks the thread read; answers how many messages were unread.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a thread that does not
    /// exist or that the caller may not read; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, thread: ThreadId) -> Result<u64, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let thread = readable_thread(&mut work, actor, thread).await?;
        let newly = work
            .mark_read(actor.project, actor.member, thread.id, now)
            .await?;
        work.commit_quietly().await?;
        Ok(newly)
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::message::{Body, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{Addressees, StartThread, StartThreadInput};

    #[tokio::test]
    async fn marking_read_counts_once_and_appends_nothing() {
        let env = Env::new().await;
        let started = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse("Hello").unwrap(),
                    to: Addressees::Project,
                    item: None,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("hello").unwrap(),
                },
            )
            .await
            .unwrap();
        let events = env.events().await.len();
        let mark = MarkRead::new(&env.deps);
        assert_eq!(
            mark.execute(&env.bob.actor, started.thread.id)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            mark.execute(&env.bob.actor, started.thread.id)
                .await
                .unwrap(),
            0
        );
        assert_eq!(env.events().await.len(), events);
        assert_eq!(
            mark.execute(&env.bob.actor, ThreadId(9999))
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
    }
}
