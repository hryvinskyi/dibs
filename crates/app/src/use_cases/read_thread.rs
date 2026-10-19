//! One conversation with its messages.

use std::sync::Arc;

use dibs_domain::ids::ThreadId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedMessage, NamedThread, Names};
use crate::ports::Store;
use crate::use_cases::readable::readable_thread;
use crate::work::Work;

/// A thread and its messages, their members named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadView {
    /// The thread.
    pub thread: NamedThread,
    /// Its messages, oldest first.
    pub messages: Vec<NamedMessage>,
}

/// Shows a thread the caller may read. Reading does not mark it read; that is its own step.
pub struct ReadThread {
    store: Arc<dyn Store>,
}

impl ReadThread {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The thread and its messages.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a thread that does not
    /// exist or that the caller may not read; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, thread: ThreadId) -> Result<ThreadView, AppError> {
        actor.require_person()?;
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let thread = readable_thread(&mut work, actor, thread).await?;
        let messages = work.messages(actor.project, thread.id).await?;
        let names = Names::load(&mut work).await?;
        Ok(ThreadView {
            thread: names.thread(thread)?,
            messages: messages
                .into_iter()
                .map(|message| names.message(message))
                .collect::<Result<_, _>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        Addressees, PostMessage, PostMessageInput, StartThread, StartThreadInput,
    };

    #[tokio::test]
    async fn a_thread_reads_oldest_first_and_only_by_its_people() {
        let env = Env::new().await;
        let label = SessionLabel::parse("web").unwrap();
        let started = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse("Private").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    item: None,
                    label: label.clone(),
                    body: Body::parse("first").unwrap(),
                },
            )
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        PostMessage::new(&env.deps)
            .execute(
                &env.alice.actor,
                PostMessageInput {
                    thread: started.thread.record.id,
                    label,
                    body: Body::parse("second").unwrap(),
                },
            )
            .await
            .unwrap();
        let read = ReadThread::new(&env.deps);
        let view = read
            .execute(&env.alice.actor, started.thread.record.id)
            .await
            .unwrap();
        let bodies: Vec<&str> = view
            .messages
            .iter()
            .map(|m| m.record.body.as_str())
            .collect();
        assert_eq!(bodies, ["first", "second"]);
        assert!(view.messages.iter().all(|m| m.author.as_str() == "alice"));
        assert_eq!(view.thread.recipients, [Handle::parse("alice").unwrap()]);
        assert_eq!(
            read.execute(&env.bob.actor, started.thread.record.id)
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(
            read.execute(&env.importer, started.thread.record.id)
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }
}
