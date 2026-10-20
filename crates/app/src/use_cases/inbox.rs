//! The threads waiting for the caller, and the messages waiting for their acknowledgement.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use time::{Duration, OffsetDateTime};

use dibs_domain::delivery::DeliverySubject;
use dibs_domain::ids::MessageId;
use dibs_domain::message::speaks_for;
use dibs_domain::wide::ask_stands;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::slot::SlotName;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedMessage, NamedSummary, NamedThread, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::records::{MessageRecord, ThreadRecord, ThreadSummary};
use crate::use_cases::acks::{awaiting, awaiting_per_thread, people, reached};
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
}

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
/// to their agent; and the slots on offer to them, after applying and recording what time changed
/// about the slots — all in one transaction.
pub struct Inbox {
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
        let threads = readable
            .iter()
            .filter(|summary| !query.unread_only || summary.unread > 0)
            .cloned()
            .map(|summary| {
                let count = waiting.get(&summary.thread.id).copied().unwrap_or(0);
                names.summary(summary, count)
            })
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
        work.commit(&*self.notifier).await?;
        Ok(InboxView {
            threads,
            unacked,
            offers,
            deliverable,
        })
    }
}

/// The messages still to be delivered to the actor's agent, newest first: written by someone else
/// within [`DELIVERABLE_FOR`], reaching the actor, and pressing — raised and unread, or awaiting
/// their acknowledgement — leaving out every announcement of a wide claim that has ended and every
/// message already delivered to one of their sessions.
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
    for summary in readable
        .iter()
        .filter(|summary| summary.unread > 0 && summary.importance.is_raised())
    {
        for message in work.messages(project, summary.thread.id).await? {
            let pressing = message.importance.is_raised()
                && speaks_for(summary.thread.author, message.author) != Some(actor.member)
                && recent(&message)
                && !found.contains_key(&message.id)
                && reached(&summary.thread, &message, &people).contains(&actor.member);
            if !pressing {
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
    async fn what_presses_and_was_not_delivered_is_offered_for_delivery_newest_first() {
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
        send(&env, &env.alice.actor, input(to_bob(), "Lunch")).await;
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
            [asking, urgent],
            "an ordinary message asking nothing, and my own, are never delivered"
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
            [asking, urgent],
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
            [asking],
            "delivered to one of my sessions"
        );
        env.advance(DELIVERABLE_FOR);
        assert!(
            deliverable_ids(&env, &env.bob.actor).await.is_empty(),
            "a day later it is left to the brief"
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
        let acked = AckMessage::new(&env.deps)
            .execute(&env.alice.actor, announced[0])
            .await
            .unwrap();
        assert!(acked.granted.is_empty());
        assert!(acked.announcement_ended);
    }
}
