//! The sweep that keeps the order of each collision between two live claims in step with those
//! claims.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ProjectId;
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::radar::Radar;
use crate::work::Work;

/// What one sweep of the overlaps' orders did.
#[derive(Debug, Default)]
pub struct OverlapSweep {
    /// How many projects were swept; the ones that failed are in `failed` instead.
    pub projects: usize,
    /// How many collisions got a new order.
    pub reordered: usize,
    /// The projects the sweep could not sweep, each with why; the others were swept all the same.
    pub failed: Vec<(ProjectId, AppError)>,
}

/// Works out again, in every project, the order of each stored same-file or contract overlap whose
/// claims are all live, from the claims and their items' priorities as they are now: a claim
/// entering review or an item's priority changing shows in which claim lands first. A changed order
/// is stored and told to both holders in their claims' conversations, and a change to a tie is
/// taken to the project's arbiters first. An overlap found before orders were stored gets its
/// first order here, told as a first notice.
///
/// Each project is swept in a transaction of its own; a project whose orders all hold is committed
/// without a write. The service runs it on a schedule; it acts for nobody.
pub struct TickOverlaps {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl TickOverlaps {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Sweeps every project once.
    ///
    /// # Errors
    /// [`AppError::Store`] when the projects cannot be listed; a project that fails, its stored
    /// settings not parsing included, is in the report instead.
    pub async fn execute(&self) -> Result<OverlapSweep, AppError> {
        let mut tx = self.store.read().await?;
        let projects = tx.project_ids().await?;
        drop(tx);
        let mut report = OverlapSweep::default();
        for project in projects {
            match self.sweep(project).await {
                Ok(reordered) => {
                    report.projects += 1;
                    report.reordered += reordered;
                }
                Err(error) => report.failed.push((project, error)),
            }
        }
        Ok(report)
    }

    /// Works out again the project's orders, answering how many changed.
    async fn sweep(&self, project: ProjectId) -> Result<usize, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_unattended(&*self.store, project).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let reordered = Radar {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        }
        .reorder(&mut work)
        .await?;
        if reordered == 0 {
            work.commit_quietly().await?;
        } else {
            work.commit(&*self.notifier).await?;
        }
        Ok(reordered)
    }
}
