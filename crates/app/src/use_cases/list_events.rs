//! Reading the project's event log by cursor.

use std::sync::Arc;

use dibs_domain::event::Event;
use dibs_domain::ids::EventId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::Store;
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
    /// The events, oldest first.
    pub events: Vec<Event>,
    /// The cursor to read on from: the last event of the page, or the cursor asked with when the
    /// page is empty. `None` while the log is empty.
    pub next: Option<EventId>,
}

/// Reads a page of the caller's project's events. Events carry ids and authors, never a subject
/// or a body, so every member and integration may read the whole log.
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

    /// Up to `limit` events from `from`; `limit` is held between 1 and [`MAX_PAGE`].
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
        let after = match from {
            EventCursor::Start => None,
            EventCursor::After(id) => Some(id),
            EventCursor::Latest => {
                return Ok(EventPage {
                    events: Vec::new(),
                    next: work.latest_event(actor.project).await?,
                });
            }
        };
        let events = work
            .events_after(actor.project, after, limit.clamp(1, MAX_PAGE))
            .await?;
        let next = events.last().map(|event| event.id).or(after);
        Ok(EventPage { events, next })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_cases::testing::Env;

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
        assert_eq!(first.events, all[..2]);
        let rest = list
            .execute(&env.importer, EventCursor::After(first.next.unwrap()), 100)
            .await
            .unwrap();
        assert_eq!(rest.events, all[2..]);
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
