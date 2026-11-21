//! The threads waiting for the caller, and the messages waiting for their acknowledgement.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use time::{Duration, OffsetDateTime};

use dibs_domain::area::QualifiedArea;
use dibs_domain::delivery::DeliverySubject;
use dibs_domain::grant::AskedGrant;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, MessageId, ThreadId};
use dibs_domain::message::{Recipients, handed_to_sessions, speaks_for};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::ProjectSettings;
use dibs_domain::wide::ask_stands;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::slot::SlotName;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{ItemName, NamedMessage, NamedSummary, NamedThread, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::records::{MessageRecord, ThreadRecord, ThreadSummary};
use crate::use_cases::acks::{awaiting, awaiting_per_thread, people, reached};
use crate::use_cases::common::existing_item;
use crate::use_cases::readable::readable_summaries;
use crate::use_cases::slots::Scene;
use crate::use_cases::unacked::{UnackedMessage, unacked};
use crate::work::Work;

/// What the inbox is asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboxQuery {
    /// Only the threads with unread messages.
    pub unread_only: bool,
    /// The messages awaiting the caller's acknowledgement too.
    pub unacked: bool,
    /// The messages that may still be delivered to the caller's agent while it works.
    pub deliverable: bool,
}

/// The caller's inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxView {
    /// The threads, newest activity first.
    pub threads: Vec<NamedSummary>,
    /// The messages awaiting the caller's acknowledgement, newest first, when they were asked for.
    pub unacked: Option<Vec<UnackedMessage>>,
    /// The slots on offer to the caller now, ordered by slot.
    pub offers: Vec<PendingOffer>,
    /// The messages that may still be delivered to the caller's agent, newest first, when they
    /// were asked for.
    pub deliverable: Option<Vec<DeliverableMessage>>,
    /// The grants asked of the caller that wait for their decision, newest first, at most
    /// [`MAX_GRANT_ASKS`]: the list an agent is handed, the beginning of `every_grant_ask`.
    pub grant_asks: Vec<PendingGrantAsk>,
    /// Every grant asked of the caller that waits for their decision, newest first, however many
    /// there are: a conversation holding one is a decision, never an unread conversation.
    pub every_grant_ask: Vec<PendingGrantAsk>,
    /// The threads the caller may read with messages awaiting their acknowledgement, newest
    /// activity first, read or not: `unread_only` never leaves one out.
    pub awaiting_ack: Vec<NamedSummary>,
    /// How many of the threads the caller may read have unread messages from people and from
    /// dibs, whatever `unread_only` leaves out of `threads`.
    pub unread_conversations: UnreadConversations,
}

/// How many conversations have messages the reader has not read, by who wrote them: a member, or
/// dibs. A conversation with unread messages of both is counted in both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadConversations {
    /// The conversations with unread messages a member wrote.
    pub from_people: u64,
    /// The conversations with unread messages dibs wrote.
    pub from_dibs: u64,
}

impl UnreadConversations {
    /// The counts over `summaries`.
    pub fn of<'a>(summaries: impl IntoIterator<Item = &'a ThreadSummary>) -> Self {
        summaries
            .into_iter()
            .fold(Self::default(), |counted, summary| Self {
                from_people: counted.from_people + u64::from(summary.unread_from_people > 0),
                from_dibs: counted.from_dibs + u64::from(summary.unread_from_dibs > 0),
            })
    }
}

/// A grant asked of the caller that waits for their decision: asked in a conversation they have not
/// answered, and still asking them, as [`AskedGrant::waits_on`] decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingGrantAsk {
    /// The conversation that asks.
    pub thread: ThreadId,
    /// The message that asked.
    pub message: MessageId,
    /// Who asks, and would be the grantee.
    pub asker: Handle,
    /// The asker's item, the grant's item.
    pub item: ItemName,
    /// The area.
    pub area: QualifiedArea,
    /// The item of the caller's claim that holds the area.
    pub holder_item: ItemName,
    /// When it was asked.
    pub asked_at: OffsetDateTime,
}

/// The most grant requests one inbox answer lists.
pub const MAX_GRANT_ASKS: usize = 10;

/// A message that should reach the caller's agent while it works and has not reached any of their
/// sessions yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverableMessage {
    /// The thread, its members named.
    pub thread: NamedThread,
    /// The message, its author named.
    pub message: NamedMessage,
    /// Whether it awaits the caller's acknowledgement.
    pub awaiting_ack: bool,
    /// Whether the caller's acknowledgement grants something: it is the announcement of a wide
    /// claim, whose acknowledgement grants edits.
    pub ack_grants: bool,
}

/// How old a message may be and still be delivered while the member works; older ones are left
/// to the brief and the inbox.
pub const DELIVERABLE_FOR: Duration = Duration::hours(24);

