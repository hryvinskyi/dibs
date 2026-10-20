//! Snapshots of the database: consistent copies written into a directory while the server serves,
//! on a schedule and on an operator's command, of which the newest few, and none older than
//! [`MAX_AGE`] but the newest, are kept.
//!
//! A snapshot is named after the moment it was taken, `dibs-<UTC timestamp>.db`, so the names sort
//! by age and a person can tell them apart. It is first written under a hidden temporary name,
//! flushed to the disk, and only then renamed into place, so a snapshot that carries its final
//! name is always whole. A temporary file a crash left behind is removed once it is older than
//! [`PARTIAL_AFTER`]. The directory holds other files too; only the names of these two forms count,
//! and only those are ever deleted.

use std::fs::File;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use dibs_app::ports::Clock;
use dibs_store_sqlite::{MaintenanceError, SqliteStore};

use crate::config::SnapshotConfig;
use crate::stream::StopSignal;

const PREFIX: &str = "dibs-";
const SUFFIX: &str = ".db";
const TEMPORARY_SUFFIX: &str = ".partial";

/// How old a snapshot may be and still be kept, unless it is the newest.
pub const MAX_AGE: time::Duration = time::Duration::days(30);

/// How old a temporary file of a snapshot that was never finished may be before it is removed: a
/// snapshot being written is far younger, so only what a crash left behind is that old.
pub const PARTIAL_AFTER: time::Duration = time::Duration::hours(1);

/// The name of one snapshot: the moment it was taken, to the millisecond, in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotName {
    taken_at: OffsetDateTime,
}

impl SnapshotName {
    /// The name of a snapshot taken at `at`, kept to the millisecond.
    pub fn taken(at: OffsetDateTime) -> Self {
        let at = at.to_offset(time::UtcOffset::UTC);
        let millis = at.millisecond();
        let taken_at = at
            .replace_nanosecond(u32::from(millis) * 1_000_000)
            .unwrap_or(at);
        Self { taken_at }
    }

    /// The snapshot a file of this name is, or `None` for any other file.
    pub fn parse(file_name: &str) -> Option<Self> {
        let stamp = file_name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
        let bytes = stamp.as_bytes();
        if bytes.len() != 20
            || bytes[8] != b'T'
            || bytes[15] != b'.'
            || bytes[19] != b'Z'
            || !stamp.is_ascii()
        {
            return None;
        }
        let number = |from: usize, to: usize| -> Option<u32> {
            let digits = stamp.get(from..to)?;
            if digits.bytes().all(|b| b.is_ascii_digit()) {
                digits.parse().ok()
            } else {
                None
            }
        };
        let year = i32::try_from(number(0, 4)?).ok()?;
        let month = Month::try_from(u8::try_from(number(4, 6)?).ok()?).ok()?;
        let day = u8::try_from(number(6, 8)?).ok()?;
        let hour = u8::try_from(number(9, 11)?).ok()?;
        let minute = u8::try_from(number(11, 13)?).ok()?;
        let second = u8::try_from(number(13, 15)?).ok()?;
        let milli = u16::try_from(number(16, 19)?).ok()?;
        let date = Date::from_calendar_date(year, month, day).ok()?;
        let time = Time::from_hms_milli(hour, minute, second, milli).ok()?;
        Some(Self {
            taken_at: PrimitiveDateTime::new(date, time).assume_utc(),
        })
    }

    /// When the snapshot was taken.
    pub fn taken_at(&self) -> OffsetDateTime {
        self.taken_at
    }

    /// Its file name, such as `dibs-20260302T090000.000Z.db`.
    pub fn file_name(&self) -> String {
        let at = self.taken_at;
        format!(
            "{PREFIX}{:04}{:02}{:02}T{:02}{:02}{:02}.{:03}Z{SUFFIX}",
            at.year(),
            u8::from(at.month()),
            at.day(),
            at.hour(),
            at.minute(),
            at.second(),
            at.millisecond()
        )
    }

    /// The hidden name it is written under before it is complete.
    fn temporary_name(&self) -> String {
        format!(".{}{TEMPORARY_SUFFIX}", self.file_name())
    }

    /// The snapshot a temporary file of this name was being written for, or `None` for any other
    /// file.
    pub fn parse_temporary(file_name: &str) -> Option<Self> {
        Self::parse(
            file_name
                .strip_prefix('.')?
                .strip_suffix(TEMPORARY_SUFFIX)?,
        )
    }
}

