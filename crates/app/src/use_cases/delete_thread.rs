//! An admin deleting a conversation at once.

use std::sync::Arc;

use dibs_domain::event::{EventKind, ThreadDeleted};
use dibs_domain::ids::ThreadId;

use crate::authority::AdminAuthority;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::ports::{Clock, EventNotifier, Store};
use crate::work::Work;

/// A conversation deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletedThread {
    /// The conversation.
    pub thread: ThreadId,
    /// How many messages went with it.
    pub messages: u64,
}

/// Deletes a conversation of the project with every message in it, for an admin of the project or
/// the operator of the service, whether or not they may read it: what was written there is gone,
/// and the log records only that it was (`thread_deleted`, ids alone). The events about it are no
/// longer shown to anyone, since the conversation they were about cannot be found.
pub struct DeleteThread {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl DeleteThread {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Deletes the thread.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] unless `by` may administer the project; [`AppError::NotFound`] for a
    /// thread the project does not have; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        by: &impl AdminAuthority,
        thread: ThreadId,
    ) -> Result<DeletedThread, AppError> {
        let project = by.administered_project()?;
        let now = self.clock.now();
        let mut work = Work::begin_administering(&*self.store, by).await?;
        if work.thread(project, thread).await?.is_none() {
            return Err(AppError::NotFound(Missing::Thread(thread)));
        }
        let messages = work.delete_thread(project, thread).await?;
        work.record(
            now,
            EventKind::ThreadDeleted(ThreadDeleted {
                thread,
                by: by.acting_member(),
            }),
        )
        .await?;
        work.commit(&*self.notifier).await?;
        Ok(DeletedThread { thread, messages })
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, Importance, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        Addressees, EventCursor, ListEvents, ListThreads, StartThread, StartThreadInput,
    };

    #[tokio::test]
    async fn an_admin_deletes_a_thread_at_once_leaving_an_event_without_content() {
        let env = Env::new().await;
        let started = StartThread::new(&env.deps)
            .execute(
                &env.bob.actor,
                StartThreadInput {
                    subject: Subject::parse("A secret pasted by mistake").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    item: None,
                    label: SessionLabel::parse("desktop/77aa01bc").unwrap(),
                    body: Body::parse("password: hunter2").unwrap(),
                    importance: Importance::High,
                    ack_required: true,
                },
            )
            .await
            .unwrap();
        let thread = started.thread.record.id;
        let delete = DeleteThread::new(&env.deps);
        assert_eq!(
            delete
                .execute(&env.bob.actor, thread)
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
        let deleted = delete.execute(&env.alice.actor, thread).await.unwrap();
        assert_eq!(deleted.messages, 1);
        assert!(
            ListThreads::new(&env.deps)
                .execute(&env.bob.actor, None)
                .await
                .unwrap()
                .is_empty()
        );
        let page = ListEvents::new(&env.deps)
            .execute(&env.bob.actor, EventCursor::Start, 500)
            .await
            .unwrap();
        let kinds: Vec<&EventKind> = page.events.iter().map(|named| &named.event.kind).collect();
        assert!(
            kinds.iter().all(|kind| kind.thread() != Some(thread)),
            "the events about it are shown to nobody"
        );
        assert_eq!(
            kinds.last(),
            Some(&&EventKind::ThreadDeleted(ThreadDeleted {
                thread,
                by: Some(env.alice.actor.member),
            }))
        );
        assert_eq!(
            delete
                .execute(&env.alice.actor, thread)
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
    }
}