/// The most messages one inbox answer offers for delivery.
pub const MAX_DELIVERABLE: usize = 20;

/// A slot offered to the caller, which is theirs if they take it before `until`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOffer {
    /// The slot.
    pub slot: SlotName,
    /// When the offer lapses.
    pub until: OffsetDateTime,
}

/// Lists the threads the caller may read, newest activity first, with what they have not read and
/// how many messages await their acknowledgement; only the ones with unread messages when asked;
/// when asked, the messages awaiting their acknowledgement and the messages still to be delivered
/// to their agent; the slots on offer to them, after applying and recording what time changed
/// about the slots; and the grants asked of them that wait for their decision — all in one
/// transaction.
pub struct Inbox {
    ref_schemes: Arc<RefSchemeRegistry>,
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl Inbox {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            ref_schemes: deps.ref_schemes.clone(),
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// The caller's inbox.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor, query: InboxQuery) -> Result<InboxView, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        scene.save(&mut work, &*self.ids, now).await?;
        let offers = scene
            .board
            .offers
            .iter()
            .filter(|offer| offer.member == actor.member)
            .map(|offer| PendingOffer {
                slot: offer.slot.clone(),
                until: offer.until,
            })
            .collect();
        let summaries = work
            .thread_summaries(actor.project, actor.member, None)
            .await?;
        let waiting = awaiting_per_thread(&awaiting(&mut work, actor).await?);
        let names = Names::load(&mut work).await?;
        let readable = readable_summaries(actor, summaries);
        let unread_conversations = UnreadConversations::of(&readable);
        let awaiting_count =
            |summary: &ThreadSummary| waiting.get(&summary.thread.id).copied().unwrap_or(0);
        let threads = readable
            .iter()
            .filter(|summary| !query.unread_only || summary.unread > 0)
            .map(|summary| names.summary(summary.clone(), awaiting_count(summary)))
            .collect::<Result<_, _>>()?;
        let awaiting_ack = readable
            .iter()
            .filter(|summary| awaiting_count(summary) > 0)
            .map(|summary| names.summary(summary.clone(), awaiting_count(summary)))
            .collect::<Result<_, _>>()?;
        let unacked = if query.unacked {
            Some(unacked(&mut work, actor).await?)
        } else {
            None
        };
        let deliverable = if query.deliverable {
            Some(deliverable(&mut work, actor, &readable, &names, now).await?)
        } else {
            None
        };
        let every_grant_ask = self
            .every_grant_ask(&mut work, actor, &scene.settings, &names, now)
            .await?;
        work.commit(&*self.notifier).await?;
        Ok(InboxView {
            threads,
            unacked,
            offers,
            deliverable,
            grant_asks: every_grant_ask
                .iter()
                .take(MAX_GRANT_ASKS)
                .cloned()
                .collect(),
            every_grant_ask,
            awaiting_ack,
            unread_conversations,
        })
    }

    /// Every grant asked of the actor that waits for their decision, newest first: asked in a
    /// conversation they wrote nothing in, and still waiting by the claims and grants of now.
    async fn every_grant_ask(
        &self,
        work: &mut Work,
        actor: &Actor,
        settings: &ProjectSettings,
        names: &Names,
        now: OffsetDateTime,
    ) -> Result<Vec<PendingGrantAsk>, AppError> {
        let project = work.project_id();
        let asked = work.unanswered_asked_grants(project, actor.member).await?;
        if asked.is_empty() {
            return Ok(Vec::new());
        }
        let claims = work.claims(project).await?;
        let grants: Vec<_> = work
            .grants(project)
            .await?
            .into_iter()
            .map(|record| record.grant)
            .collect();
        let mut waiting: Vec<(MessageRecord, AskedGrant, ItemId)> = Vec::new();
        for request in asked {
            let Some(holder_item) = request.waits_on(&grants, &claims, now, settings) else {
                continue;
            };
            let message = work
                .message(project, request.message)
                .await?
                .ok_or_else(|| {
                    AppError::Store(StoreError::Corrupt {
                        what: format!(
                            "a grant request names message {}, which does not exist",
                            request.message
                        ),
                    })
                })?;
            waiting.push((message, request, holder_item));
        }
        waiting.sort_by_key(|(message, _, _)| std::cmp::Reverse((message.created_at, message.id)));
        let mut listed = Vec::new();
        for (message, request, holder_item) in waiting {
            let item = existing_item(work, request.ask.item).await?;
            let holder_item = existing_item(work, holder_item).await?;
            listed.push(PendingGrantAsk {
                thread: message.thread,
                message: message.id,
                asker: names.handle(request.ask.asker)?,
                item: ItemName::of(&item, &self.ref_schemes),
                area: request.ask.area,
                holder_item: ItemName::of(&holder_item, &self.ref_schemes),
                asked_at: message.created_at,
            });
        }
        Ok(listed)
    }
}

