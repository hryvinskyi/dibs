//! The sweep: what time alone changes, applied on a schedule so that it happens even when nobody
//! touches what it changes. For slots — a hold that expires or stalls, an offer that lapses, a
//! waiter who went silent, the next waiter's offer — so the first waiter is offered a freed slot
//! and its followers are woken. For worktree pools — a hold whose holder went silent for longer
//! than its kind of holder may ends, so the board stops showing it. For the areas feature claims
//! wait for — an area whose debt claim went stale moves to the claim waiting for it, and a request
//! to hand one over that nobody answered within the project's `yield_escalates_after` goes to the
//! project's arbiters.

use std::sync::Arc;
use std::time::Duration;

use dibs_app::use_cases::UseCases;

use crate::stream::StopSignal;

/// How often the sweep runs.
pub const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// Sweeps every `every` until `stop` is given: the slots first, then the worktree pools, then the
/// waiting areas. A sweep that fails for one project, or one pool, is logged and the others are
/// applied; a sweep already running when the stop comes is finished first.
pub async fn run_sweeper(app: Arc<UseCases>, every: Duration, mut stop: StopSignal) {
    tracing::info!(every_s = every.as_secs(), "the sweep is scheduled");
    loop {
        tokio::select! {
            () = tokio::time::sleep(every) => {}
            () = stop.stopped() => return,
        }
        sweep_slots(&app).await;
        sweep_worktrees(&app).await;
        sweep_handovers(&app).await;
    }
}

async fn sweep_slots(app: &UseCases) {
    match app.tick_slots.execute().await {
        Ok(report) => {
            for (project, error) in &report.failed {
                tracing::error!(project = %project, code = error.code(), error = ?error, "the slots of a project could not be swept");
            }
            if report.events > 0 {
                tracing::debug!(
                    projects = report.projects,
                    events = report.events,
                    "the slots were swept"
                );
            }
        }
        Err(error) => {
            tracing::error!(code = error.code(), error = ?error, "the slot sweep could not list the projects to sweep");
        }
    }
}

async fn sweep_worktrees(app: &UseCases) {
    match app.tick_worktrees.execute().await {
        Ok(report) => {
            for (project, _, error) in &report.failed {
                tracing::error!(project = %project, code = error.code(), error = ?error, "a worktree pool could not be swept");
            }
            if report.events > 0 {
                tracing::debug!(
                    pools = report.pools,
                    events = report.events,
                    "the worktree pools were swept"
                );
            }
        }
        Err(error) => {
            tracing::error!(code = error.code(), error = ?error, "the worktree sweep could not list the pools to sweep");
        }
    }
}

async fn sweep_handovers(app: &UseCases) {
    match app.tick_handovers.execute().await {
        Ok(report) => {
            for (project, error) in &report.failed {
                tracing::error!(project = %project, code = error.code(), error = ?error, "the waiting areas of a project could not be swept");
            }
            if report.unescalated > 0 {
                tracing::warn!(
                    areas = report.unescalated,
                    "requests to hand areas over waited past the project's limit and reached no arbiter: name arbiters in the settings"
                );
            }
            if report.handed_over + report.asked + report.escalated + report.withdrawn > 0 {
                tracing::debug!(
                    projects = report.projects,
                    handed_over = report.handed_over,
                    asked = report.asked,
                    escalated = report.escalated,
                    withdrawn = report.withdrawn,
                    "the waiting areas were swept"
                );
            }
        }
        Err(error) => {
            tracing::error!(code = error.code(), error = ?error, "the sweep could not list the projects whose claims wait for areas");
        }
    }
}
