//! The sweep that settles pending areas when nobody asks: an area whose debt claim went stale is
//! handed over, and a request to hand one over that nobody answered is escalated.

use std::sync::Arc;

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ProjectId;
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::handover::{Escalated, Handovers, Settled};
use crate::work::Work;

/// What one sweep of the pending areas did.
#[derive(Debug, Default)]
pub struct HandoverSweep {
    /// How many projects had a claim waiting for an area.
    pub projects: usize,
    /// How many areas were handed over.
    pub handed_over: usize,
    /// How many debt holders were asked to hand an area over.
    pub asked: usize,
    /// How many unanswered requests were escalated to the arbiters.
    pub escalated: usize,
    /// How many areas waited past the limit with no arbiter to escalate them to.
    pub unescalated: usize,
    /// How many waits ended without a handover.
    pub withdrawn: usize,
    /// The projects the sweep could not settle, each with why; the others were settled all the
    /// same.
    pub failed: Vec<(ProjectId, AppError)>,
}

/// Settles the pending areas of every project whose claims wait for one, each project in a
/// transaction of its own: what time alone changed is applied — an area a debt claim held until it
/// went stale moves to the claim waiting for it, a feature claim gone stale stops waiting — and
/// every request to hand an area over that waited the project's `yield_escalates_after` is
/// escalated to the project's arbiters, so an area is never taken silently and never waits for
/// ever. The service runs it on
/// a schedule; it acts for nobody.
pub struct TickHandovers {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl TickHandovers {
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

    /// Sweeps every project with a claim waiting for an area once.
    ///
    /// # Errors
    /// [`AppError::Store`] when those projects cannot be listed; a project that fails is in the
    /// report instead.
    pub async fn execute(&self) -> Result<HandoverSweep, AppError> {
        let mut tx = self.store.begin().await?;
        let projects = tx.pending_projects().await?;
        drop(tx);
        let mut report = HandoverSweep {
            projects: projects.len(),
            ..HandoverSweep::default()
        };
        for project in projects {
            match self.sweep(project).await {
                Ok((settled, escalated)) => {
                    report.handed_over += settled.handed_over.len();
                    report.asked += settled.asked.len();
                    report.withdrawn += settled.withdrawn.len();
                    report.escalated += escalated.escalated;
                    report.unescalated += escalated.unescalated;
                }
                Err(error) => report.failed.push((project, error)),
            }
        }
        Ok(report)
    }

    async fn sweep(&self, project: ProjectId) -> Result<(Settled, Escalated), AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_unattended(&*self.store, project).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let handovers = Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        let settled = handovers.settle(&mut work).await?;
        let escalated = handovers.escalate_due(&mut work).await?;
        work.commit(&*self.notifier).await?;
        Ok((settled, escalated))
    }
}
