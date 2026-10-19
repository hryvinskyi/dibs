//! Following the project's event log as one person: the events they may see, each announced
//! message with its text when they may read it.

use std::sync::Arc;

use dibs_domain::ids::EventId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedEvent, NamedMessage, Names};
use crate::ports::Store;
use crate::use_cases::list_events::{EventCursor, visible_page};
use crate::work::Work;

/// An event as one follower receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowedEvent {
    /// The event, with the handles of the members it names.
    pub event: NamedEvent,
    /// The message a `message_posted` event announces, with its author's handle; `None` for every
    /// other kind.
    pub message: Option<NamedMessage>,
}

/// One page of the log as one follower receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowPage {
    /// The events the follower may see, oldest first.
    pub events: Vec<FollowedEvent>,
    /// The cursor to read on from: the last event read, whether or not the follower may see it,
    /// or the cursor asked with when nothing was read. `None` while the log is empty.
    pub next: Option<EventId>,
    /// Whether the page read as many events as it could hold, so more may be waiting already.
    pub more: bool,
}

/// Reads a page of the caller's project's log for a person following it: the events
/// [`ListEvents`](crate::use_cases::ListEvents) shows them, each announced message with its text.
/// Each read checks that the token the caller signed in with still works, so a follower whose
/// token was revoked is refused at its next read.
pub struct FollowEvents {
    store: Arc<dyn Store>,
}

impl FollowEvents {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// Up to `limit` events from `from`; `limit` is held between 1 and
    /// [`MAX_PAGE`](crate::use_cases::MAX_PAGE). From
    /// [`EventCursor::Latest`] the page holds no event, only the cursor of the newest one.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Unauthorized`] when the caller's
    /// token was revoked or its member is gone; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        from: EventCursor,
        limit: usize,
    ) -> Result<FollowPage, AppError> {
        actor.require_person()?;
        let mut work = Work::begin(&*self.store, actor.project).await?;
        still_signed_in(&mut work, actor).await?;
        let page = visible_page(&mut work, actor, from, limit).await?;
        let names = Names::load(&mut work).await?;
        let mut events = Vec::new();
        for event in page.events {
            let message = match event.kind.message() {
                Some(id) => work
                    .message(actor.project, id)
                    .await?
                    .map(|message| names.message(message))
                    .transpose()?,
                None => None,
            };
            events.push(FollowedEvent {
                event: names.event(event)?,
                message,
            });
        }
        let (next, more) = (page.next, page.more);
        Ok(FollowPage { events, next, more })
    }
}

