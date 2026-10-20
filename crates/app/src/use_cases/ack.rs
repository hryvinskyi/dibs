//! Acknowledging a message.

use std::sync::Arc;

use dibs_domain::event::{Acked, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{MessageId, ThreadId};
use dibs_domain::message::{Ack, AckOutcome, AckState, decide_ack};
use dibs_domain::wide::ask_stands;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::named::{NamedGrant, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::records::{MessageRecord, ThreadRecord};
use crate::use_cases::acks::{people, reached};
use crate::use_cases::readable::may_read_thread;
use crate::use_cases::wide_asks::grant_by_ack;
use crate::work::Work;

/// The caller's acknowledgement of a message, with the thread of the message and the handle of the
/// member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acknowledged {
    /// The acknowledgement that stands: the new one, or the caller's first.
    pub ack: Ack,
    /// Whether this call recorded it, rather than finding it recorded.
    pub created: bool,
    /// The thread of the message.
    pub thread: ThreadId,
    /// The handle of `ack.member`, the caller.
    pub member: Handle,
    /// The grants this acknowledgement made: for the announcement of a wide claim, one per area it
    /// asked about that the caller still holds. Empty for any other message and for a repeat.
    pub granted: Vec<NamedGrant>,
    /// Whether the message announced a wide claim that has ended, so the acknowledgement grants
    /// nothing.
    pub announcement_ended: bool,
}

/// Records that the caller acknowledged a message. Only a message that asks for it, and only by one
/// of its recipients — never its author. A recipient acknowledges once: asking again answers their
/// first acknowledgement and records nothing, so a retry after a lost answer is harmless. The
/// message itself stays as it was written.
///
/// Acknowledging a wide claim's announcement is the holder's grant: in the same transaction, the
/// wide claim's member is granted edits, for the wide claim's item, in each area it asked about
/// that the holder still holds through a live claim, while the wide claim is live.
pub struct AckMessage {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl AckMessage {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// Acknowledges the message.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for a message that does
    /// not exist or is in a thread the caller may not read; [`AppError::Ack`] when the message asks
    /// for no acknowledgement or the caller is not one of its recipients; [`AppError::Store`] when
    /// the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        message: MessageId,
    ) -> Result<Acknowledged, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let (message, thread) = readable_message(&mut work, actor, message).await?;
        let people = people(&mut work).await?;
        let acks = work.acks(actor.project, message.id).await?;
        let state = AckState::derive(&reached(&thread, &message, &people), &acks);
        let outcome = decide_ack(message.id, message.ack_required, &state, actor.member, now)?;
        let member = Names::load(&mut work).await?.handle(actor.member)?;
        let announcement_ended = match work.wide_ask(actor.project, message.id).await? {
            Some(ask) => !ask_stands(&ask, &work.claims(actor.project).await?),
            None => false,
        };
        let mut granted = Vec::new();
        let created = match outcome {
            AckOutcome::Already(_) => false,
            AckOutcome::New(ack) => {
                work.insert_ack(actor.project, &ack).await?;
                work.record(
                    now,
                    EventKind::Acked(Acked {
                        thread: thread.id,
                        message: message.id,
                        by: actor.member,
                    }),
                )
                .await?;
                let settings = work.project_settings(&self.hot_rules).await?;
                granted = grant_by_ack(&mut work, &*self.ids, message.id, &settings, now).await?;
                work.commit(&*self.notifier).await?;
                true
            }
        };
        Ok(Acknowledged {
            ack: outcome.ack(),
            created,
            thread: thread.id,
            member,
            granted,
            announcement_ended,
        })
    }
}