/// The messages still to be delivered to the actor's agent, newest first: written by someone else
/// within [`DELIVERABLE_FOR`], reaching the actor, and either awaiting their acknowledgement or
/// unread and handed to sessions by what it is ([`handed_to_sessions`]: a member's message in a
/// thread addressed to members at any importance, any other only when raised) — leaving out every
/// announcement of a wide claim that has ended and every message already delivered to one of their
/// sessions. Only the threads that may hold such a message are opened: one with unread messages
/// whose highest importance is raised, or one addressed to members with unread messages a member
/// wrote, so the notices dibs wrote at normal importance cost nothing however many are unread.
async fn deliverable(
    work: &mut Work,
    actor: &Actor,
    readable: &[ThreadSummary],
    names: &Names,
    now: OffsetDateTime,
) -> Result<Vec<DeliverableMessage>, AppError> {
    let project = work.project_id();
    let recent = |message: &MessageRecord| now - message.created_at < DELIVERABLE_FOR;
    let mut found: BTreeMap<MessageId, (MessageRecord, ThreadRecord, bool)> = BTreeMap::new();
    for (message, thread) in awaiting(work, actor).await? {
        if recent(&message) {
            found.insert(message.id, (message, thread, true));
        }
    }
    let people = people(work).await?;
    let mut claims = None;
    for summary in readable.iter().filter(|summary| may_hand_over(summary)) {
        for message in work.messages(project, summary.thread.id).await? {
            let handed = handed_to_sessions(
                &summary.thread.recipients,
                message.author,
                message.importance,
            ) && speaks_for(summary.thread.author, message.author)
                != Some(actor.member)
                && recent(&message)
                && !found.contains_key(&message.id)
                && reached(&summary.thread, &message, &people).contains(&actor.member);
            if !handed {
                continue;
            }
            let read = work
                .reads_of(project, message.id)
                .await?
                .iter()
                .any(|mark| mark.member == actor.member);
            if read {
                continue;
            }
            if let Some(ask) = work.wide_ask(project, message.id).await? {
                if claims.is_none() {
                    claims = Some(work.claims(project).await?);
                }
                if !claims
                    .as_deref()
                    .is_some_and(|claims| ask_stands(&ask, claims))
                {
                    continue;
                }
            }
            found.insert(message.id, (message, summary.thread.clone(), false));
        }
    }
    let subjects: Vec<DeliverySubject> = found
        .keys()
        .map(|id| DeliverySubject::Message(*id))
        .collect();
    let delivered: BTreeSet<DeliverySubject> = work
        .deliveries(project, actor.member, &subjects)
        .await?
        .into_iter()
        .map(|delivery| delivery.subject)
        .collect();
    let mut left: Vec<(MessageRecord, ThreadRecord, bool)> = found
        .into_values()
        .filter(|(message, _, _)| !delivered.contains(&DeliverySubject::Message(message.id)))
        .collect();
    left.sort_by_key(|(message, _, _)| std::cmp::Reverse((message.created_at, message.id)));
    let mut answered = Vec::new();
    for (message, thread, awaiting_ack) in left.into_iter().take(MAX_DELIVERABLE) {
        let ack_grants = work.wide_ask(project, message.id).await?.is_some();
        answered.push(DeliverableMessage {
            thread: names.thread(thread)?,
            message: names.message(message)?,
            awaiting_ack,
            ack_grants,
        });
    }
    Ok(answered)
}

