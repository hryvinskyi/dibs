//! The daily retention: conversations quiet past their date, old events and ended browser sessions
//! removed while the server serves, once when it starts and then once a day.

use std::sync::Arc;
use std::time::Duration;

use dibs_app::use_cases::UseCases;

use crate::stream::StopSignal;

/// How often the retention runs.
pub const RETAIN_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Runs one pass at once and then one every `every` until `stop` is given. A project that fails is
/// logged and the others are applied; a pass already running when the stop comes is finished first.
pub async fn run_retention(app: Arc<UseCases>, every: Duration, mut stop: StopSignal) {
    tracing::info!(every_s = every.as_secs(), "the retention is scheduled");
    loop {
        match app.prune.execute().await {
            Ok(report) => {
                for (project, error) in &report.failed {
                    tracing::error!(project = %project, code = error.code(), error = ?error, "the retention could not be applied to a project");
                }
                for (project, counts) in &report.pruned {
                    tracing::info!(
                        project = %project,
                        threads = counts.threads,
                        messages = counts.messages,
                        events = counts.events,
                        "records past their date were removed"
                    );
                }
                if report.ui_sessions > 0 {
                    tracing::debug!(
                        ui_sessions = report.ui_sessions,
                        "ended browser sessions were removed"
                    );
                }
            }
            Err(error) => {
                tracing::error!(code = error.code(), error = ?error, "the retention could not list the projects");
            }
        }
        tokio::select! {
            () = tokio::time::sleep(every) => {}
            () = stop.stopped() => return,
        }
    }
}
