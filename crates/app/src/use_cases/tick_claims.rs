//! The sweep that releases claims whose work stopped showing.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use time::Duration;

use dibs_domain::claim::due_for_release;
use dibs_domain::event::{EventKind, ReleasedStale};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ProjectId;
use dibs_domain::item::ItemStatus;
use dibs_domain::message::Importance;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::settings::format_duration;

use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::ItemName;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::claim_notice::ClaimNotice;
use crate::use_cases::common::existing_item;
use crate::use_cases::handover::Handovers;
use crate::use_cases::writing::titled_body;
use crate::work::Work;

/// What one sweep of the claims did.
#[derive(Debug, Default)]
pub struct ClaimSweep {
    /// How many projects were swept; the ones that failed are in `failed` instead.
    pub projects: usize,
    /// How many claims were released.
    pub released: usize,
    /// The projects the sweep could not sweep, each with why; the others were swept all the same.
    pub failed: Vec<(ProjectId, AppError)>,
}

/// Releases, in every project, each working claim that went the project's `release_after` without
/// a sign of its own work: the item is open again, `released_stale` is recorded, what the claim
/// held or waited for is let go as any ending lets it go, and its holder is told in the claim's
/// conversation. Each project is swept in a transaction of its own, and the claims are read inside
/// it, so a request that showed a claim's work before the sweep took the project keeps that claim.
/// A project with no claim due is left without a write. The service runs it on a schedule; it acts
/// for nobody.
pub struct TickClaims {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl TickClaims {
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
    pub async fn execute(&self) -> Result<ClaimSweep, AppError> {
        let mut tx = self.store.read().await?;
        let projects = tx.project_ids().await?;
        drop(tx);
        let mut report = ClaimSweep::default();
        for project in projects {
            match self.sweep(project).await {
                Ok(released) => {
                    report.projects += 1;
                    report.released += released;
                }
                Err(error) => report.failed.push((project, error)),
            }
        }
        Ok(report)
    }

    /// Releases the project's due claims, answering how many.
    async fn sweep(&self, project: ProjectId) -> Result<usize, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_unattended(&*self.store, project).await?;
        let claims = work.claims(project).await?;
        if claims.is_empty() {
            work.commit_quietly().await?;
            return Ok(0);
        }
        let settings = work.project_settings(&self.hot_rules).await?;
        let release_after = settings.release_after();
        let due: Vec<_> = due_for_release(&claims, now, release_after)
            .into_iter()
            .map(|claim| claim.item)
            .collect();
        if due.is_empty() {
            work.commit_quietly().await?;
            return Ok(0);
        }
        let handovers = Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        let mut released = 0;
        for item in due {
            // Ending one claim may hand areas to another, so each is read again before it ends.
            let Some(claim) = work
                .claim(project, item)
                .await?
                .filter(|claim| claim.is_due_for_release(now, release_after))
            else {
                continue;
            };
            let ending = Ending {
                status: ItemStatus::Open,
                event: EventKind::ReleasedStale(ReleasedStale {
                    item: claim.item,
                    member: claim.member,
                    last_seen: claim.last_seen,
                }),
            };
            end_claim(&mut work, &claim, ending, &handovers).await?;
            let named = ItemName::of(&existing_item(&mut work, item).await?, &self.ref_schemes);
            ClaimNotice {
                claim: &claim,
                item: &named,
                body: titled_body(&released_stale(&named, release_after), None)?,
                importance: Importance::Normal,
            }
            .send(&mut work, &*self.ids, now)
            .await?;
            released += 1;
        }
        work.commit(&*self.notifier).await?;
        Ok(released)
    }
}

/// What the holder of a claim the sweep released is told: that dibs released it, after how long
/// without a sign of its work and which signs those are, that the item is open again, and how to
/// claim it again.
fn released_stale(item: &ItemName, after: Duration) -> String {
    format!(
        "Released: dibs released your claim on work item {label} \"{title}\" because nothing of its work showed for {after}: no edit made under it, no push of its branch, no renewal. The item is open again. If the work goes on, claim it again (dibs claim {key}).",
        label = item.label(),
        title = item.title,
        after = format_duration(after),
        key = item.key(),
    )
}
