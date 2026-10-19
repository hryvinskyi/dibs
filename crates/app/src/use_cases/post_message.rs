//! Replying in a conversation.

use std::sync::Arc;

use dibs_domain::event::{EventKind, MessagePosted};
use dibs_domain::ids::{MessageId, ThreadId};
use dibs_domain::message::{Body, Importance, SessionLabel};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedMessage, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::records::MessageRecord;
use crate::use_cases::readable::readable_thread;
use crate::work::Work;

/// The message to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostMessageInput {
    /// The thread.
    pub thread: ThreadId,
    /// The label of the session writing (`web` for the browser).
    pub label: SessionLabel,
    /// The text.
    pub body: Body,
    /// How much it asks for its recipients' attention.
    pub importance: Importance,
    /// Whether each recipient is asked to acknowledge it.
    pub ack_required: bool,
}

/// Posts a message to a thread the caller may read: its author, its recipients, or anyone in the
/// project for a project-addressed thread.
pub struct PostMessage {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
}

impl PostMessage {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Posts the message, answering it with its author's handle.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a thread that does not
    /// exist or that the caller may not read; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: PostMessageInput,
    ) -> Result<NamedMessage, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let thread = readable_thread(&mut work, actor, input.thread).await?;
        let message = MessageRecord {
            id: MessageId(self.ids.next()),
            thread: thread.id,
            author: actor.member,
            label: input.label,
            body: input.body,
            importance: input.importance,
            ack_required: input.ack_required,
            created_at: now,
        };
        work.insert_message(actor.project, &message).await?;
        work.record(
            now,
            EventKind::MessagePosted(MessagePosted {
                thread: thread.id,
                message: message.id,
                author: actor.member,
            }),
        )
        .await?;
        let posted = Names::load(&mut work).await?.message(message)?;
        work.commit(&*self.notifier).await?;
        Ok(posted)
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::Subject;

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        Addressees, CreateMember, CreateMemberInput, StartThread, StartThreadInput,
    };

    fn label() -> SessionLabel {
        SessionLabel::parse("web").unwrap()
    }

    #[tokio::test]
    async fn a_recipient_replies_and_an_outsider_cannot() {
        let env = Env::new().await;
        let carol = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: crate::use_cases::testing::new_member("carol"),
                    role: dibs_domain::member::Role::Member,
                },
            )
            .await
            .unwrap();
        let carol = crate::use_cases::Authenticate::new(&env.deps)
            .execute(carol.token.expose())
            .await
            .unwrap();
        let started = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse("Billing").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                    item: None,
                    label: label(),
                    body: Body::parse("Can you look?").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        let post = PostMessage::new(&env.deps);
        let before = env.events().await.len();
        let reply = post
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread: started.thread.record.id,
                    label: label(),
                    body: Body::parse("On it.").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::MessagePosted(MessagePosted {
                thread: started.thread.record.id,
                message: reply.record.id,
                author: env.bob.actor.member,
            })]
        );
        assert_eq!(reply.author.as_str(), "bob");
        let outsider = post
            .execute(
                &carol,
                PostMessageInput {
                    thread: started.thread.record.id,
                    label: label(),
                    body: Body::parse("Me too").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(outsider.code(), "not_found");
        let missing = post
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread: ThreadId(9999),
                    label: label(),
                    body: Body::parse("Hello").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(missing.code(), "not_found");
        let integration = post
            .execute(
                &env.importer,
                PostMessageInput {
                    thread: started.thread.record.id,
                    label: label(),
                    body: Body::parse("Hello").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(integration.code(), "forbidden");
    }
}
