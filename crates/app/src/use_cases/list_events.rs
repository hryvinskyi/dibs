//! Reading the project's event log by cursor, as the caller may see it.

use std::collections::BTreeMap;
use std::sync::Arc;

use dibs_domain::event::{Event, EventKind};
use dibs_domain::ids::{EventId, ThreadId};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedEvent, Names};
use crate::ports::Store;
use crate::records::ThreadRecord;
use crate::use_cases::readable::may_read_thread;
use crate::work::Work;

/// The most events one page holds.
pub const MAX_PAGE: usize = 500;

/// Where to start reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCursor {
    /// From the first event of the log.
    Start,
    /// After this event.
    After(EventId),
    /// From now on: no event yet, only the cursor of the newest one.
    Latest,
}

/// One page of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    /// The events the caller may see, oldest first, each with the handles of the members it names.
    pub events: Vec<NamedEvent>,
    /// The cursor to read on from: the last event read, whether or not the caller may see it, or
    /// the cursor asked with when nothing was read. `None` while the log is empty.
    pub next: Option<EventId>,
}

/// Reads a page of the caller's project's events. Events carry ids and authors, never a subject
/// or a body; an event about a conversation the caller may not read is still left out, and so is
/// one about a conversation that cannot be found, while the cursor moves past it. An integration
/// reads no conversation, so it sees no conversation's events.
pub struct ListEvents {
    store: Arc<dyn Store>,
}

impl ListEvents {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// Up to `limit` events read from `from`, those the caller may see kept; `limit` is held
    /// between 1 and [`MAX_PAGE`].
    ///
    /// # Errors
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        from: EventCursor,
        limit: usize,
    ) -> Result<EventPage, AppError> {
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let page = visible_page(&mut work, actor, from, limit).await?;
        let names = Names::load(&mut work).await?;
        let events = page
            .events
            .into_iter()
            .map(|event| names.event(event))
            .collect::<Result<_, _>>()?;
        Ok(EventPage {
            events,
            next: page.next,
        })
    }
}

/// A page of the log with what the reader may not see left out.
pub(crate) struct VisiblePage {
    /// The events the reader may see, oldest first.
    pub(crate) events: Vec<Event>,
    /// The last event read, or the cursor asked with when nothing was read.
    pub(crate) next: Option<EventId>,
    /// Whether as many events were read as the page could hold.
    pub(crate) more: bool,
}

/// Reads up to `limit` events from `from` (held between 1 and [`MAX_PAGE`]) and keeps those the
/// actor may see. From [`EventCursor::Latest`] the page holds no event, only the newest cursor.
pub(crate) async fn visible_page(
    work: &mut Work,
    actor: &Actor,
    from: EventCursor,
    limit: usize,
) -> Result<VisiblePage, AppError> {
    let project = work.project_id();
    let after = match from {
        EventCursor::Start => None,
        EventCursor::After(id) => Some(id),
        EventCursor::Latest => {
            return Ok(VisiblePage {
                events: Vec::new(),
                next: work.latest_event(project).await?,
                more: false,
            });
        }
    };
    let limit = limit.clamp(1, MAX_PAGE);
    let read = work.events_after(project, after, limit).await?;
    let more = read.len() == limit;
    let next = read.last().map(|event| event.id).or(after);
    let mut threads = BTreeMap::new();
    for id in read.iter().filter_map(|event| event.kind.thread()) {
        if !threads.contains_key(&id)
            && let Some(thread) = work.thread(project, id).await?
        {
            threads.insert(id, thread);
        }
    }
    let events = read
        .into_iter()
        .filter(|event| visible(actor, &event.kind, &threads))
        .collect();
    Ok(VisiblePage { events, next, more })
}

