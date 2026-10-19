//! The hook durations of this machine, kept under the state directory in `hooks/timings.json`.
//! Hooks of several hosts and projects record into one file; a run that finds another one writing
//! leaves its own run out rather than wait.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use time::{Date, Duration};

use dibs_agent_host::EventKind;
use dibs_agent_host::timing::HookTimings;
use dibs_client::state::{read_json, write_json};

/// The directory of the hook files under the state directory.
fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("hooks")
}

/// The histograms recorded so far; none when nothing was recorded or the file is damaged.
pub fn load(state_dir: &Path) -> HookTimings {
    read_json(&dir(state_dir).join("timings.json")).unwrap_or_default()
}

/// Records one hook run.
///
/// # Errors
/// The file system's failure.
pub fn record(
    state_dir: &Path,
    day: Date,
    host: &str,
    kind: EventKind,
    took: Duration,
) -> std::io::Result<()> {
    let dir = dir(state_dir);
    fs::create_dir_all(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("timings.lock"))?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    let mut timings = load(state_dir);
    timings.record(day, host, kind, took);
    write_json(&dir.join("timings.json"), &timings)
}

#[cfg(test)]
mod tests {
    use time::Month;

    use super::*;

    #[test]
    fn runs_are_added_to_what_was_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let day = Date::from_calendar_date(2026, Month::March, 2).unwrap();
        for millis in [4, 12] {
            record(
                dir.path(),
                day,
                "neutral",
                EventKind::AfterTool,
                Duration::milliseconds(millis),
            )
            .unwrap();
        }
        let timings = load(dir.path());
        let buckets = timings.day(day).unwrap()["neutral after_tool"];
        assert_eq!(buckets.total(), 2);
        assert_eq!(buckets.p95_bound_ms(), Some(30));
    }
}
