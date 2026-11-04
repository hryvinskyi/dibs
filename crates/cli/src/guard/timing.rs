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

/// Records one hook run; `false` when another process held the file's lock and the run was left
/// out, as a hook never waits to be counted.
///
/// # Errors
/// The file system's failure.
pub fn record(
    state_dir: &Path,
    day: Date,
    host: &str,
    kind: EventKind,
    took: Duration,
) -> std::io::Result<bool> {
    let dir = dir(state_dir);
    fs::create_dir_all(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("timings.lock"))?;
    if lock.try_lock().is_err() {
        return Ok(false);
    }
    let mut timings = load(state_dir);
    timings.record(day, host, kind, took);
    write_json(&dir.join("timings.json"), &timings)?;
    Ok(true)
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
            // A process another test starts at this moment can briefly share the lock file it
            // inherited, so a free lock may answer busy once; the run is counted on a later try.
            let recorded = (0..200).any(|_| {
                let recorded = record(
                    dir.path(),
                    day,
                    "neutral",
                    EventKind::AfterTool,
                    Duration::milliseconds(millis),
                )
                .unwrap();
                if !recorded {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                recorded
            });
            assert!(recorded, "the lock stayed busy for two seconds");
        }
        let timings = load(dir.path());
        let buckets = timings.day(day).unwrap()["neutral after_tool"];
        assert_eq!(buckets.total(), 2);
        assert_eq!(buckets.p95_bound_ms(), Some(30));
    }
}