/// Which snapshots are kept: the newest `keep` of them, of which none older than `max_age` — but
/// the newest snapshot, whatever its age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    keep: NonZeroUsize,
    max_age: time::Duration,
}

impl Retention {
    /// Keeps the newest `keep` snapshots that are at most `max_age` old, and the newest always.
    pub fn new(keep: NonZeroUsize, max_age: time::Duration) -> Self {
        Self { keep, max_age }
    }

    /// The snapshots of `present` to delete at `now`, oldest first. The newest snapshot is never
    /// among them.
    pub fn doomed(&self, present: &[SnapshotName], now: OffsetDateTime) -> Vec<SnapshotName> {
        let mut sorted = present.to_vec();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        sorted.dedup();
        let mut doomed: Vec<SnapshotName> = sorted
            .into_iter()
            .enumerate()
            .filter(|(place, name)| {
                *place > 0 && (*place >= self.keep.get() || now - name.taken_at() > self.max_age)
            })
            .map(|(_, name)| name)
            .collect();
        doomed.reverse();
        doomed
    }
}

/// A snapshot written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    /// Its file.
    pub path: PathBuf,
    /// Its size in bytes.
    pub bytes: u64,
    /// How long writing it took.
    pub duration: Duration,
    /// How many older snapshots were deleted after it.
    pub removed: usize,
}

/// Why a snapshot was not written. Each message is a sentence for the operator.
#[derive(Debug, Error)]
pub enum SnapshotError {
    /// The database could not be copied into the directory: it does not exist, is not writable,
    /// or the disk is full.
    #[error(
        "the database could not be copied into {dir}; check that the directory exists, is writable and has room"
    )]
    Copy {
        /// The snapshot directory.
        dir: PathBuf,
        /// What failed.
        #[source]
        source: MaintenanceError,
    },
    /// A snapshot of the same millisecond exists.
    #[error("a snapshot named {0} exists already")]
    Exists(PathBuf),
    /// The copy could not be flushed to the disk or renamed into place.
    #[error("the copy of the database could not be completed in {dir}")]
    Finish {
        /// The snapshot directory.
        dir: PathBuf,
        /// What failed.
        #[source]
        source: io::Error,
    },
}

/// Writes snapshots of one store into one directory and keeps the newest of them.
pub struct Snapshots {
    store: Arc<SqliteStore>,
    clock: Arc<dyn Clock>,
    dir: PathBuf,
    retention: Retention,
}

impl Snapshots {
    /// Snapshots of `store` as `config` says, named by `clock`.
    pub fn new(store: Arc<SqliteStore>, clock: Arc<dyn Clock>, config: &SnapshotConfig) -> Self {
        Self {
            store,
            clock,
            dir: config.dir.clone(),
            retention: Retention::new(config.keep, MAX_AGE),
        }
    }

    /// The directory the snapshots are written into.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes one snapshot now, then deletes the snapshots past the retention. Writers carry on
    /// while it is written: the copy is a read of one consistent state of the database.
    ///
    /// # Errors
    /// [`SnapshotError`] saying what kept the snapshot from being written; nothing is deleted then.
    pub async fn take(&self) -> Result<Taken, SnapshotError> {
        let started = Instant::now();
        let name = SnapshotName::taken(self.clock.now());
        let path = self.dir.join(name.file_name());
        if path.exists() {
            return Err(SnapshotError::Exists(path));
        }
        let temporary = self.dir.join(name.temporary_name());
        remove_if_present(&temporary);
        if let Err(source) = self.store.vacuum_into(&temporary).await {
            remove_if_present(&temporary);
            return Err(SnapshotError::Copy {
                dir: self.dir.clone(),
                source,
            });
        }
        let bytes = match finish(&temporary, &path) {
            Ok(bytes) => bytes,
            Err(source) => {
                remove_if_present(&temporary);
                return Err(SnapshotError::Finish {
                    dir: self.dir.clone(),
                    source,
                });
            }
        };
        let duration = started.elapsed();
        let removed = self.prune();
        tracing::info!(
            snapshot = %path.display(),
            bytes,
            duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            removed,
            "snapshot written"
        );
        Ok(Taken {
            path,
            bytes,
            duration,
            removed,
        })
    }

