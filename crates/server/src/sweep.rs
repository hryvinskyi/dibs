//! The slot sweep: what time alone changes about slots — a hold that expires or stalls, an offer
//! that lapses, a waiter who went silent, the next waiter's offer — applied on a schedule, so the
//! first waiter is offered a freed slot and its followers are woken even when nobody touches it.

use std::sync::Arc;
use std::time::Duration;

use dibs_app::use_cases::UseCases;

use crate::stream::StopSignal;

/// How often the sweep runs.
pub const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// Sweeps every `every` until `stop` is given. A sweep that fails for one project is logged and the
/// others are applied; a sweep already running when the stop comes is finished first.
pub async fn run_sweeper(app: Arc<UseCases>, every: Duration, mut stop: StopSignal) {
    tracing::info!(every_s = every.as_secs(), "the slot sweep is scheduled");
    loop {
        tokio::select! {
            () = tokio::time::sleep(every) => {}
            () = stop.stopped() => return,
        }
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
}
