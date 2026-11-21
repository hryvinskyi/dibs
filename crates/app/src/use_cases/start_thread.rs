//! Starting a conversation.

use std::collections::BTreeSet;
use std::sync::Arc;

use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ItemId;
use dibs_domain::message::{Body, Importance, Recipients, SessionLabel, Subject};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedMessage, NamedThread, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::{existing_item, present_member_by_handle};
use crate::use_cases::no_session::{NoSessionAtWork, no_session_at_work};
use crate::use_cases::writing::{MemberThread, Posted};
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

/// The new thread and its first message, their members named, with the recipients no session of
/// whose will show it soon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedThread {
    /// The thread.
    pub thread: NamedThread,
    /// Its first message.
    pub message: NamedMessage,
    /// The members it names none of whose sessions was seen within the last hour, ordered by
    /// handle; nobody for a thread addressed to the whole project. The message is sent all the
    /// same and waits for their next session.
    pub no_session_at_work: Vec<NoSessionAtWork>,
}

/// Starts a thread with its first message, and says which members it names have no session at
/// work. Only people write, and only people receive: naming an integration as a recipient is
/// refused.
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
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let recipients = match input.to {
            Addressees::Project => Recipients::Project,
            Addressees::Members(handles) => {
                let mut members = BTreeSet::new();
                for handle in handles {
                    let member = present_member_by_handle(&mut work, &handle).await?;
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
        let settings = work.project_settings(&self.hot_rules).await?;
        let limit = settings.raised_per_hour();
        let (thread, message) = MemberThread {
            ids: Posted::next(&*self.ids),
            subject: input.subject,
            item: input.item,
            author: actor.member,
            to: recipients,
            label: input.label,
            body: input.body,
            importance: input.importance,
            ack_required: input.ack_required,
        }
        .start(&mut work, limit, now)
        .await?;
        let names = Names::load(&mut work).await?;
        let no_session_at_work =
            no_session_at_work(&mut work, &thread, &message, &settings, &names, now).await?;
        let started = StartedThread {
            thread: names.thread(thread)?,
            message: names.message(message)?,
            no_session_at_work,
        };
        work.commit(&*self.notifier).await?;
        Ok(started)
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::event::{EventKind, MessagePosted, ThreadStarted};

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
                    author: Some(env.alice.actor.member),
                    item: Some(item),
                }),
                EventKind::MessagePosted(MessagePosted {
                    thread: started.thread.record.id,
                    message: started.message.record.id,
                    author: Some(env.alice.actor.member),
                }),
            ]
        );
        let json = serde_json::to_string(&kinds).unwrap();
        assert!(!json.contains("Secret plans"));
        assert!(!json.contains("Invoice totals"));
    }

    #[tokio::test]
    async fn the_answer_names_each_recipient_with_no_session_seen_in_the_hour_and_their_live_claim()
    {
        use crate::use_cases::testing::{new_member, session};
        use crate::use_cases::{Authenticate, ClaimItem, ClaimItemInput, CreateMember};

        let env = Env::new().await;
        let carol = CreateMember::new(&env.deps)
            .execute(
                &env.alice.actor,
                crate::use_cases::CreateMemberInput {
                    member: new_member("carol"),
                    role: dibs_domain::member::Role::Member,
                },
            )
            .await
            .unwrap();
        Authenticate::new(&env.deps)
            .execute(carol.token.expose())
            .await
            .unwrap();
        let item = env.item("gh:acme/widgets#1").await;
        ClaimItem::new(&env.deps)
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    branches: Default::default(),
                    item,
                    session: env.bob.session,
                    areas: vec!["billing".to_owned()],
                    wide: None,
                    kind: None,
                },
            )
            .await
            .unwrap();
        let start = StartThread::new(&env.deps);
        let named = |started: StartedThread| {
            started
                .no_session_at_work
                .into_iter()
                .map(|away| (away.member.as_str().to_owned(), away.holds_live_claim))
                .collect::<Vec<_>>()
        };
        let both = || handles(&["carol", "bob"]);

        let at_once = start
            .execute(&env.alice.actor, input(both(), "hello"))
            .await
            .unwrap();
        assert_eq!(
            named(at_once),
            [("carol".to_owned(), false)],
            "bob's session registered just now"
        );

        env.advance(time::Duration::minutes(61));
        let later = start
            .execute(&env.alice.actor, input(both(), "hello again"))
            .await
            .unwrap();
        assert_eq!(later.message.record.body.as_str(), "hello again", "sent");
        assert_eq!(
            named(later),
            [("bob".to_owned(), true), ("carol".to_owned(), false)]
        );
        let everyone = start
            .execute(&env.alice.actor, input(Addressees::Project, "noon"))
            .await
            .unwrap();
        assert!(
            everyone.no_session_at_work.is_empty(),
            "a conversation with the whole project names nobody"
        );

        session(&env.deps, &env.bob.actor, "desktop", "desktop/77aa01bc").await;
        env.advance(time::Duration::minutes(10));
        let seen = start
            .execute(&env.alice.actor, input(both(), "and again"))
            .await
            .unwrap();
        assert_eq!(
            named(seen),
            [("carol".to_owned(), false)],
            "a session of bob's was seen ten minutes ago"
        );
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