    /// The snapshots in the directory, by name; other files are not listed.
    ///
    /// # Errors
    /// The failure to read the directory.
    pub fn present(&self) -> io::Result<Vec<SnapshotName>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str().and_then(SnapshotName::parse) {
                names.push(name);
            }
        }
        names.sort_unstable();
        Ok(names)
    }

    /// How long to wait for the first scheduled snapshot: until one interval after the newest
    /// snapshot in the directory, at once when there is none or it is older than that, and never
    /// longer than one interval, even when the newest one claims to be from the future.
    pub fn first_wait(&self, every: Duration) -> Duration {
        let newest = match self.present() {
            Ok(names) => names.last().copied(),
            Err(error) => {
                tracing::warn!(dir = %self.dir.display(), error = %error, "the snapshot directory cannot be read");
                None
            }
        };
        let Some(newest) = newest else {
            return Duration::ZERO;
        };
        let since = self.clock.now() - newest.taken_at();
        let since = Duration::try_from(since).unwrap_or(Duration::ZERO);
        every.saturating_sub(since).min(every)
    }

    /// The temporary files of snapshots that were never finished, older than [`PARTIAL_AFTER`] by
    /// the clock, oldest first: what a crash while writing one left behind.
    ///
    /// # Errors
    /// The failure to read the directory.
    pub fn abandoned(&self) -> io::Result<Vec<PathBuf>> {
        let now = self.clock.now();
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let Some(name) = entry
                .file_name()
                .to_str()
                .and_then(SnapshotName::parse_temporary)
            else {
                continue;
            };
            if now - name.taken_at() > PARTIAL_AFTER {
                found.push((name, entry.path()));
            }
        }
        found.sort_unstable_by_key(|(name, _)| *name);
        Ok(found.into_iter().map(|(_, path)| path).collect())
    }

    /// Deletes the snapshots past the retention and the temporary files a crash left behind, and
    /// answers how many snapshots went. A file that cannot be deleted is logged and left for the
    /// next time.
    fn prune(&self) -> usize {
        match self.abandoned() {
            Ok(abandoned) => {
                for path in abandoned {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {
                            tracing::info!(file = %path.display(), "an unfinished snapshot left by an interrupted run was removed");
                        }
                        Err(error) => {
                            tracing::warn!(file = %path.display(), error = %error, "an unfinished snapshot could not be removed");
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(dir = %self.dir.display(), error = %error, "unfinished snapshots could not be listed; none were removed");
            }
        }
        let present = match self.present() {
            Ok(present) => present,
            Err(error) => {
                tracing::warn!(dir = %self.dir.display(), error = %error, "old snapshots could not be listed; none were deleted");
                return 0;
            }
        };
        let mut removed = 0;
        for name in self.retention.doomed(&present, self.clock.now()) {
            let path = self.dir.join(name.file_name());
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) => {
                    tracing::warn!(snapshot = %path.display(), error = %error, "an old snapshot could not be deleted");
                }
            }
        }
        removed
    }
}

/// Takes a snapshot every `every` until `stop` is given: the first as [`Snapshots::first_wait`]
/// says, each next one an interval after the one before began, whether that one succeeded or not.
/// A failed snapshot is logged and the server carries on; the next tick tries again. A snapshot
/// already being written when the stop comes is finished first.
pub async fn run_schedule(snapshots: Arc<Snapshots>, every: Duration, mut stop: StopSignal) {
    let mut wait = snapshots.first_wait(every);
    tracing::info!(
        dir = %snapshots.dir().display(),
        every_s = every.as_secs(),
        first_in_s = wait.as_secs(),
        "snapshots scheduled"
    );
    loop {
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = stop.stopped() => return,
        }
        let began = Instant::now();
        if let Err(error) = snapshots.take().await {
            tracing::error!(error = %error, cause = ?error, "the snapshot failed; it is tried again at the next interval");
        }
        wait = every.saturating_sub(began.elapsed());
    }
}

/// Flushes the copy at `temporary` to the disk, renames it to `path` and answers its size.
fn finish(temporary: &Path, path: &Path) -> io::Result<u64> {
    let file = File::open(temporary)?;
    file.sync_all()?;
    let bytes = file.metadata()?.len();
    drop(file);
    std::fs::rename(temporary, path)?;
    sync_parent(path)?;
    Ok(bytes)
}

/// Flushes the directory entry of `path`, so a rename survives a power loss. Only some systems can
/// open a directory for that; elsewhere the rename is left to the file system.
#[cfg(unix)]
pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => File::open(parent)?.sync_all(),
        _ => Ok(()),
    }
}

