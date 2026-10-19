//! The conversations the caller may read, optionally about one work item.

use std::sync::Arc;

use dibs_domain::ids::ItemId;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::{NamedSummary, Names};
use crate::ports::Store;
use crate::use_cases::readable::readable_summaries;
use crate::work::Work;

/// Lists the threads the caller may read — about `item` only, when given — newest activity first.
pub struct ListThreads {
    store: Arc<dyn Store>,
}

impl ListThreads {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
        }
    }

    /// The threads.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        item: Option<ItemId>,
    ) -> Result<Vec<NamedSummary>, AppError> {
        actor.require_person()?;
        let mut work = Work::begin(&*self.store, actor.project).await?;
        let summaries = work
            .thread_summaries(actor.project, actor.member, item)
            .await?;
        let names = Names::load(&mut work).await?;
        readable_summaries(actor, summaries)
            .into_iter()
            .map(|summary| names.summary(summary))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::handle::Handle;
    use dibs_domain::message::{Body, SessionLabel, Subject};

    use super::*;
    use crate::use_cases::testing::Env;
    use crate::use_cases::{Addressees, StartThread, StartThreadInput};

    #[tokio::test]
    async fn threads_filter_by_item_and_hide_private_ones() {
        let env = Env::new().await;
        let item = env.item("gh:acme/widgets#1").await;
        let start = StartThread::new(&env.deps);
        let thread = |to: Addressees, item: Option<ItemId>| StartThreadInput {
            subject: Subject::parse("About it").unwrap(),
            to,
            item,
            label: SessionLabel::parse("web").unwrap(),
            body: Body::parse("hello").unwrap(),
        };
        let about = start
            .execute(&env.alice.actor, thread(Addressees::Project, Some(item)))
            .await
            .unwrap();
        let private = start
            .execute(
                &env.alice.actor,
                thread(
                    Addressees::Members(vec![Handle::parse("alice").unwrap()]),
                    Some(item),
                ),
            )
            .await
            .unwrap();
        start
            .execute(&env.alice.actor, thread(Addressees::Project, None))
            .await
            .unwrap();
        let list = ListThreads::new(&env.deps);
        let bobs: Vec<_> = list
            .execute(&env.bob.actor, Some(item))
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.thread.record.id)
            .collect();
        assert_eq!(bobs, [about.thread.record.id]);
        let alices = list.execute(&env.alice.actor, Some(item)).await.unwrap();
        assert_eq!(alices.len(), 2);
        assert!(
            alices
                .iter()
                .any(|s| s.thread.record.id == private.thread.record.id)
        );
        assert_eq!(list.execute(&env.bob.actor, None).await.unwrap().len(), 2);
    }
}