/// Whether a thread may hold an unread message [`handed_to_sessions`] hands over: unread messages
/// with a raised one among its messages, or, in a thread addressed to members, unread messages a
/// member wrote.
fn may_hand_over(summary: &ThreadSummary) -> bool {
    let raised = summary.unread > 0 && summary.importance.is_raised();
    let from_people_to_members = summary.unread_from_people > 0
        && matches!(summary.thread.recipients, Recipients::Members(_));
    raised || from_people_to_members
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use dibs_domain::message::Importance;

    use crate::actor::Actor;
    use crate::use_cases::{AckMessage, Addressees, MarkRead, StartThread, StartThreadInput};
    use dibs_domain::ids::ThreadId;

    fn input(to: Addressees, subject: &str) -> StartThreadInput {
        StartThreadInput {
            subject: Subject::parse(subject).unwrap(),
            to,
            item: None,
            label: SessionLabel::parse("web").unwrap(),
            body: Body::parse("hello").unwrap(),
            importance: Importance::Normal,
            ack_required: false,
        }
    }

    #[tokio::test]
    async fn the_inbox_holds_what_the_caller_may_read_and_counts_the_unread() {
        let env = Env::new().await;
        let start = StartThread::new(&env.deps);
        let to_bob = start
            .execute(
                &env.alice.actor,
                input(
                    Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                    "To bob",
                ),
            )
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        let to_all = start
            .execute(&env.alice.actor, input(Addressees::Project, "To all"))
            .await
            .unwrap();
        env.advance(time::Duration::minutes(1));
        start
            .execute(
                &env.bob.actor,
                input(
                    Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    "Bob writes",
                ),
            )
            .await
            .unwrap();
        let inbox = Inbox::new(&env.deps);
        let bobs = inbox
            .execute(&env.bob.actor, InboxQuery::default())
            .await
            .unwrap()
            .threads;
        let subjects: Vec<&str> = bobs
            .iter()
            .map(|s| s.thread.record.subject.as_str())
            .collect();
        assert_eq!(subjects, ["Bob writes", "To all", "To bob"]);
        let unread: Vec<u64> = bobs.iter().map(|s| s.unread).collect();
        assert_eq!(unread, [0, 1, 1]);
        MarkRead::new(&env.deps)
            .execute(&env.bob.actor, to_bob.thread.record.id)
            .await
            .unwrap();
        let waiting = inbox
            .execute(
                &env.bob.actor,
                InboxQuery {
                    unread_only: true,
                    unacked: false,
                    deliverable: false,
                },
            )
            .await
            .unwrap()
            .threads;
        let ids: Vec<_> = waiting.iter().map(|s| s.thread.record.id).collect();
        assert_eq!(ids, [to_all.thread.record.id]);
        let authors: Vec<&str> = bobs.iter().map(|s| s.thread.author.as_str()).collect();
        assert_eq!(authors, ["bob", "alice", "alice"]);
        assert_eq!(bobs[0].thread.recipients, [Handle::parse("alice").unwrap()]);
        assert!(bobs[1].thread.recipients.is_empty());
        assert_eq!(
            inbox
                .execute(&env.importer, InboxQuery::default())
                .await
                .unwrap_err()
                .code(),
            "forbidden"
        );
    }

    /// Stores a conversation dibs started for alice about nothing, with one message of its own.
    async fn notice_to_alice(env: &Env, id: u64) {
        let thread = ThreadRecord {
            id: ThreadId(id),
            subject: Subject::parse(&format!("Notice {id}")).unwrap(),
            item: None,
            author: dibs_domain::message::Author::System,
            recipients: dibs_domain::message::Recipients::Members(
                [env.alice.actor.member].into_iter().collect(),
            ),
            created_at: env.now(),
        };
        let message = MessageRecord {
            id: MessageId(id),
            thread: thread.id,
            author: dibs_domain::message::Author::System,
            label: SessionLabel::parse("dibs").unwrap(),
            body: Body::parse("Something happened to your claim.").unwrap(),
            importance: Importance::Normal,
            ack_required: false,
            created_at: env.now(),
        };
        let mut tx = env.deps.store.begin().await.unwrap();
        tx.insert_thread(env.project, &thread).await.unwrap();
        tx.insert_message(env.project, &message).await.unwrap();
        tx.commit().await.unwrap();
    }

    #[tokio::test]
    async fn the_inbox_counts_the_conversations_unread_from_people_apart_from_those_from_dibs() {
        let env = Env::new().await;
        let start = StartThread::new(&env.deps);
        let to_alice = || Addressees::Members(vec![Handle::parse("alice").unwrap()]);
        for subject in ["Rates", "Staging"] {
            start
                .execute(&env.bob.actor, input(to_alice(), subject))
                .await
                .unwrap();
            env.advance(time::Duration::minutes(1));
        }
        for id in [9001, 9002, 9003] {
            notice_to_alice(&env, id).await;
            env.advance(time::Duration::minutes(1));
        }
        let inbox = Inbox::new(&env.deps);
        let asked = InboxQuery {
            unread_only: true,
            unacked: false,
            deliverable: false,
        };
        let alices = inbox.execute(&env.alice.actor, asked).await.unwrap();
        assert_eq!(
            alices.unread_conversations,
            UnreadConversations {
                from_people: 2,
                from_dibs: 3,
            }
        );
        let split: Vec<(&str, u64, u64, Option<&str>)> = alices
            .threads
            .iter()
            .map(|summary| {
                (
                    summary.thread.record.subject.as_str(),
                    summary.unread_from_people,
                    summary.unread_from_dibs,
                    summary.last_unread_from.as_ref().map(Handle::as_str),
                )
            })
            .collect();
        assert_eq!(
            split,
            [
                ("Notice 9003", 0, 1, None),
                ("Notice 9002", 0, 1, None),
                ("Notice 9001", 0, 1, None),
                ("Staging", 1, 0, Some("bob")),
                ("Rates", 1, 0, Some("bob")),
            ]
        );
        let bobs = inbox.execute(&env.bob.actor, asked).await.unwrap();
        assert_eq!(
            bobs.unread_conversations,
            UnreadConversations::default(),
            "bob may read none of dibs's notices to alice, and wrote the rest"
        );
    }

    #[tokio::test]
    async fn the_inbox_counts_and_lists_what_awaits_an_ack_when_asked() {
        let env = Env::new().await;
        let asked = StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    importance: Importance::Urgent,
                    ack_required: true,
                    ..input(Addressees::Project, "Freeze")
                },
            )
            .await
            .unwrap();
        let inbox = Inbox::new(&env.deps);
        let plain = inbox
            .execute(&env.bob.actor, InboxQuery::default())
            .await
            .unwrap();
        assert_eq!(plain.unacked, None, "not asked for");
        assert_eq!(plain.threads[0].awaiting_ack, 1);
        assert_eq!(plain.threads[0].importance, Importance::Urgent);
        let full = inbox
            .execute(
                &env.bob.actor,
                InboxQuery {
                    unread_only: false,
                    unacked: true,
                    deliverable: false,
                },
            )
            .await
            .unwrap();
        let ids: Vec<_> = full
            .unacked
            .unwrap()
            .iter()
            .map(|u| u.message.record.id)
            .collect();
        assert_eq!(ids, [asked.message.record.id]);
        let author = inbox
            .execute(&env.alice.actor, InboxQuery::default())
            .await
            .unwrap();
        assert_eq!(author.threads[0].awaiting_ack, 0, "never for its author");
        AckMessage::new(&env.deps)
            .execute(&env.bob.actor, asked.message.record.id)
            .await
            .unwrap();
        let after = inbox
            .execute(
                &env.bob.actor,
                InboxQuery {
                    unread_only: false,
                    unacked: true,
                    deliverable: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(after.threads[0].awaiting_ack, 0);
        assert_eq!(after.unacked, Some(Vec::new()));
    }

    fn deliverable_query() -> InboxQuery {
        InboxQuery {
            unread_only: false,
            unacked: false,
            deliverable: true,
        }
    }

    async fn deliverable_ids(env: &Env, actor: &Actor) -> Vec<MessageId> {
        Inbox::new(&env.deps)
            .execute(actor, deliverable_query())
            .await
            .unwrap()
            .deliverable
            .unwrap()
            .iter()
            .map(|entry| entry.message.record.id)
            .collect()
    }

    async fn send(env: &Env, from: &Actor, input: StartThreadInput) -> (ThreadId, MessageId) {
        let started = StartThread::new(&env.deps)
            .execute(from, input)
            .await
            .unwrap();
        (started.thread.record.id, started.message.record.id)
    }

    #[tokio::test]
    async fn what_reaches_a_session_and_was_not_delivered_is_offered_for_delivery_newest_first() {
        let env = Env::new().await;
        let to_bob = || Addressees::Members(vec![Handle::parse("bob").unwrap()]);
        let (_, urgent) = send(
            &env,
            &env.alice.actor,
            StartThreadInput {
                importance: Importance::Urgent,
                ..input(to_bob(), "Staging is down")
            },
        )
        .await;
        env.advance(time::Duration::minutes(1));
        let (_, lunch) = send(&env, &env.alice.actor, input(to_bob(), "Lunch")).await;
        env.advance(time::Duration::minutes(1));
        let (asking_thread, asking) = send(
            &env,
            &env.alice.actor,
            StartThreadInput {
                ack_required: true,
                ..input(to_bob(), "Read the rates")
            },
        )
        .await;
        send(
            &env,
            &env.bob.actor,
            StartThreadInput {
                importance: Importance::Urgent,
                ..input(Addressees::Project, "Mine")
            },
        )
        .await;
        assert_eq!(
            deliverable_ids(&env, &env.bob.actor).await,
            [asking, lunch, urgent],
            "my own message is never delivered"
        );
        let plain = Inbox::new(&env.deps)
            .execute(&env.bob.actor, InboxQuery::default())
            .await
            .unwrap();
        assert_eq!(plain.deliverable, None, "not asked for");
        MarkRead::new(&env.deps)
            .execute(&env.bob.actor, asking_thread)
            .await
            .unwrap();
        assert_eq!(
            deliverable_ids(&env, &env.bob.actor).await,
            [asking, lunch, urgent],
            "a message that awaits my acknowledgement presses until I give it"
        );
        crate::use_cases::ClaimDelivery::new(&env.deps)
            .execute(
                &env.bob.actor,
                env.bob.session,
                crate::use_cases::DeliveryAsk::Message(urgent),
            )
            .await
            .unwrap();
        assert_eq!(
            deliverable_ids(&env, &env.bob.actor).await,
            [asking, lunch],
            "delivered to one of my sessions"
        );
        env.advance(DELIVERABLE_FOR);
        assert!(
            deliverable_ids(&env, &env.bob.actor).await.is_empty(),
            "a day later it is left to the brief"
        );
    }

    #[tokio::test]
    async fn a_persons_message_to_named_members_is_delivered_whatever_its_importance() {
        let env = Env::new().await;
        let to = |handle: &str| Addressees::Members(vec![Handle::parse(handle).unwrap()]);
        let (_, to_alice) = send(&env, &env.bob.actor, input(to("alice"), "Rates")).await;
        env.advance(time::Duration::minutes(1));
        let (alices_thread, _) = send(&env, &env.alice.actor, input(to("bob"), "Search")).await;
        let reply = crate::use_cases::PostMessage::new(&env.deps)
            .execute(
                &env.bob.actor,
                crate::use_cases::PostMessageInput {
                    thread: alices_thread,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("On it.").unwrap(),
                    importance: Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap()
            .message
            .record
            .id;
        env.advance(time::Duration::minutes(1));
        send(&env, &env.bob.actor, input(Addressees::Project, "Deploy")).await;
        let (_, raised_to_everyone) = send(
            &env,
            &env.bob.actor,
            StartThreadInput {
                importance: Importance::High,
                ..input(Addressees::Project, "Freeze")
            },
        )
        .await;
        notice_to_alice(&env, 9001).await;
        assert_eq!(
            deliverable_ids(&env, &env.alice.actor).await,
            [raised_to_everyone, reply, to_alice],
            "a normal message to the whole project and dibs's normal notice wait for the brief; \
             a reply in a conversation she started reaches her as its author"
        );
        crate::use_cases::ClaimDelivery::new(&env.deps)
            .execute(
                &env.alice.actor,
                env.alice.session,
                crate::use_cases::DeliveryAsk::Message(to_alice),
            )
            .await
            .unwrap();
        assert_eq!(
            deliverable_ids(&env, &env.alice.actor).await,
            [raised_to_everyone, reply],
            "handed to one of her sessions once"
        );
        env.advance(DELIVERABLE_FOR - time::Duration::minutes(1));
        assert_eq!(
            deliverable_ids(&env, &env.alice.actor).await,
            [raised_to_everyone],
            "older than a day, it is left to the brief"
        );
    }

    #[tokio::test]
    async fn an_ended_wide_claims_announcement_is_neither_delivered_nor_awaited_and_grants_nothing()
    {
        use crate::use_cases::{ClaimItem, ClaimItemInput, Release, Unacked, WideInput};
        use dibs_domain::wide::WideReason;

        let env = Env::new().await;
        let held = env.item("gh:acme/widgets#1").await;
        let wide = env.item("gh:acme/widgets#2").await;
        let claim = ClaimItem::new(&env.deps);
        claim
            .execute(
                &env.alice.actor,
                ClaimItemInput {
                    branches: Default::default(),
                    item: held,
                    session: env.alice.session,
                    areas: vec!["billing".to_owned()],
                    wide: None,
                    kind: None,
                },
            )
            .await
            .unwrap();
        claim
            .execute(
                &env.bob.actor,
                ClaimItemInput {
                    branches: Default::default(),
                    item: wide,
                    session: env.bob.session,
                    areas: Vec::new(),
                    wide: Some(WideInput {
                        areas: vec!["billing".to_owned(), "search".to_owned()],
                        reason: WideReason::parse("Upgrade the HTTP client").unwrap(),
                    }),
                    kind: None,
                },
            )
            .await
            .unwrap();
        let announced = deliverable_ids(&env, &env.alice.actor).await;
        assert_eq!(announced.len(), 1, "the announcement presses");
        let entries = Inbox::new(&env.deps)
            .execute(&env.alice.actor, deliverable_query())
            .await
            .unwrap()
            .deliverable
            .unwrap();
        assert!(
            entries[0].ack_grants,
            "acknowledging an announcement grants edits"
        );
        assert!(
            entries[0]
                .message
                .record
                .body
                .as_str()
                .ends_with(crate::use_cases::writing::TITLE_NOTE)
        );
        assert_eq!(
            Unacked::new(&env.deps)
                .execute(&env.alice.actor)
                .await
                .unwrap()
                .len(),
            1
        );
        Release::new(&env.deps)
            .execute(&env.bob.actor, wide)
            .await
            .unwrap();
        assert!(deliverable_ids(&env, &env.alice.actor).await.is_empty());
        assert!(
            Unacked::new(&env.deps)
                .execute(&env.alice.actor)
                .await
                .unwrap()
                .is_empty()
        );
        let inbox = Inbox::new(&env.deps)
            .execute(&env.alice.actor, InboxQuery::default())
            .await
            .unwrap();
        assert!(inbox.threads.iter().all(|thread| thread.awaiting_ack == 0));
        assert!(inbox.awaiting_ack.is_empty());
        let acked = AckMessage::new(&env.deps)
            .execute(&env.alice.actor, announced[0])
            .await
            .unwrap();
        assert!(acked.granted.is_empty());
        assert!(acked.announcement_ended);
    }

    /// alice holds billing on `#1` and search on `#3`; bob claims `#2` and `#4` and asks alice for
    /// billing for `#2`. Answers the conversation that asks, and the four items in that order.
    async fn asked_of_alice() -> (Env, ThreadId, [ItemId; 4]) {
        use crate::use_cases::{AskGrant, AskGrantInput, ClaimItem, ClaimItemInput, GrantAsked};

        let env = Env::new().await;
        let claim =
            |who: &crate::use_cases::testing::Person, item, areas: &[&str]| ClaimItemInput {
                branches: Default::default(),
                wide: None,
                item,
                session: who.session,
                areas: areas.iter().map(|a| (*a).to_owned()).collect(),
                kind: None,
            };
        let claim_item = ClaimItem::new(&env.deps);
        let mut items = [ItemId(0); 4];
        for (n, (who, areas)) in [
            (&env.alice, &["billing"][..]),
            (&env.bob, &[][..]),
            (&env.alice, &["search"][..]),
            (&env.bob, &[][..]),
        ]
        .into_iter()
        .enumerate()
        {
            items[n] = env.item(&format!("gh:acme/widgets#{}", n + 1)).await;
            claim_item
                .execute(&who.actor, claim(who, items[n], areas))
                .await
                .unwrap();
        }
        let asked = AskGrant::new(&env.deps)
            .execute(
                &env.bob.actor,
                AskGrantInput {
                    item: items[1],
                    area: "billing".to_owned(),
                },
            )
            .await
            .unwrap();
        let GrantAsked::Asked { thread, .. } = asked.outcome else {
            panic!("not asked: {asked:?}");
        };
        (env, thread, items)
    }

    async fn grant_asks(env: &Env, actor: &Actor) -> Vec<PendingGrantAsk> {
        Inbox::new(&env.deps)
            .execute(actor, InboxQuery::default())
            .await
            .unwrap()
            .grant_asks
    }

    #[tokio::test]
    async fn a_grant_asked_of_the_caller_is_listed_with_what_answering_it_needs() {
        let (env, thread, items) = asked_of_alice().await;
        let [listed] = &grant_asks(&env, &env.alice.actor).await[..] else {
            panic!("one request");
        };
        assert_eq!(listed.thread, thread);
        assert_eq!(listed.asker.as_str(), "bob");
        assert_eq!(
            listed.item.external_ref.as_ref().map(ToString::to_string),
            Some("gh:acme/widgets#2".to_owned())
        );
        assert_eq!(listed.item.title.as_str(), "Work on gh:acme/widgets#2");
        assert_eq!(listed.area.to_string(), "default:billing");
        assert_eq!(listed.item.id, items[1]);
        assert_eq!(
            listed.holder_item.id, items[0],
            "the claim of the caller's that holds the area"
        );
        assert_eq!(listed.asked_at, env.now());
        assert!(
            grant_asks(&env, &env.bob.actor).await.is_empty(),
            "the asker's inbox lists nothing"
        );
        MarkRead::new(&env.deps)
            .execute(&env.alice.actor, thread)
            .await
            .unwrap();
        assert_eq!(
            grant_asks(&env, &env.alice.actor).await.len(),
            1,
            "reading it decides nothing"
        );
    }

    #[tokio::test]
    async fn grant_requests_are_listed_newest_first() {
        use crate::use_cases::{AskGrant, AskGrantInput};

        let (env, first, items) = asked_of_alice().await;
        env.advance(time::Duration::minutes(1));
        AskGrant::new(&env.deps)
            .execute(
                &env.bob.actor,
                AskGrantInput {
                    item: items[3],
                    area: "search".to_owned(),
                },
            )
            .await
            .unwrap();
        let listed = grant_asks(&env, &env.alice.actor).await;
        let areas: Vec<String> = listed.iter().map(|ask| ask.area.to_string()).collect();
        assert_eq!(areas, ["default:search", "default:billing"]);
        assert_eq!(listed[0].holder_item.id, items[2]);
        assert_eq!(listed[1].thread, first);
    }

    #[tokio::test]
    async fn a_granted_request_is_no_longer_listed() {
        use crate::use_cases::{Grant, GrantInput};

        let (env, _, items) = asked_of_alice().await;
        Grant::new(&env.deps)
            .execute(
                &env.alice.actor,
                GrantInput {
                    item: items[1],
                    area: "billing".to_owned(),
                    to: Handle::parse("bob").unwrap(),
                },
            )
            .await
            .unwrap();
        assert!(grant_asks(&env, &env.alice.actor).await.is_empty());
    }

    #[tokio::test]
    async fn a_request_answered_in_its_conversation_is_no_longer_listed() {
        use crate::use_cases::{PostMessage, PostMessageInput};

        let (env, thread, _) = asked_of_alice().await;
        PostMessage::new(&env.deps)
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("It is a one-line change.").unwrap(),
                    importance: Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            grant_asks(&env, &env.alice.actor).await.len(),
            1,
            "the asker writing again answers nothing"
        );
        PostMessage::new(&env.deps)
            .execute(
                &env.alice.actor,
                PostMessageInput {
                    thread,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Not today, billing is mid-migration.").unwrap(),
                    importance: Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        assert!(grant_asks(&env, &env.alice.actor).await.is_empty());
    }

    #[tokio::test]
    async fn every_waiting_request_is_listed_for_the_pages_while_an_agent_is_handed_the_newest_ten()
    {
        use crate::use_cases::{AskGrant, AskGrantInput, ClaimItem, ClaimItemInput};

        let (env, first, _) = asked_of_alice().await;
        for n in 5..16 {
            env.advance(time::Duration::minutes(7));
            let item = env.item(&format!("gh:acme/widgets#{n}")).await;
            ClaimItem::new(&env.deps)
                .execute(
                    &env.bob.actor,
                    ClaimItemInput {
                        branches: Default::default(),
                        item,
                        session: env.bob.session,
                        areas: Vec::new(),
                        wide: None,
                        kind: None,
                    },
                )
                .await
                .unwrap();
            AskGrant::new(&env.deps)
                .execute(
                    &env.bob.actor,
                    AskGrantInput {
                        item,
                        area: "billing".to_owned(),
                    },
                )
                .await
                .unwrap();
        }
        let inbox = Inbox::new(&env.deps)
            .execute(
                &env.alice.actor,
                InboxQuery {
                    unread_only: true,
                    unacked: false,
                    deliverable: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(inbox.every_grant_ask.len(), 12);
        assert_eq!(inbox.grant_asks.len(), MAX_GRANT_ASKS);
        assert_eq!(
            inbox.grant_asks[..],
            inbox.every_grant_ask[..MAX_GRANT_ASKS],
            "the agent's list is the newest of them"
        );
        assert_eq!(
            inbox.every_grant_ask.last().map(|ask| ask.thread),
            Some(first),
            "the oldest last"
        );
        let threads: BTreeSet<ThreadId> =
            inbox.every_grant_ask.iter().map(|ask| ask.thread).collect();
        assert_eq!(threads.len(), 12, "each in a conversation of its own");
    }

    #[tokio::test]
    async fn a_thread_awaiting_the_callers_acknowledgement_is_listed_read_or_not_until_it_is_given()
    {
        let env = Env::new().await;
        let (thread, asking) = send(
            &env,
            &env.alice.actor,
            StartThreadInput {
                ack_required: true,
                ..input(
                    Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                    "Read the rates",
                )
            },
        )
        .await;
        send(
            &env,
            &env.alice.actor,
            input(
                Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                "Lunch",
            ),
        )
        .await;
        let inbox = Inbox::new(&env.deps);
        let unread_only = InboxQuery {
            unread_only: true,
            unacked: false,
            deliverable: false,
        };
        let listed = |view: &InboxView| -> Vec<(ThreadId, u64)> {
            view.awaiting_ack
                .iter()
                .map(|summary| (summary.thread.record.id, summary.awaiting_ack))
                .collect()
        };
        let unread = inbox.execute(&env.bob.actor, unread_only).await.unwrap();
        assert_eq!(
            listed(&unread),
            [(thread, 1)],
            "the one asking, not the other"
        );
        MarkRead::new(&env.deps)
            .execute(&env.bob.actor, thread)
            .await
            .unwrap();
        let read = inbox.execute(&env.bob.actor, unread_only).await.unwrap();
        assert!(
            read.threads
                .iter()
                .all(|summary| summary.thread.record.id != thread),
            "read, so not among the unread"
        );
        assert_eq!(listed(&read), [(thread, 1)], "still waiting once read");
        assert!(
            inbox
                .execute(&env.alice.actor, unread_only)
                .await
                .unwrap()
                .awaiting_ack
                .is_empty(),
            "its author's never waits on itself"
        );
        AckMessage::new(&env.deps)
            .execute(&env.bob.actor, asking)
            .await
            .unwrap();
        assert!(
            inbox
                .execute(&env.bob.actor, unread_only)
                .await
                .unwrap()
                .awaiting_ack
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_request_lapses_with_the_askers_claim() {
        use crate::use_cases::Release;

        let (env, _, items) = asked_of_alice().await;
        Release::new(&env.deps)
            .execute(&env.bob.actor, items[1])
            .await
            .unwrap();
        assert!(grant_asks(&env, &env.alice.actor).await.is_empty());
    }
}
