//! The daily retention: what is past its date goes, project by project.

use std::collections::BTreeSet;
use std::sync::Arc;

use dibs_domain::event::{EventKind, Pruned};
use dibs_domain::ids::{ProjectId, ThreadId};
use dibs_domain::retention::{Cutoffs, Keeps, PruneCounts, thread_expired};

use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, Store};
use crate::work::Work;

/// What one pass did.
#[derive(Debug, Default)]
pub struct PruneReport {
    /// What went, per project that lost anything, ordered by project.
    pub pruned: Vec<(ProjectId, PruneCounts)>,
    /// How many browser sessions past their end went, across every project.
    pub ui_sessions: u64,
    /// The projects the pass could not apply, each with why; the others were applied all the same.
    pub failed: Vec<(ProjectId, AppError)>,
}

/// Removes, in every project and each project in a transaction of its own, the conversations quiet
/// for longer than they are kept — but never one about a work item that is still open, nor one
/// carrying an escalation no arbiter has settled — with their messages, and the events older than the log keeps; then records what went in one `pruned` event
/// per project that lost anything. A reader whose cursor is behind what went is told so. Browser
/// sessions past their end go too. The service runs it once a day; it acts for nobody.
pub struct Prune {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl Prune {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Runs one pass.
    ///
    /// # Errors
    /// [`AppError::Store`] when the projects cannot be listed or the browser sessions cannot be
    /// swept; a project that fails is in the report instead.
    pub async fn execute(&self) -> Result<PruneReport, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let projects = tx.project_ids().await?;
        let ui_sessions = tx.delete_expired_ui_sessions(now).await?;
        tx.commit().await?;
        let mut report = PruneReport {
            ui_sessions,
            ..PruneReport::default()
        };
        let cutoffs = Cutoffs::at(now);
        for project in projects {
            match self.prune(project, &cutoffs).await {
                Ok(counts) if counts.is_empty() => {}
                Ok(counts) => report.pruned.push((project, counts)),
                Err(error) => report.failed.push((project, error)),
            }
        }
        Ok(report)
    }