/// Refuses an actor whose token no longer authenticates them in their project.
async fn still_signed_in(work: &mut Work, actor: &Actor) -> Result<(), AppError> {
    let found = work.token_by_hash(&actor.token).await?;
    let current = found.is_some_and(|found| {
        found.project == actor.project
            && found.record.member == actor.member
            && found.record.is_active()
    });
    if current {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use dibs_domain::event::EventKind;
    use dibs_domain::handle::Handle;
    use dibs_domain::ids::ThreadId;
    use dibs_domain::message::{Body, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::MAX_PAGE;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{
        Addressees, PostMessage, PostMessageInput, RevokeToken, StartThread, StartThreadInput,
    };

    fn input(to: Addressees, body: &str) -> StartThreadInput {
        StartThreadInput {
            subject: Subject::parse("Invoice totals").unwrap(),
            to,
            item: None,
            label: SessionLabel::parse("laptop/mcp").unwrap(),
            body: Body::parse(body).unwrap(),
        }
    }

    #[tokio::test]
    async fn a_follower_sees_what_they_may_read_and_the_cursor_passes_the_rest() {
        let env = Env::new().await;
        let start = StartThread::new(&env.deps);
        let private = start
            .execute(
                &env.alice.actor,
                input(
                    Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    "Only for me",
                ),
            )
            .await
            .unwrap();
        let open = start
            .execute(&env.alice.actor, input(Addressees::Project, "For everyone"))
            .await
            .unwrap();
        PostMessage::new(&env.deps)
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread: open.thread.record.id,
                    label: SessionLabel::parse("web").unwrap(),
                    body: Body::parse("Seen.").unwrap(),
                },
            )
            .await
            .unwrap();
        let all = env.events().await;
        let follow = FollowEvents::new(&env.deps);
        let bobs = follow
            .execute(&env.bob.actor, EventCursor::Start, MAX_PAGE)
            .await
            .unwrap();
        let threads: BTreeSet<Option<ThreadId>> = bobs
            .events
            .iter()
            .map(|e| e.event.event.kind.thread())
            .collect();
        assert!(!threads.contains(&Some(private.thread.record.id)));
        assert!(threads.contains(&Some(open.thread.record.id)));
        assert_eq!(bobs.next, all.last().map(|e| e.id));
        assert!(!bobs.more);
        let bodies: Vec<(&str, &str)> = bobs
            .events
            .iter()
            .filter_map(|e| e.message.as_ref())
            .map(|m| (m.author.as_str(), m.record.body.as_str()))
            .collect();
        assert_eq!(bodies, [("alice", "For everyone"), ("bob", "Seen.")]);
        assert!(
            bobs.events.iter().all(|e| e.message.is_none()
                == !matches!(e.event.event.kind, EventKind::MessagePosted(_)))
        );

        let alices = follow
            .execute(&env.alice.actor, EventCursor::Start, MAX_PAGE)
            .await
            .unwrap();
        assert_eq!(alices.events.len(), all.len());
        assert!(alices.events.iter().any(|e| {
            e.message
                .as_ref()
                .is_some_and(|m| m.record.body.as_str() == "Only for me")
        }));
    }

    #[tokio::test]
    async fn pages_follow_the_cursor_and_say_when_more_wait() {
        let env = Env::new().await;
        env.item("gh:acme/widgets#1").await;
        env.item("gh:acme/widgets#2").await;
        let all = env.events().await;
        let follow = FollowEvents::new(&env.deps);
        let first = follow
            .execute(&env.bob.actor, EventCursor::Start, 2)
            .await
            .unwrap();
        assert!(first.more);
        assert_eq!(first.next, Some(all[1].id));
        let rest = follow
            .execute(&env.bob.actor, EventCursor::After(all[1].id), 2)
            .await
            .unwrap();
        let ids: Vec<EventId> = rest.events.iter().map(|e| e.event.event.id).collect();
        assert_eq!(ids, all[2..].iter().map(|e| e.id).collect::<Vec<_>>());
        let latest = follow
            .execute(&env.bob.actor, EventCursor::Latest, 2)
            .await
            .unwrap();
        assert!(latest.events.is_empty() && !latest.more);
        assert_eq!(latest.next, all.last().map(|e| e.id));
        let past_the_end = follow
            .execute(&env.bob.actor, EventCursor::After(EventId(9_999)), 2)
            .await
            .unwrap();
        assert!(past_the_end.events.is_empty());
        assert_eq!(past_the_end.next, Some(EventId(9_999)));
    }

    #[tokio::test]
    async fn an_integration_and_a_revoked_token_are_refused() {
        let env = Env::new().await;
        let follow = FollowEvents::new(&env.deps);
        let integration = follow
            .execute(&env.importer, EventCursor::Start, 10)
            .await
            .unwrap_err();
        assert_eq!(integration.code(), "forbidden");
        let mut tx = env.deps.store.begin().await.unwrap();
        let bobs = tx
            .token_by_hash(&env.bob.actor.token)
            .await
            .unwrap()
            .unwrap()
            .record;
        drop(tx);
        RevokeToken::new(&env.deps)
            .execute(&env.alice.actor, bobs.prefix)
            .await
            .unwrap();
        let revoked = follow
            .execute(&env.bob.actor, EventCursor::Start, 10)
            .await
            .unwrap_err();
        assert_eq!(revoked.code(), "unauthorized");
    }
}
