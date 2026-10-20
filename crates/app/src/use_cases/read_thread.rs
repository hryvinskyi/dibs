//! One conversation with its messages.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{MemberId, MessageId, ThreadId};
use dibs_domain::message::{Ack, AckState};
use dibs_domain::wide::ask_stands;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedMessage, NamedThread, Names};
use crate::ports::Store;
use crate::use_cases::acks::{people, reached};
use crate::use_cases::readable::readable_thread;
use crate::work::Work;

/// One recipient of a message that asks to be acknowledged, and when they acknowledged it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientAck {
    /// The recipient.
    pub member: MemberId,
    /// Their handle.
    pub handle: Handle,
    /// When they acknowledged it; `None` while they have not.
    pub acked_at: Option<OffsetDateTime>,
}

/// A thread and its messages, their members named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadView {
    /// The thread.
    pub thread: NamedThread,
    /// Its messages, oldest first.
    pub messages: Vec<NamedMessage>,
    /// For each message that asks to be acknowledged, its recipients ordered by handle, with when
    /// each acknowledged it.
    pub acks: BTreeMap<MessageId, Vec<RecipientAck>>,
    /// The messages of `acks` that still ask for an acknowledgement: all of them but a wide claim's
    /// announcement whose wide claim has ended, which asks for nothing any more.
    pub asking: BTreeSet<MessageId>,
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
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let thread = readable_thread(&mut work, actor, thread).await?;
        let messages = work.messages(actor.project, thread.id).await?;
        let names = Names::load(&mut work).await?;
        let mut acks = BTreeMap::new();
        let mut asking = BTreeSet::new();
        if messages.iter().any(|message| message.ack_required) {
            let people = people(&mut work).await?;
            let recorded = work.thread_acks(actor.project, thread.id).await?;
            let mut claims = None;
            for message in messages.iter().filter(|message| message.ack_required) {
                let of_message: Vec<Ack> = recorded
                    .iter()
                    .filter(|ack| ack.message == message.id)
                    .copied()
                    .collect();
                let state = AckState::derive(&reached(&thread, message, &people), &of_message);
                let mut entries = state
                    .entries()
                    .map(|(member, acked_at)| {
                        Ok(RecipientAck {
                            member,
                            handle: names.handle(member)?,
                            acked_at,
                        })
                    })
                    .collect::<Result<Vec<_>, AppError>>()?;
                entries.sort_by(|a, b| a.handle.cmp(&b.handle));
                acks.insert(message.id, entries);
                if still_asks(&mut work, &mut claims, message.id).await? {
                    asking.insert(message.id);
                }
            }
        }
        Ok(ThreadView {
            thread: names.thread(thread)?,
            messages: messages
                .into_iter()
                .map(|message| names.message(message))
                .collect::<Result<_, _>>()?,
            acks,
            asking,
        })
    }
}

/// Whether a message that asks to be acknowledged still does: a wide claim's announcement asks
/// only while its wide claim stands. The project's claims are read once, the first time one is
/// needed.
async fn still_asks(
    work: &mut Work,
    claims: &mut Option<Vec<Claim>>,
    message: MessageId,
) -> Result<bool, AppError> {
    let project = work.project_id();
    let Some(ask) = work.wide_ask(project, message).await? else {
        return Ok(true);
    };
    if claims.is_none() {
        *claims = Some(work.claims(project).await?);
    }
    Ok(claims
        .as_deref()
        .is_some_and(|claims| ask_stands(&ask, claims)))
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
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
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
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
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

    #[tokio::test]
    async fn a_message_that_asks_shows_each_recipient_and_their_ack() {
        let env = Env::new().await;
        let started = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse("Freeze").unwrap(),
                    to: Addressees::Project,
                    item: None,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Freeze main.").unwrap(),
                    importance: dibs_domain::message::Importance::High,
                    ack_required: true,
                },
            )
            .await
            .unwrap();
        let reply = PostMessage::new(&env.deps)
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread: started.thread.record.id,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("ok").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        let read = ReadThread::new(&env.deps);
        let id = started.message.record.id;
        let before = read
            .execute(&env.bob.actor, started.thread.record.id)
            .await
            .unwrap();
        assert_eq!(before.acks.len(), 1, "only the message that asks");
        assert!(!before.acks.contains_key(&reply.record.id));
        let entries = &before.acks[&id];
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (entries[0].handle.as_str(), entries[0].acked_at),
            ("bob", None)
        );
        env.advance(time::Duration::minutes(4));
        crate::use_cases::AckMessage::new(&env.deps)
            .execute(&env.bob.actor, id)
            .await
            .unwrap();
        let after = read
            .execute(&env.alice.actor, started.thread.record.id)
            .await
            .unwrap();
        assert_eq!(after.acks[&id][0].acked_at, Some(env.now()));
        assert_eq!(after.acks[&id][0].member, env.bob.actor.member);
    }
}