/// Whether the actor may see an event: always, unless it is about a conversation; then only when
/// the conversation is among `threads` and the actor may read it.
pub(crate) fn visible(
    actor: &Actor,
    kind: &EventKind,
    threads: &BTreeMap<ThreadId, ThreadRecord>,
) -> bool {
    match kind.thread() {
        None => true,
        Some(id) => threads
            .get(&id)
            .is_some_and(|thread| may_read_thread(actor, thread)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use time::OffsetDateTime;

    use dibs_domain::event::{Completed, MessagePosted, ThreadStarted};
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId};
    use dibs_domain::member::Role;
    use dibs_domain::message::{Body, Recipients, SessionLabel, Subject};

    use super::*;
    use crate::token::TokenHash;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{Addressees, StartThread, StartThreadInput};

    fn actor(member: u64, role: Role) -> Actor {
        Actor {
            project: ProjectId(1),
            member: MemberId(member),
            role,
            token: TokenHash::new("h".to_owned()),
        }
    }

    fn thread(id: u64, author: u64, recipients: Recipients) -> ThreadRecord {
        ThreadRecord {
            id: ThreadId(id),
            subject: Subject::parse("Billing").unwrap(),
            item: None,
            author: MemberId(author),
            recipients,
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn posted(thread: u64) -> EventKind {
        EventKind::MessagePosted(MessagePosted {
            thread: ThreadId(thread),
            message: MessageId(1),
            author: MemberId(1),
        })
    }

    #[test]
    fn an_event_about_a_conversation_is_seen_by_its_readers_only() {
        let threads = BTreeMap::from([
            (
                ThreadId(10),
                thread(
                    10,
                    1,
                    Recipients::members(BTreeSet::from([MemberId(2)])).unwrap(),
                ),
            ),
            (ThreadId(20), thread(20, 1, Recipients::Project)),
        ]);
        let started = EventKind::ThreadStarted(ThreadStarted {
            thread: ThreadId(10),
            author: MemberId(1),
            item: None,
        });
        let completed = EventKind::Completed(Completed {
            item: ItemId(4),
            member: MemberId(1),
        });
        let cases = [
            (actor(1, Role::Admin), &started, true),
            (actor(2, Role::Member), &started, true),
            (actor(3, Role::Member), &started, false),
            (actor(3, Role::Member), &posted(10), false),
            (actor(2, Role::Member), &posted(10), true),
            (actor(3, Role::Member), &posted(20), true),
            (actor(3, Role::Member), &posted(30), false),
            (actor(3, Role::Member), &completed, true),
            (actor(4, Role::Importer), &posted(20), false),
            (actor(4, Role::Importer), &completed, true),
        ];
        for (who, kind, expected) in cases {
            assert_eq!(
                visible(&who, kind, &threads),
                expected,
                "member {} sees {}",
                who.member,
                kind.name()
            );
        }
    }

    #[tokio::test]
    async fn a_private_conversation_is_left_out_and_the_cursor_passes_it() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse("Private").unwrap(),
                    to: Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    item: Some(item),
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("just me").unwrap(),
                    importance: dibs_domain::message::Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
        let all = env.events().await;
        let list = ListEvents::new(&env.deps);
        for reader in [&env.bob.actor, &env.importer] {
            let page = list.execute(reader, EventCursor::Start, 100).await.unwrap();
            let kinds: Vec<&str> = page.events.iter().map(|e| e.event.kind.name()).collect();
            assert!(!kinds.contains(&"thread_started"), "{kinds:?}");
            assert!(!kinds.contains(&"message_posted"), "{kinds:?}");
            assert_eq!(page.next, all.last().map(|e| e.id));
        }
        let only_private = list
            .execute(
                &env.bob.actor,
                EventCursor::After(all[all.len() - 3].id),
                100,
            )
            .await
            .unwrap();
        assert!(only_private.events.is_empty());
        assert_eq!(only_private.next, all.last().map(|e| e.id));
        let alices = list
            .execute(&env.alice.actor, EventCursor::Start, 100)
            .await
            .unwrap();
        assert_eq!(alices.events.len(), all.len());
    }

    #[tokio::test]
    async fn the_log_pages_by_cursor_and_stays_in_its_project() {
        let env = Env::new().await;
        for n in 1..=3 {
            env.item(&format!("gh:acme/widgets#{n}")).await;
        }
        let list = ListEvents::new(&env.deps);
        let all = env.events().await;
        let first = list
            .execute(&env.importer, EventCursor::Start, 2)
            .await
            .unwrap();
        let events = |page: &EventPage| -> Vec<Event> {
            page.events
                .iter()
                .map(|named| named.event.clone())
                .collect()
        };
        assert_eq!(events(&first), all[..2]);
        assert!(first.events.iter().all(|named| {
            named
                .members
                .values()
                .map(|h| h.as_str())
                .collect::<Vec<_>>()
                == ["alice"]
        }));
        let rest = list
            .execute(&env.importer, EventCursor::After(first.next.unwrap()), 100)
            .await
            .unwrap();
        assert_eq!(events(&rest), all[2..]);
        let end = list
            .execute(&env.importer, EventCursor::After(rest.next.unwrap()), 100)
            .await
            .unwrap();
        assert!(end.events.is_empty());
        assert_eq!(end.next, rest.next);
        let latest = list
            .execute(&env.bob.actor, EventCursor::Latest, 100)
            .await
            .unwrap();
        assert!(latest.events.is_empty());
        assert_eq!(latest.next, all.last().map(|e| e.id));
        assert!(all.iter().all(|e| e.project == env.project));
    }

    #[tokio::test]
    async fn a_page_holds_one_to_five_hundred_events() {
        let env = Env::new().await;
        env.item("gh:acme/widgets#1").await;
        env.item("gh:acme/widgets#2").await;
        let page = ListEvents::new(&env.deps)
            .execute(&env.bob.actor, EventCursor::Start, 0)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
    }
}