    async fn prune(&self, project: ProjectId, cutoffs: &Cutoffs) -> Result<PruneCounts, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_unattended(&*self.store, project).await?;
        let mut counts = PruneCounts::default();
        let escalated: BTreeSet<ThreadId> = work
            .open_escalations(project)
            .await?
            .into_iter()
            .map(|open| open.thread)
            .collect();
        for quiet in work.quiet_threads(project, cutoffs.threads).await? {
            let open_item = match quiet.item {
                Some(item) => work
                    .item(project, item)
                    .await?
                    .is_some_and(|item| !item.is_closed()),
                None => false,
            };
            let keeps = Keeps {
                open_item,
                open_escalation: escalated.contains(&quiet.thread),
            };
            if thread_expired(quiet.last_activity, keeps, cutoffs) {
                counts.messages += work.delete_thread(project, quiet.thread).await?;
                counts.threads += 1;
            }
        }
        counts.events = work
            .delete_events_before(project, cutoffs.events)
            .await?
            .count;
        if !counts.is_empty() {
            work.record(now, EventKind::Pruned(Pruned { counts }))
                .await?;
        }
        work.commit(&*self.notifier).await?;
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use dibs_domain::handle::Handle;
    use dibs_domain::ids::{EventId, ItemId, ThreadId};
    use dibs_domain::message::{Body, Importance, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::{Env, Person};
    use crate::use_cases::{
        Addressees, ClaimItem, ClaimItemInput, Complete, EventCursor, ListEvents, ListThreads,
        PostMessage, PostMessageInput, StartThread, StartThreadInput,
    };

    async fn thread(env: &Env, subject: &str, item: Option<ItemId>) -> ThreadId {
        StartThread::new(&env.deps)
            .execute(
                &env.alice.actor,
                StartThreadInput {
                    subject: Subject::parse(subject).unwrap(),
                    to: Addressees::Members(vec![Handle::parse("bob").unwrap()]),
                    item,
                    label: SessionLabel::parse("laptop/4f2a91c0").unwrap(),
                    body: Body::parse("hello").unwrap(),
                    importance: Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap()
            .thread
            .record
            .id
    }

    async fn reply(env: &Env, thread: ThreadId) {
        PostMessage::new(&env.deps)
            .execute(
                &env.bob.actor,
                PostMessageInput {
                    thread,
                    label: SessionLabel::parse("desktop/77aa01bc").unwrap(),
                    body: Body::parse("still here").unwrap(),
                    importance: Importance::Normal,
                    ack_required: false,
                },
            )
            .await
            .unwrap();
    }

    async fn claim(env: &Env, who: &Person, item: ItemId) {
        ClaimItem::new(&env.deps)
            .execute(
                &who.actor,
                ClaimItemInput {
                    item,
                    session: who.session,
                    areas: Vec::new(),
                    wide: None,
                    kind: None,
                },
            )
            .await
            .unwrap();
    }

    async fn subjects(env: &Env) -> Vec<String> {
        let mut found: Vec<String> = ListThreads::new(&env.deps)
            .execute(&env.alice.actor, None)
            .await
            .unwrap()
            .into_iter()
            .map(|summary| summary.thread.record.subject.to_string())
            .collect();
        found.sort();
        found
    }

    #[tokio::test]
    async fn quiet_threads_and_old_events_go_by_date_but_an_open_items_thread_stays() {
        let env = Env::new().await;
        let open = env.item("gh:acme/widgets#1").await;
        let done = env.item("gh:acme/widgets#2").await;
        claim(&env, &env.alice, open).await;
        claim(&env, &env.bob, done).await;
        Complete::new(&env.deps)
            .execute(&env.bob.actor, done)
            .await
            .unwrap();
        thread(&env, "Quiet", None).await;
        thread(&env, "About the open item", Some(open)).await;
        thread(&env, "About the done item", Some(done)).await;
        let lively = thread(&env, "Lively", None).await;
        let old_cursor = env.events().await.last().unwrap().id;
        env.advance(Duration::days(100));
        reply(&env, lively).await;
        env.advance(Duration::days(81));

        let report = Prune::new(&env.deps).execute().await.unwrap();
        assert!(report.failed.is_empty());
        assert_eq!(report.pruned.len(), 1);
        let (project, counts) = report.pruned[0];
        assert_eq!(project, env.project);
        assert_eq!(
            (counts.threads, counts.messages),
            (2, 2),
            "the quiet thread and the done item's, a message each"
        );
        assert!(counts.events > 0);
        assert_eq!(
            subjects(&env).await,
            ["About the open item", "Lively"],
            "180 days after the last message, unless the item is open"
        );
        let kinds = env.kinds_after(0).await;
        assert!(
            kinds
                .iter()
                .all(|kind| !matches!(kind, EventKind::Claimed(_)))
        );
        assert_eq!(kinds.last(), Some(&EventKind::Pruned(Pruned { counts })));
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, EventKind::MessagePosted(_))),
            "the reply 81 days ago is younger than the log keeps"
        );

        let again = Prune::new(&env.deps).execute().await.unwrap();
        assert!(again.pruned.is_empty(), "nothing more is past its date");
        assert_eq!(env.kinds_after(0).await, kinds, "and no event says so");

        let list = ListEvents::new(&env.deps);
        let behind = list
            .execute(
                &env.alice.actor,
                EventCursor::After(EventId(old_cursor.0 - 1)),
                10,
            )
            .await
            .unwrap_err();
        assert_eq!(behind.code(), "cursor_expired");
        assert!(
            list.execute(&env.alice.actor, EventCursor::After(old_cursor), 10)
                .await
                .is_ok(),
            "a reader that had every event removed missed nothing"
        );
        assert!(
            list.execute(&env.alice.actor, EventCursor::Start, 10)
                .await
                .is_ok(),
            "a reader starts again from the oldest kept"
        );
        let newest = env.events().await.last().unwrap().id;
        assert!(
            list.execute(&env.alice.actor, EventCursor::After(newest), 10)
                .await
                .is_ok()
        );
        assert!(
            list.execute(
                &env.alice.actor,
                EventCursor::After(EventId(newest.0 + 5)),
                10
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_thread_stays_while_its_item_is_open_and_goes_once_the_item_is_done() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#7").await;
        claim(&env, &env.alice, item).await;
        thread(&env, "Long work", Some(item)).await;
        env.advance(Duration::days(400));
        Prune::new(&env.deps).execute().await.unwrap();
        assert_eq!(subjects(&env).await, ["Long work"]);
        Complete::new(&env.deps)
            .execute(&env.alice.actor, item)
            .await
            .unwrap();
        Prune::new(&env.deps).execute().await.unwrap();
        assert!(subjects(&env).await.is_empty());
    }

    #[tokio::test]
    async fn a_thread_with_an_escalation_still_waiting_stays_until_an_arbiter_settles_it() {
        use dibs_domain::escalation::{EscalationReason, EscalationSubject};
        use dibs_domain::slot::SlotName;

        use crate::use_cases::{AckMessage, Escalate, EscalateInput};

        let env = Env::with_settings(
            r#"{"arbiters": ["alice"], "slots": [{"name": "deploy:prod", "max_hold": "30m"}]}"#,
        )
        .await;
        let sent = Escalate::new(&env.deps)
            .execute(
                &env.bob.actor,
                EscalateInput {
                    subject: EscalationSubject::Slot(SlotName::parse("deploy:prod").unwrap()),
                    reason: EscalationReason::parse("who deploys first?").unwrap(),
                },
            )
            .await
            .unwrap();
        env.advance(Duration::days(200));
        Prune::new(&env.deps).execute().await.unwrap();
        assert_eq!(
            subjects(&env).await,
            ["Escalated by bob: the slot deploy:prod"],
            "no item keeps it, the escalation does"
        );
        AckMessage::new(&env.deps)
            .execute(&env.alice.actor, sent.message)
            .await
            .unwrap();
        env.advance(Duration::days(181));
        Prune::new(&env.deps).execute().await.unwrap();
        assert!(subjects(&env).await.is_empty(), "settled, it goes by date");
    }
}
