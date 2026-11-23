//! The sweep: what time alone changes, applied on a schedule so that it happens even when nobody
//! touches what it changes. For slots — a hold that expires or stalls, an offer that lapses, a
//! waiter who went silent, the next waiter's offer — so the first waiter is offered a freed slot
//! and its followers are woken. For worktree pools — a hold whose holder went silent for longer
//! than its kind of holder may ends, and the event that says so reaches every member, whose boards
//! stop showing it. For the areas feature claims
//! wait for — an area whose debt claim went stale moves to the claim waiting for it, and a request
//! to hand one over that nobody answered within the project's `yield_escalates_after` goes to the
//! project's arbiters. For claims — a working claim that went the project's `release_after`
//! without a sign of its own work is released, its item open again and its holder told; and an area
//! of a live claim that went the project's `idle_area_after` with no sign of the claim's work in it
//! is given back, its holder told. For requests for grants — dibs decides each request still
//! waiting on its holder's behalf, and grants it once the holder's claim is in review, the area is
//! idle on that claim or untouched by its branch, or the project's `grant_after` has passed since
//! it was asked. For overlaps — the order of each collision between two live claims is worked out
//! again from the claims as they are now, a change is told to both holders, and a change to a tie
//! goes to the project's arbiters.

use std::sync::Arc;
use std::time::Duration;

use dibs_app::use_cases::UseCases;

use crate::stream::StopSignal;

/// How often the sweep runs.
pub const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// Sweeps every `every` until `stop` is given: the slots first, then the worktree pools, then the
/// waiting areas, then the claims, then the requests for grants, then the overlaps' orders. A
/// sweep that fails for one project, or one pool, is logged and the others are applied; a sweep
/// already running when the stop comes is finished first.
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
        sweep_claims(&app).await;
        sweep_grants(&app).await;
        sweep_overlaps(&app).await;
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

async fn sweep_claims(app: &UseCases) {
    match app.tick_claims.execute().await {
        Ok(report) => {
            for (project, error) in &report.failed {
                tracing::error!(project = %project, code = error.code(), error = ?error, "the claims of a project could not be swept");
            }
            if report.released > 0 || report.returned > 0 {
                tracing::debug!(
                    projects = report.projects,
                    released = report.released,
                    returned = report.returned,
                    "the claims were swept"
                );
            }
        }
        Err(error) => {
            tracing::error!(code = error.code(), error = ?error, "the claim sweep could not list the projects to sweep");
        }
    }
}

async fn sweep_grants(app: &UseCases) {
    match app.tick_grants.execute().await {
        Ok(report) => {
            for (project, error) in &report.failed {
                tracing::error!(project = %project, code = error.code(), error = ?error, "the requests for grants of a project could not be swept");
            }
            if report.granted > 0
                || report.settled > 0
                || report.acknowledged > 0
                || report.left_to_holders > 0
                || report.announcements_closed > 0
            {
                tracing::debug!(
                    projects = report.projects,
                    granted = report.granted,
                    settled = report.settled,
                    acknowledged = report.acknowledged,
                    left_to_holders = report.left_to_holders,
                    announcements_closed = report.announcements_closed,
                    "the requests for grants and the announcements of wide claims were swept"
                );
            }
        }
        Err(error) => {
            tracing::error!(code = error.code(), error = ?error, "the grant sweep could not list the projects to sweep");
        }
    }
}

async fn sweep_overlaps(app: &UseCases) {
    match app.tick_overlaps.execute().await {
        Ok(report) => {
            for (project, error) in &report.failed {
                tracing::error!(project = %project, code = error.code(), error = ?error, "the overlaps of a project could not be swept");
            }
            if report.reordered > 0 {
                tracing::debug!(
                    projects = report.projects,
                    reordered = report.reordered,
                    "the overlaps' orders were swept"
                );
            }
        }
        Err(error) => {
            tracing::error!(code = error.code(), error = ?error, "the overlap sweep could not list the projects to sweep");
        }
    }
}
