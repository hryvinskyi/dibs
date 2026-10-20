//! Starting a conversation.

use std::collections::BTreeSet;
use std::sync::Arc;

use dibs_domain::event::{EventKind, MessagePosted, ThreadStarted};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, MessageId, ThreadId};
use dibs_domain::message::{Body, Importance, Recipients, SessionLabel, Subject};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedMessage, NamedThread, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::records::{MessageRecord, ThreadRecord};
use crate::use_cases::common::{existing_item, member_by_handle};
use crate::use_cases::writing::{Authorship, write_message};
use crate::work::Work;

/// Whom a new thread is addressed to, as the caller names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addressees {
    /// Every person in the project.
    Project,
    /// These members, by handle.
    Members(Vec<Handle>),
}

/// The thread to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartThreadInput {
    /// Its subject.
    pub subject: Subject,
    /// Whom it is addressed to.
    pub to: Addressees,
    /// The work item it is about, if any.
    pub item: Option<ItemId>,
    /// The label of the session writing (`web` for the browser).
    pub label: SessionLabel,
    /// The first message.
    pub body: Body,
    /// How much the first message asks for its recipients' attention.
    pub importance: Importance,
    /// Whether each recipient is asked to acknowledge the first message.
    pub ack_required: bool,
}

/// The new thread and its first message, their members named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedThread {
    /// The thread.
    pub thread: NamedThread,
    /// Its first message.
    pub message: NamedMessage,
}

/// Starts a thread with its first message. Only people write, and only people receive: naming an
/// integration as a recipient is refused.
pub struct StartThread {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl StartThread {
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

    /// Starts the thread.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown recipient
    /// or item; [`AppError::NotARecipient`] for an integration named as recipient;
    /// [`AppError::Recipients`] when no member is named; [`AppError::ImportanceLimited`] for a high
    /// or urgent message past the project's hourly limit; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: StartThreadInput,
    ) -> Result<StartedThread, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin(&*self.store, project).await?;
        let recipients = match input.to {
            Addressees::Project => Recipients::Project,
            Addressees::Members(handles) => {
                let mut members = BTreeSet::new();
                for handle in handles {
                    let member = member_by_handle(&mut work, &handle).await?;
                    if !member.role.is_person() {
                        return Err(AppError::NotARecipient(handle));
                    }
                    members.insert(member.id);
                }
                Recipients::members(members)?
            }
        };
        if let Some(item) = input.item {
            existing_item(&mut work, item).await?;
        }
        let thread = ThreadRecord {
            id: ThreadId(self.ids.next()),
            subject: input.subject,
            item: input.item,
            author: actor.member,
            recipients,
            created_at: now,
        };
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
        let limit = work
            .project_settings(&self.hot_rules)
            .await?
            .raised_per_hour();
        work.insert_thread(project, &thread).await?;
        write_message(&mut work, &message, Authorship::Member { limit }, now).await?;
        work.record(
            now,
            EventKind::ThreadStarted(ThreadStarted {
                thread: thread.id,
                author: actor.member,
                item: thread.item,
            }),
        )
        .await?;
        work.record(
            now,
            EventKind::MessagePosted(MessagePosted {
                thread: thread.id,
                message: message.id,
                author: actor.member,
            }),
        )
        .await?;
        let names = Names::load(&mut work).await?;
        let started = StartedThread {
            thread: names.thread(thread)?,
            message: names.message(message)?,
        };
        work.commit(&*self.notifier).await?;
        Ok(started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;

    pub(crate) fn input(to: Addressees, body: &str) -> StartThreadInput {
        StartThreadInput {
            subject: Subject::parse("Invoice totals").unwrap(),
            to,
            item: None,
            label: SessionLabel::parse("laptop/4f2a91c0").unwrap(),
            body: Body::parse(body).unwrap(),
            importance: Importance::Normal,
            ack_required: false,
        }
    }

    fn handles(names: &[&str]) -> Addressees {
        Addressees::Members(names.iter().map(|n| Handle::parse(n).unwrap()).collect())
    }

    #[tokio::test]
    async fn a_thread_starts_and_its_events_carry_no_content() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let before = env.events().await.len();
        let started = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    item: Some(item),
                    ..input(handles(&["bob"]), "Secret plans for the invoice fix")
                },
            )
            .await
            .unwrap();
        assert_eq!(
            started.thread.record.recipients,
            Recipients::Members([env.bob.actor.member].into())
        );
        assert_eq!(started.thread.author.as_str(), "alice");
        assert_eq!(started.thread.recipients, [Handle::parse("bob").unwrap()]);
        assert_eq!(started.message.author.as_str(), "alice");
        let kinds = env.kinds_after(before).await;
        assert_eq!(
            kinds,
            [
                EventKind::ThreadStarted(ThreadStarted {
                    thread: started.thread.record.id,
                    author: env.alice.actor.member,
                    item: Some(item),
                }),
                EventKind::MessagePosted(MessagePosted {
                    thread: started.thread.record.id,
                    message: started.message.record.id,
                    author: env.alice.actor.member,
                }),
            ]
        );
        let json = serde_json::to_string(&kinds).unwrap();
        assert!(!json.contains("Secret plans"));
        assert!(!json.contains("Invoice totals"));
    }

    #[tokio::test]
    async fn recipients_are_people_of_the_project() {
        let env = Env::new().await;
        let start = StartThread::new(&env.deps);
        let cases = [
            (
                env.alice.actor.clone(),
                handles(&["importer"]),
                "not_a_recipient",
            ),
            (env.alice.actor.clone(), handles(&["nobody"]), "not_found"),
            (env.alice.actor.clone(), handles(&[]), "no_recipients"),
            (env.importer.clone(), Addressees::Project, "forbidden"),
        ];
        for (actor, to, code) in cases {
            let error = start.execute(&actor, input(to, "hello")).await.unwrap_err();
            assert_eq!(error.code(), code);
        }
        let unknown_item = start
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    item: Some(ItemId(9999)),
                    ..input(Addressees::Project, "hello")
                },
            )
            .await
            .unwrap_err();
        assert_eq!(unknown_item.code(), "not_found");
        assert!(
            env.events()
                .await
                .iter()
                .all(|e| !matches!(e.kind, EventKind::ThreadStarted(_)))
        );
    }
}