/// The message and its thread, when the actor may read the thread. A message the actor may not
/// read is reported as missing, so the answer never confirms that it exists.
pub(crate) async fn readable_message(
    work: &mut Work,
    actor: &Actor,
    id: MessageId,
) -> Result<(MessageRecord, ThreadRecord), AppError> {
    let project = work.project_id();
    let missing = || AppError::NotFound(Missing::Message(id));
    let message = work.message(project, id).await?.ok_or_else(missing)?;
    let thread = work
        .thread(project, message.thread)
        .await?
        .filter(|thread| may_read_thread(actor, thread))
        .ok_or_else(missing)?;
    Ok((message, thread))
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, Importance, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        Addressees, CreateMember, CreateMemberInput, PostMessage, PostMessageInput, StartThread,
        StartThreadInput,
    };

    pub(crate) fn asking(to: Addressees, ack_required: bool) -> StartThreadInput {
        StartThreadInput {
            subject: Subject::parse("Staging is down").unwrap(),
            to,
            item: None,
            label: SessionLabel::parse("laptop/mcp").unwrap(),
            body: Body::parse("Do not deploy until I say so.").unwrap(),
            importance: Importance::Urgent,
            ack_required,
        }
    }

    fn to(handles: &[&str]) -> Addressees {
        Addressees::Members(handles.iter().map(|h| Handle::parse(h).unwrap()).collect())
    }

    #[tokio::test]
    async fn a_recipient_acks_once_and_a_repeat_answers_the_first_ack() {
        let env = Env::new().await;
        let started = StartThread::new(&env.deps)
            .execute(&env.alice.actor, asking(to(&["bob"]), true))
            .await
            .unwrap();
        let message = started.message.record.id;
        env.advance(time::Duration::minutes(3));
        let before = env.events().await.len();
        let ack = AckMessage::new(&env.deps);
        let acked = ack.execute(&env.bob.actor, message).await.unwrap();
        assert_eq!(acked.ack.member, env.bob.actor.member);
        assert_eq!(acked.ack.message, message);
        assert_eq!(acked.ack.at, env.now());
        assert_eq!(acked.member.as_str(), "bob");
        assert_eq!(acked.thread, started.thread.record.id);
        assert_eq!(
            env.kinds_after(before).await,
            [EventKind::Acked(Acked {
                thread: started.thread.record.id,
                message,
                by: env.bob.actor.member,
            })]
        );
        assert!(acked.created);
        env.advance(time::Duration::minutes(5));
        let again = ack.execute(&env.bob.actor, message).await.unwrap();
        assert!(!again.created, "a repeat records nothing");
        assert_eq!(
            again.ack, acked.ack,
            "the first acknowledgement stands, with its time"
        );
        assert_eq!(
            env.events().await.len(),
            before + 1,
            "a repeat appends nothing"
        );
    }

    #[tokio::test]
    async fn only_a_recipient_of_a_message_that_asks_may_ack_it() {
        let env = Env::new().await;
        let start = StartThread::new(&env.deps);
        let ack = AckMessage::new(&env.deps);
        let asked = start
            .execute(&env.alice.actor, asking(to(&["bob"]), true))
            .await
            .unwrap();
        let plain = start
            .execute(&env.alice.actor, asking(to(&["bob"]), false))
            .await
            .unwrap();
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
        let id = asked.message.record.id;
        let cases = [
            (env.alice.actor.clone(), id, "not_a_recipient"),
            (carol, id, "not_found"),
            (env.importer.clone(), id, "forbidden"),
            (
                env.bob.actor.clone(),
                plain.message.record.id,
                "ack_not_required",
            ),
            (env.bob.actor.clone(), MessageId(9_999), "not_found"),
        ];
        for (actor, message, code) in cases {
            assert_eq!(
                ack.execute(&actor, message).await.unwrap_err().code(),
                code,
                "{code}"
            );
        }
        let reply = PostMessage::new(&env.deps)
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread: asked.thread.record.id,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Understood; who decides?").unwrap(),
                    importance: Importance::Normal,
                    ack_required: true,
                },
            )
            .await
            .unwrap();
        assert!(
            ack.execute(&env.alice.actor, reply.record.id).await.is_ok(),
            "a reply reaches the thread's author"
        );
    }

    #[tokio::test]
    async fn a_project_message_reaches_the_people_who_were_there_when_it_was_written() {
        let env = Env::new().await;
        let started = StartThread::new(&env.deps)
            .execute(&env.alice.actor, asking(Addressees::Project, true))
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        let late = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                CreateMemberInput {
                    member: crate::use_cases::testing::new_member("dave"),
                    role: dibs_domain::member::Role::Member,
                },
            )
            .await
            .unwrap();
        let late = crate::use_cases::Authenticate::new(&env.deps)
            .execute(late.token.expose())
            .await
            .unwrap();
        let ack = AckMessage::new(&env.deps);
        let id = started.message.record.id;
        assert!(ack.execute(&env.bob.actor, id).await.is_ok());
        assert_eq!(
            ack.execute(&late, id).await.unwrap_err().code(),
            "not_a_recipient"
        );
    }
}
