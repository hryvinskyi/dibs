//! The messages waiting for the caller's acknowledgement.

use std::sync::Arc;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedMessage, NamedThread, Names};
use crate::ports::Store;
use crate::use_cases::acks::awaiting;
use crate::work::Work;

/// A message that asks the caller for an acknowledgement they have not given, with its thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnackedMessage {
    /// The thread, its members named.
    pub thread: NamedThread,
    /// The message, its author named.
    pub message: NamedMessage,
}

/// Lists the messages that ask the caller for an acknowledgement they have not given, newest
/// first: of the threads they may read, the messages they are a recipient of.
pub struct Unacked {
    store: Arc<dyn Store>,
}

impl Unacked {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The caller's unacknowledged messages.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<Vec<UnackedMessage>, AppError> {
        actor.require_person()?;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        unacked(&mut work, actor).await
    }
}

/// The actor's unacknowledged messages, read inside `work`.
pub(crate) async fn unacked(
    work: &mut Work,
    actor: &Actor,
) -> Result<Vec<UnackedMessage>, AppError> {
    let waiting = awaiting(work, actor).await?;
    let names = Names::load(work).await?;
    waiting
        .into_iter()
        .map(|(message, thread)| {
            Ok(UnackedMessage {
                thread: names.thread(thread)?,
                message: names.message(message)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, Importance, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        AckMessage, Addressees, PostMessage, PostMessageInput, StartThread, StartThreadInput,
    };

    fn input(to: Addressees, subject: &str, ack_required: bool) -> StartThreadInput {
        StartThreadInput {
            subject: Subject::parse(subject).unwrap(),
            to,
            item: None,
            label: SessionLabel::parse("web").unwrap(),
            body: Body::parse("please confirm").unwrap(),
            importance: Importance::High,
            ack_required,
        }
    }

    #[tokio::test]
    async fn the_unacked_list_holds_what_awaits_me_newest_first_until_i_ack() {
        let env = Env::new().await;
        let start = StartThread::new(&env.deps);
        let to_bob = Addressees::Members(vec![Handle::parse("bob").unwrap()]);
        let first = start
            .execute(&env.alice.actor, input(to_bob.clone(), "First", true))
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        start
            .execute(&env.alice.actor, input(to_bob, "Plain", false))
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        let second = start
            .execute(&env.alice.actor, input(Addressees::Project, "Second", true))
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        PostMessage::new(&env.deps)
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread: second.thread.record.id,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("my own, asking").unwrap(),
                    importance: Importance::Normal,
                    ack_required: true,
                },
            )
            .await
            .unwrap();
        let unacked = Unacked::new(&env.deps);
        let bobs = unacked.execute(&env.bob.actor).await.unwrap();
        let subjects: Vec<&str> = bobs
            .iter()
            .map(|u| u.thread.record.subject.as_str())
            .collect();
        assert_eq!(subjects, ["Second", "First"], "never my own message");
        assert!(bobs.iter().all(|u| u.message.author.as_str() == "alice"));
        AckMessage::new(&env.deps)
            .execute(&env.bob.actor, first.message.record.id)
            .await
            .unwrap();
        let left: Vec<_> = unacked
            .execute(&env.bob.actor)
            .await
            .unwrap()
            .into_iter()
            .map(|u| u.message.record.id)
            .collect();
        assert_eq!(left, [second.message.record.id]);
        let alices = unacked.execute(&env.alice.actor).await.unwrap();
        assert_eq!(alices.len(), 1, "bob's reply asks alice");
        assert_eq!(
            unacked.execute(&env.importer).await.unwrap_err().code(),
            "forbidden"
        );
    }
}