/// Flushes the directory entry of `path`, so a rename survives a power loss. Only some systems can
/// open a directory for that; elsewhere the rename is left to the file system.
#[cfg(not(unix))]
pub(crate) fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Removes a file left by a copy that did not complete, if there is one.
fn remove_if_present(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(file = %path.display(), error = %error, "an incomplete snapshot could not be removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    fn name(at: OffsetDateTime) -> SnapshotName {
        SnapshotName::taken(at)
    }

    #[test]
    fn a_name_reads_back_as_the_moment_it_was_taken() {
        let taken = name(datetime!(2026-03-02 09:05:07.123456 UTC));
        assert_eq!(taken.file_name(), "dibs-20260302T090507.123Z.db");
        assert_eq!(taken.taken_at(), datetime!(2026-03-02 09:05:07.123 UTC));
        assert_eq!(SnapshotName::parse(&taken.file_name()), Some(taken));
        assert_eq!(
            taken.temporary_name(),
            ".dibs-20260302T090507.123Z.db.partial"
        );
    }

    #[test]
    fn a_moment_in_another_offset_is_named_in_utc() {
        let taken = name(datetime!(2026-03-02 11:00:00 +02:00));
        assert_eq!(taken.file_name(), "dibs-20260302T090000.000Z.db");
    }

    #[test]
    fn names_of_other_files_are_not_snapshots() {
        for other in [
            "dibs.db",
            "dibs-20260302T090507.123Z.db.partial",
            ".dibs-20260302T090507.123Z.db.partial",
            "dibs-20260302T090507Z.db",
            "dibs-20261302T090507.123Z.db",
            "dibs-20260230T090507.123Z.db",
            "dibs-2026030xT090507.123Z.db",
            "dibs-+0260302T090507.123Z.db",
            "dibs-20260302T250507.123Z.db",
            "notes.txt",
        ] {
            assert_eq!(SnapshotName::parse(other), None, "{other}");
        }
    }

    #[test]
    fn names_sort_by_age() {
        let older = name(datetime!(2026-03-02 09:00:00.999 UTC));
        let newer = name(datetime!(2026-03-02 09:00:01 UTC));
        assert!(older < newer);
        assert!(older.file_name() < newer.file_name());
    }

    #[test]
    fn retention_deletes_all_but_the_newest_oldest_first() {
        let names: Vec<SnapshotName> = (0..5)
            .map(|hour| name(datetime!(2026-03-02 00:00 UTC) + time::Duration::hours(hour)))
            .collect();
        let now = datetime!(2026-03-02 05:00 UTC);
        let shuffled = vec![names[3], names[0], names[4], names[1], names[2]];
        let keep = |n| Retention::new(NonZeroUsize::new(n).unwrap(), MAX_AGE);
        assert_eq!(keep(2).doomed(&shuffled, now), names[..3].to_vec());
        assert!(keep(5).doomed(&shuffled, now).is_empty());
        assert_eq!(keep(1).doomed(&shuffled, now), names[..4].to_vec());
        assert!(keep(1).doomed(&[], now).is_empty());
    }

    #[test]
    fn retention_deletes_what_is_older_than_its_age_on_top_of_the_count_but_never_the_newest() {
        let day = |n: i64| name(datetime!(2026-03-01 00:00 UTC) + time::Duration::days(n));
        let names: Vec<SnapshotName> = (0..40).map(day).collect();
        let retention = Retention::new(NonZeroUsize::new(35).unwrap(), MAX_AGE);
        let now = datetime!(2026-03-01 00:00 UTC) + time::Duration::days(40);
        let doomed = retention.doomed(&names, now);
        assert_eq!(
            doomed,
            names[..10].to_vec(),
            "the count keeps 35, and of those the five past 30 days go too"
        );
        let years_later = now + time::Duration::days(400);
        assert_eq!(
            retention.doomed(&names, years_later),
            names[..39].to_vec(),
            "the newest is kept, however old it is"
        );
        assert_eq!(
            retention.doomed(&names[39..], years_later),
            Vec::<SnapshotName>::new()
        );
    }

    #[test]
    fn a_temporary_name_reads_back_as_the_snapshot_it_was_for() {
        let taken = name(datetime!(2026-03-02 09:05:07.123 UTC));
        assert_eq!(
            SnapshotName::parse_temporary(&taken.temporary_name()),
            Some(taken)
        );
        for other in [
            "dibs-20260302T090507.123Z.db",
            "dibs-20260302T090507.123Z.db.partial",
            ".dibs-20260302T090507.123Z.db",
            ".notes.partial",
        ] {
            assert_eq!(SnapshotName::parse_temporary(other), None, "{other}");
        }
    }
}
