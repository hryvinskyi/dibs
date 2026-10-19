//! Who read and acknowledged a message the caller wrote.

use std::collections::BTreeMap;
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::handle::Handle;
use dibs_domain::ids::MessageId;
use dibs_domain::message::AckState;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedMessage, NamedThread, Names};
use crate::ports::Store;
use crate::use_cases::ack::readable_message;
use crate::use_cases::acks::{people, reached};
use crate::work::Work;

/// One recipient of a message: when they read it and when they acknowledged it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientStatus {
    /// The recipient's handle.
    pub member: Handle,
    /// When they read it, if they did.
    pub read_at: Option<OffsetDateTime>,
    /// When they acknowledged it, if they did.
    pub acked_at: Option<OffsetDateTime>,
}

/// A message the caller wrote with where each of its recipients stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentStatusView {
    /// The thread.
    pub thread: NamedThread,
    /// The message.
    pub message: NamedMessage,
    /// Its recipients, ordered by handle.
    pub recipients: Vec<RecipientStatus>,
}

/// Shows its author who a message reached, and for each of them when they read and acknowledged
/// it.
pub struct SentStatus {
    store: Arc<dyn Store>,
}

impl SentStatus {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The message's recipients as they stand.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a message that does
    /// not exist or is in a thread the caller may not read; [`AppError::NotAuthor`] for somebody
    /// else's message; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        message: MessageId,
    ) -> Result<SentStatusView, AppError> {
        actor.require_person()?;
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let (message, thread) = readable_message(&mut work, actor, message).await?;
        if message.author != actor.member {
            return Err(AppError::NotAuthor);
        }
        let people = people(&mut work).await?;
        let acks = work.acks(actor.project, message.id).await?;
        let state = AckState::derive(&reached(&thread, &message, &people), &acks);
        let reads: BTreeMap<_, _> = work
            .reads_of(actor.project, message.id)
            .await?
            .into_iter()
            .map(|read| (read.member, read.at))
            .collect();
        let names = Names::load(&mut work).await?;
        let mut recipients = state
            .entries()
            .map(|(member, acked_at)| {
                Ok(RecipientStatus {
                    member: names.handle(member)?,
                    read_at: reads.get(&member).copied(),
                    acked_at,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        recipients.sort_by(|a, b| a.member.cmp(&b.member));
        Ok(SentStatusView {
            thread: names.thread(thread)?,
            message: names.message(message)?,
            recipients,
        })
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, Importance, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::{Env, START};
    use crate::use_cases::{AckMessage, Addressees, MarkRead, StartThread, StartThreadInput};

    #[tokio::test]
    async fn its_author_sees_who_read_and_who_acked() {
        let env = Env::new().await;
        let started = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse("Freeze").unwrap(),
                    to: Addressees::Project,
                    item: None,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Freeze main until noon.").unwrap(),
                    importance: Importance::Urgent,
                    ack_required: true,
                },
            )
            .await
            .unwrap();
        let id = started.message.record.id;
        let status = SentStatus::new(&env.deps);
        let before = status.execute(&env.alice.actor, id).await.unwrap();
        assert_eq!(
            before.recipients,
            [RecipientStatus {
                member: Handle::parse("bob").unwrap(),
                read_at: None,
                acked_at: None,
            }],
            "the people of the project but its author, and never an integration"
        );
        env.advance(time::Duration::minutes(2));
        MarkRead::new(&env.deps)
            .execute(&env.bob.actor, started.thread.record.id)
            .await
            .unwrap();
        env.advance(time::Duration::minutes(3));
        AckMessage::new(&env.deps)
            .execute(&env.bob.actor, id)
            .await
            .unwrap();
        let after = status.execute(&env.alice.actor, id).await.unwrap();
        assert_eq!(
            after.recipients[0].read_at,
            Some(START + time::Duration::minutes(2))
        );
        assert_eq!(
            after.recipients[0].acked_at,
            Some(START + time::Duration::minutes(5))
        );
        assert_eq!(after.message.record.importance, Importance::Urgent);
        assert_eq!(
            status.execute(&env.bob.actor, id).await.unwrap_err().code(),
            "not_author"
        );
        assert_eq!(
            status
                .execute(&env.alice.actor, MessageId(9_999))
                .await
                .unwrap_err()
                .code(),
            "not_found"
        );
        assert_eq!(
            status.execute(&env.importer, id).await.unwrap_err().code(),
            "forbidden"
        );
    }
}
