//! A person removing that one work item is blocked by another.

use std::sync::Arc;

use dibs_domain::event::{DependencyRemoved, EventKind};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::Names;
use crate::ports::{Clock, EventNotifier, Store};
use crate::use_cases::add_dependency::{DependencyChange, DependencyInput};
use crate::use_cases::common::existing_item;
use crate::work::Work;

/// Removes, for a person, the edge between two items, whoever recorded it. An edge an import
/// reported comes back with the next import that reports it. Removing an edge that is not there
/// changes nothing and appends nothing.
pub struct RemoveDependency {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    notifier: Arc<dyn EventNotifier>,
}

impl RemoveDependency {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            notifier: deps.notifier.clone(),
        }
    }

    /// Removes the edge.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an item the project
    /// does not have; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: DependencyInput,
    ) -> Result<DependencyChange, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin(&*self.store, project).await?;
        let item = existing_item(&mut work, input.item).await?;
        let blocked_by = existing_item(&mut work, input.blocked_by).await?;
        let changed = work
            .dependencies(project)
            .await?
            .iter()
            .any(|edge| edge.item == item.id && edge.blocked_by == blocked_by.id);
        if changed {
            work.delete_dependency(project, item.id, blocked_by.id)
                .await?;
            work.record(
                now,
                EventKind::DependencyRemoved(DependencyRemoved {
                    item: item.id,
                    blocked_by: blocked_by.id,
                    by: actor.member,
                }),
            )
            .await?;
        }
        let names = Names::load(&mut work).await?;
        let change = DependencyChange {
            item: names.item(item)?,
            blocked_by: names.item(blocked_by)?,
            changed,
        };
        work.commit(&*self.notifier).await?;
        Ok(change)
    }
}
