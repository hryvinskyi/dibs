//! What this machine counts for a project's reports, in the project's state:
//!
//! - `report.spool/` — what hooks counted and nobody folded yet: one small file per count, made by
//!   the hook without a lock and without syncing it to the disk, so a hook never waits on another
//!   process or on the disk. `report.spool/sessions/` holds one marker per session counted as having
//!   run switched off. The spool holds at most [`SPOOL_CAP`] counts; a count past that is dropped.
//! - `report.json` — the counts folded, per day, with what was last sent of each day; the ids of the
//!   latest refusals with their kinds, so feedback about one can say which kind it was; and the
//!   names of the spool files the last fold counted, so a fold that stopped before deleting them
//!   never counts them twice.
//!
//! The fold — every spool file into `report.json` — runs off the hook path, under the file's lock,
//! by `dibs sync` and by a command that reads the refusals: it writes `report.json` durably in one
//! step, then deletes the spool files it counted. How long hooks took is not kept here: a report
//! sends the machine's own hook timings ([`super::timing`]).
//!
//! Nothing here holds a path, a command line or a work item, and the sessions counted never leave
//! the machine. Days older than the week a heartbeat reports are forgotten. What this dibs cannot
//! read in `report.json` — a day another version wrote in a shape it does not know, a kind it does
//! not know — is kept as it is and written back, never wiped; a file that is not JSON at all is
//! moved aside as `report.json.unread` and the counting starts again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration as StdDuration;

use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::{Duration, OffsetDateTime};

use dibs_agent_host::ports::{SessionRef, Tallies};
use dibs_agent_host::timing::HookTimings;
use dibs_client::state::{StateDir, read_json, write_json, write_json_durably};
use dibs_domain::report::{
    DayCounts, DayReport, HeartbeatReports, HookDurations, HookEventName, RefusalId, ReportDay,
    Tally,
};
use dibs_domain::session::AgentLabel;

/// The name of the file and of its lock in the project's state.
const NAME: &str = "report";

/// How many days before today a day is kept.
const KEPT_DAYS_BACK: i64 = HeartbeatReports::MAX as i64 - 1;

/// How many refusal ids are remembered with their kinds.
const KEPT_REFUSALS: usize = 64;

/// The most counts the spool holds; a count past them is dropped.
pub const SPOOL_CAP: usize = 5_000;

/// How long a session's marker of having been counted switched off is kept.
const SESSION_MARKER_KEPT: StdDuration = StdDuration::from_secs(8 * 24 * 60 * 60);

/// How long a spool file a hook did not finish writing is kept before a fold removes it.
const UNFINISHED_KEPT: StdDuration = StdDuration::from_secs(60 * 60);

/// How many ids are tried for a refusal before it is counted without one.
const ID_CANDIDATES: u32 = 8;

/// How often a day whose counts did not change is sent again for its durations alone.
pub const DURATIONS_EVERY: Duration = Duration::hours(1);

/// How long a sync that sends counts waits, after one that failed, before another is started.
pub const REPORT_SYNC_BACKOFF: Duration = Duration::minutes(10);

/// How long a fold, or a command that changes `report.json`, waits for its lock. No hook takes it.
const LOCK_PATIENCE: StdDuration = StdDuration::from_secs(5);

/// The file, as this dibs reads it.
#[derive(Debug, Default)]
struct ReportFile {
    days: BTreeMap<ReportDay, LocalDay>,
    /// Entries of `days` this dibs cannot read, kept as they are.
    unread_days: Map<String, Value>,
    refusals: Vec<RememberedRefusal>,
    /// The spool files the last fold counted, by name.
    folded: BTreeSet<String>,
    /// The fields this dibs does not know, kept as they are.
    other: Map<String, Value>,
}

/// One day of it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct LocalDay {
    /// Per kind, by its name; a kind this dibs does not know is kept and not sent.
    #[serde(default)]
    counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent: Option<Sent>,
    #[serde(flatten)]
    other: Map<String, Value>,
}

/// What was last sent of a day, and when.
#[derive(Debug, Serialize, Deserialize)]
struct Sent {
    #[serde(default)]
    counts: BTreeMap<String, u64>,
    #[serde(default)]
    durations: Value,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

/// A refusal shown on this machine, by its id; the kind by its name.
#[derive(Debug, Serialize, Deserialize)]
struct RememberedRefusal {
    id: RefusalId,
    tally: String,
}

impl LocalDay {
    /// The counts of the kinds this dibs knows.
    fn known(&self) -> BTreeMap<Tally, u64> {
        self.counts
            .iter()
            .filter(|(_, count)| **count > 0)
            .filter_map(|(name, count)| Some((Tally::parse(name)?, *count)))
            .collect()
    }

    /// Whether the day's counts differ from what was last sent of them.
    fn counts_unsent(&self) -> bool {
        let known = self.known();
        let sent: BTreeMap<Tally, u64> = self.sent.as_ref().map_or_else(BTreeMap::new, |sent| {
            sent.counts
                .iter()
                .filter_map(|(name, count)| Some((Tally::parse(name)?, *count)))
                .collect()
        });
        !known.is_empty() && known != sent
    }
}

impl ReportFile {
    /// The file read from `value`, every part this dibs cannot read kept aside.
    fn of(value: Value) -> Self {
        let Value::Object(mut fields) = value else {
            return Self::default();
        };
        let mut file = Self::default();
        if let Some(Value::Object(days)) = fields.remove("days") {
            for (key, entry) in days {
                let day = ReportDay::parse(&key).ok();
                match (day, serde_json::from_value::<LocalDay>(entry.clone())) {
                    (Some(day), Ok(local)) => {
                        file.days.insert(day, local);
                    }
                    _ => {
                        file.unread_days.insert(key, entry);
                    }
                }
            }
        }
        if let Some(Value::Array(folded)) = fields.remove("folded") {
            file.folded = folded
                .into_iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect();
        }
        if let Some(Value::Array(refusals)) = fields.remove("refusals") {
            file.refusals = refusals
                .into_iter()
                .filter_map(|refusal| serde_json::from_value(refusal).ok())
                .collect();
        }
        file.other = fields;
        file
    }

    /// The file as it is written.
    fn value(&self) -> Value {
        let mut days = self.unread_days.clone();
        for (day, local) in &self.days {
            if let Ok(entry) = serde_json::to_value(local) {
                days.insert(day.to_string(), entry);
            }
        }
        let mut fields = self.other.clone();
        fields.insert("days".to_owned(), Value::Object(days));
        fields.insert(
            "folded".to_owned(),
            Value::Array(self.folded.iter().cloned().map(Value::String).collect()),
        );
        fields.insert(
            "refusals".to_owned(),
            serde_json::to_value(&self.refusals).unwrap_or(Value::Array(Vec::new())),
        );
        Value::Object(fields)
    }

    /// Counts a spooled entry: each kind it names once on its day, and its refusal's id with the
    /// first of its kinds.
    fn count(&mut self, entry: SpoolEntry) {
        let local = self.days.entry(entry.day).or_default();
        for kind in &entry.kinds {
            let count = local.counts.entry(kind.clone()).or_default();
            *count = count.saturating_add(1);
        }
        if let (Some(id), Some(first)) = (entry.refusal, entry.kinds.first()) {
            self.refusals.push(RememberedRefusal {
                id,
                tally: first.clone(),
            });
        }
    }

    /// Forgets what is older than the week before `today`.
    fn prune(&mut self, today: ReportDay) {
        let oldest = today.minus_days(KEPT_DAYS_BACK);
        self.days.retain(|day, _| *day >= oldest);
        self.unread_days
            .retain(|key, _| ReportDay::parse(key).map_or(true, |day| day >= oldest));
        if self.refusals.len() > KEPT_REFUSALS {
            let extra = self.refusals.len() - KEPT_REFUSALS;
            self.refusals.drain(..extra);
        }
    }
}

fn path(state: &StateDir) -> PathBuf {
    state.json_file(NAME)
}

fn open_lock(state: &StateDir) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.lock_file(NAME)?)
}

/// The lock, waited for up to `patience`; `None` when another process keeps it longer.
fn take_lock(state: &StateDir, patience: StdDuration) -> Option<File> {
    let lock = open_lock(state).ok()?;
    let step = StdDuration::from_millis(5);
    let mut waited = StdDuration::ZERO;
    loop {
        match lock.try_lock() {
            Ok(()) => return Some(lock),
            Err(TryLockError::WouldBlock) if waited < patience => {
                thread::sleep(step);
                waited += step;
            }
            Err(_) => return None,
        }
    }
}

/// The file as it is on disk; one that is not JSON is moved aside, so it is not overwritten
/// unseen.
fn load(state: &StateDir) -> ReportFile {
    let path = path(state);
    let Ok(text) = std::fs::read(&path) else {
        return ReportFile::default();
    };
    match serde_json::from_slice::<Value>(&text) {
        Ok(value) => ReportFile::of(value),
        Err(error) => {
            tracing::debug!(error = %error, "the report could not be read; it is moved aside");
            let aside = path.with_extension("json.unread");
            if let Err(error) = std::fs::rename(&path, aside) {
                tracing::debug!(error = %error, "the unreadable report could not be moved aside");
            }
            ReportFile::default()
        }
    }
}

/// Changes the file under its lock, forgets what is more than a week before `today`, and writes
/// it; answers what `change` answered, or `None` when the lock or the write failed.
fn update<T>(
    state: &StateDir,
    today: ReportDay,
    change: impl FnOnce(&mut ReportFile) -> T,
) -> Option<T> {
    let _lock = take_lock(state, LOCK_PATIENCE)?;
    let mut file = load(state);
    let answer = change(&mut file);
    file.prune(today);
    match write_json_durably(&path(state), &file.value()) {
        Ok(()) => Some(answer),
        Err(error) => {
            tracing::debug!(error = %error, "the report could not be written");
            None
        }
    }
}

/// The spool directory of the project.
fn spool_dir(state: &StateDir) -> PathBuf {
    state.spool_dir(NAME)
}

/// One count in the spool: its day, the kinds it counts once each, and for a refusal its id.
#[derive(Debug, Serialize, Deserialize)]
struct SpoolEntry {
    day: ReportDay,
    kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refusal: Option<RefusalId>,
}

/// The names of the counts in the spool, finished or not.
fn spool_names(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Puts one count in the spool without a lock and without syncing it: written under a name of its
/// own, then renamed to `<nanos>-<pid>-<hex>.json`, so a fold reads it whole or not at all.
/// Answers whether it was put there; a spool that holds [`SPOOL_CAP`] counts takes no more.
fn spool(state: &StateDir, hex: &str, entry: &SpoolEntry) -> bool {
    let dir = spool_dir(state);
    if std::fs::create_dir_all(&dir).is_err() || spool_names(&dir).len() >= SPOOL_CAP {
        return false;
    }
    let name = format!(
        "{}-{}-{hex}",
        OffsetDateTime::now_utc().unix_timestamp_nanos(),
        std::process::id()
    );
    let Ok(line) = serde_json::to_vec(entry) else {
        return false;
    };
    let unfinished = dir.join(format!("{name}.part"));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&unfinished)
        .and_then(|mut file| file.write_all(&line))
        .and_then(|()| std::fs::rename(&unfinished, dir.join(format!("{name}.json"))));
    match written {
        Ok(()) => true,
        Err(error) => {
            tracing::debug!(error = %error, "a count could not be put in the spool");
            let _ = std::fs::remove_file(&unfinished);
            false
        }
    }
}

/// Six random hexadecimal digits.
fn random_hex() -> String {
    format!("{:06x}", rand::rng().random::<u32>() & 0x00ff_ffff)
}

/// A refusal id no refusal of this machine has: not one `report.json` remembers, nor one in the
/// spool, whose files end with the id they count.
fn free_refusal_id(state: &StateDir) -> Option<RefusalId> {
    let known = load(state).refusals;
    let spooled = spool_names(&spool_dir(state));
    (0..ID_CANDIDATES)
        .map(|_| random_hex())
        .find(|hex| {
            !spooled.iter().any(|name| {
                name.strip_suffix(".json")
                    .is_some_and(|stem| stem.ends_with(hex.as_str()))
            }) && !known
                .iter()
                .any(|refusal| refusal.id.as_str() == format!("r-{hex}"))
        })
        .and_then(|hex| RefusalId::parse(&format!("r-{hex}")).ok())
}

/// The marker of a session counted as having run switched off: a hash of its host and key, so
/// any session id makes one file name.
fn session_marker(state: &StateDir, session: &SessionRef) -> PathBuf {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in session
        .host
        .as_str()
        .bytes()
        .chain(*b" ")
        .chain(session.key.as_str().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    spool_dir(state)
        .join("sessions")
        .join(format!("{hash:016x}"))
}

/// The project's counts on this machine, as the guard's port: every count goes to the spool, and
/// none waits.
pub struct StateTallies {
    state: StateDir,
}

impl StateTallies {
    /// The counts kept in `state`.
    pub fn new(state: StateDir) -> Self {
        Self { state }
    }
}

impl Tallies for StateTallies {
    fn refused(&self, day: ReportDay, kinds: &[Tally]) -> Option<RefusalId> {
        let mut unique = kinds.to_vec();
        unique.sort_unstable();
        unique.dedup();
        if unique.is_empty() {
            return None;
        }
        let id = free_refusal_id(&self.state);
        let entry = SpoolEntry {
            day,
            kinds: unique.iter().map(|tally| tally.name().to_owned()).collect(),
            refusal: id.clone(),
        };
        let hex = id
            .as_ref()
            .and_then(|id| id.as_str().strip_prefix("r-"))
            .map_or_else(random_hex, str::to_owned);
        (spool(&self.state, &hex, &entry)).then_some(id).flatten()
    }

    fn unreachable(&self, day: ReportDay) {
        count(&self.state, day, Tally::Unreachable);
    }

    fn switched_off(&self, day: ReportDay, session: &SessionRef) {
        let marker = session_marker(&self.state, session);
        let Some(dir) = marker.parent() else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        let first = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .is_ok();
        if first {
            count(&self.state, day, Tally::SwitchedOff);
        }
    }
}

/// Counts one of `tally` on `day`, through the spool.
pub fn count(state: &StateDir, day: ReportDay, tally: Tally) {
    spool(
        state,
        &random_hex(),
        &SpoolEntry {
            day,
            kinds: vec![tally.name().to_owned()],
            refusal: None,
        },
    );
}

/// Folds every count of the spool into `report.json`, under its lock: what the spool holds is
/// counted, `report.json` written durably with the names of the files counted, and those files
/// deleted. A file whose name the last fold recorded was counted by it and is only deleted, so a
/// fold that stopped between its write and its deletions never counts twice. A file that cannot be
/// read is deleted uncounted; one a hook is still writing is left for the next fold. Answers
/// whether the fold ran. Days more than a week before `now` are forgotten.
pub fn fold(state: &StateDir, now: OffsetDateTime) -> bool {
    let dir = spool_dir(state);
    let counted = update(state, ReportDay::of(now), |file| {
        let names = spool_names(&dir);
        let mut counted = BTreeSet::new();
        for name in names.iter().filter(|name| name.ends_with(".json")) {
            if !file.folded.contains(name) {
                let entry = std::fs::read(dir.join(name))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<SpoolEntry>(&bytes).ok());
                match entry {
                    Some(entry) => file.count(entry),
                    None => tracing::debug!("a spooled count could not be read; it is dropped"),
                }
            }
            counted.insert(name.clone());
        }
        file.folded = counted.clone();
        counted
    });
    let Some(counted) = counted else {
        return false;
    };
    for name in &counted {
        if let Err(error) = std::fs::remove_file(dir.join(name)) {
            tracing::debug!(error = %error, "a folded count could not be deleted");
        }
    }
    clear_stale(&dir, ".part", UNFINISHED_KEPT);
    clear_stale(&dir.join("sessions"), "", SESSION_MARKER_KEPT);
    true
}

/// Deletes the files of `dir` ending with `suffix` that were last changed `kept` ago or longer.
fn clear_stale(dir: &Path, suffix: &str, kept: StdDuration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age >= kept);
        let named = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(suffix));
        if stale && named {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The hook durations of `day` from the machine's timings, per agent (the hook adapter that ran)
/// and event, in a report's form.
fn durations_of(timings: &HookTimings, day: ReportDay) -> Vec<HookDurations> {
    timings
        .day(day.date())
        .map(|histograms| {
            histograms
                .iter()
                .filter_map(|(key, buckets)| {
                    let (agent, event) = key.split_once(' ')?;
                    Some(HookDurations {
                        agent: AgentLabel::parse(agent).ok()?,
                        event: HookEventName::parse(event).ok()?,
                        buckets: *buckets,
                    })
                })
                .take(DayCounts::MAX_DURATIONS)
                .collect()
        })
        .unwrap_or_default()
}

/// The days a heartbeat sent at `now` carries, the machine's hook durations from `timings` with
/// them: those whose counts differ from what was last sent of them, and those whose durations
/// alone differ when the day is over or was last sent [`DURATIONS_EVERY`] ago or longer. Oldest
/// first, at most a week of them.
pub fn pending(state: &StateDir, timings: &HookTimings, now: OffsetDateTime) -> Vec<DayReport> {
    let today = ReportDay::of(now);
    let oldest = today.minus_days(KEPT_DAYS_BACK);
    let file = load(state);
    let mut days: Vec<ReportDay> = file.days.keys().copied().collect();
    for key in timings.days.keys() {
        if let Ok(day) = ReportDay::parse(key)
            && !days.contains(&day)
        {
            days.push(day);
        }
    }
    days.sort_unstable();
    days.into_iter()
        .filter(|day| *day >= oldest)
        .filter_map(|day| {
            let local = file.days.get(&day);
            let counts = DayCounts::new(
                local.map(LocalDay::known).unwrap_or_default(),
                durations_of(timings, day),
            )
            .unwrap_or_default();
            if counts.is_empty() {
                return None;
            }
            let sent = local.and_then(|local| local.sent.as_ref());
            let durations = serde_json::to_value(&counts.durations).unwrap_or(Value::Null);
            let due = match sent {
                None => true,
                Some(_) if local.is_some_and(LocalDay::counts_unsent) => true,
                Some(sent) => {
                    sent.durations != durations && (day < today || now - sent.at >= DURATIONS_EVERY)
                }
            };
            due.then_some(DayReport { day, counts })
        })
        .take(HeartbeatReports::MAX)
        .collect()
}

/// Whether a day's counts differ from what was last sent of it, so a command of the command line
/// starts a sync to send them.
pub fn counts_unsent(state: &StateDir) -> bool {
    spool_names(&spool_dir(state))
        .iter()
        .any(|name| name.ends_with(".json"))
        || load(state).days.values().any(LocalDay::counts_unsent)
}

/// Remembers that `reports` were sent at `now` and the server took them.
pub fn mark_sent(state: &StateDir, reports: &[DayReport], now: OffsetDateTime) {
    if reports.is_empty() {
        return;
    }
    update(state, ReportDay::of(now), |file| {
        for report in reports {
            let local = file.days.entry(report.day).or_default();
            local.sent = Some(Sent {
                counts: report
                    .counts
                    .counts
                    .iter()
                    .map(|(tally, count)| (tally.name().to_owned(), *count))
                    .collect(),
                durations: serde_json::to_value(&report.counts.durations).unwrap_or(Value::Null),
                at: now,
            });
        }
    });
}

/// The kind of the refusal shown with `id` on this machine, while it is remembered: the spool is
/// folded first, so a refusal counted a moment ago is known.
pub fn refusal_tally(state: &StateDir, id: &RefusalId, now: OffsetDateTime) -> Option<Tally> {
    fold(state, now);
    load(state)
        .refusals
        .iter()
        .rev()
        .find(|known| known.id == *id)
        .and_then(|known| Tally::parse(&known.tally))
}

/// When a sync that sends counts was last started from the command line, and when one last failed
/// for another reason than an unreachable server.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ReportSyncs {
    #[serde(default, with = "time::serde::rfc3339::option")]
    started_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    failed_at: Option<OffsetDateTime>,
}

fn syncs_path(state: &StateDir) -> PathBuf {
    state.json_file("report-sync")
}

/// Whether a command of the command line may start a sync that sends counts at `now`: always,
/// unless the last such sync failed and one was started within [`REPORT_SYNC_BACKOFF`].
pub fn report_sync_may_start(state: &StateDir, now: OffsetDateTime) -> bool {
    let syncs: ReportSyncs = read_json(&syncs_path(state)).unwrap_or_default();
    let recent = syncs
        .started_at
        .is_some_and(|at| at <= now && now - at < REPORT_SYNC_BACKOFF);
    syncs.failed_at.is_none() || !recent
}

fn change_syncs(state: &StateDir, change: impl FnOnce(&mut ReportSyncs)) {
    let path = syncs_path(state);
    let mut syncs: ReportSyncs = read_json(&path).unwrap_or_default();
    change(&mut syncs);
    if let Err(error) = write_json(&path, &syncs) {
        tracing::debug!(error = %error, "the report syncs could not be remembered");
    }
}

/// Remembers that a sync that sends counts was started at `at`.
pub fn note_report_sync_started(state: &StateDir, at: OffsetDateTime) {
    change_syncs(state, |syncs| syncs.started_at = Some(at));
}

/// Remembers how a sync that sends counts ended: `Some(at)` when it failed for another reason
/// than an unreachable server, `None` when it succeeded.
pub fn note_report_sync_ended(state: &StateDir, failed_at: Option<OffsetDateTime>) {
    change_syncs(state, |syncs| syncs.failed_at = failed_at);
}

#[cfg(test)]
mod tests {
    use dibs_agent_host::{EventKind, HostId, SessionKey};
    use dibs_client::credentials::CredentialKey;

    use super::*;

    /// Noon of a day in March 2024.
    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH
        .saturating_add(Duration::days(19_791))
        .saturating_add(Duration::hours(12));

    fn state(dir: &tempfile::TempDir) -> StateDir {
        let key = CredentialKey {
            server: dibs_client::server_url::ServerUrl::parse("https://dibs.example.com").unwrap(),
            project: dibs_domain::project::ProjectSlug::parse("widgets").unwrap(),
        };
        StateDir::new(dir.path(), &key)
    }

    fn session(key: &str) -> SessionRef {
        SessionRef {
            host: HostId::new("neutral"),
            key: SessionKey::parse(key).unwrap(),
        }
    }

    /// What a sync would send now: the spool folded first.
    fn folded(state: &StateDir, timings: &HookTimings, now: OffsetDateTime) -> Vec<DayReport> {
        assert!(fold(state, NOW));
        pending(state, timings, now)
    }

    fn spooled(state: &StateDir) -> usize {
        spool_names(&spool_dir(state))
            .iter()
            .filter(|name| name.ends_with(".json"))
            .count()
    }

    fn timings(days: &[(ReportDay, u32)]) -> HookTimings {
        let mut timings = HookTimings::default();
        for (day, runs) in days {
            for _ in 0..*runs {
                timings.record(
                    day.date(),
                    "neutral",
                    EventKind::BeforeTool,
                    Duration::milliseconds(4),
                );
            }
        }
        timings
    }

    #[test]
    fn the_file_is_report_json_in_the_projects_state() {
        let dir = tempfile::tempdir().unwrap();
        count(&state(&dir), ReportDay::of(NOW), Tally::Frozen);
        assert!(fold(&state(&dir), NOW));
        assert_eq!(path(&state(&dir)).file_name().unwrap(), "report.json");
        assert_eq!(spool_dir(&state(&dir)).file_name().unwrap(), "report.spool");
        assert!(path(&state(&dir)).exists());
    }

    #[test]
    fn a_refusal_is_counted_once_per_kind_and_remembered_by_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let tallies = StateTallies::new(state(&dir));
        let today = ReportDay::of(NOW);
        let id = tallies
            .refused(today, &[Tally::HeldBy, Tally::HeldBy, Tally::NeedsClaim])
            .unwrap();
        assert_eq!(id.as_str().len(), "r-".len() + 6);
        let other = tallies.refused(today, &[Tally::HeldBy]).unwrap();
        assert_ne!(id, other);
        assert_eq!(
            spooled(&state(&dir)),
            2,
            "counted in the spool, nothing folded yet"
        );
        assert_eq!(
            refusal_tally(&state(&dir), &id, NOW),
            Some(Tally::HeldBy),
            "a refusal still in the spool is found: the lookup folds first"
        );
        assert_eq!(tallies.refused(today, &[]), None, "nothing refused");
        tallies.unreachable(today);
        let reports = folded(&state(&dir), &HookTimings::default(), NOW);
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].counts.counts,
            BTreeMap::from([
                (Tally::HeldBy, 2),
                (Tally::NeedsClaim, 1),
                (Tally::Unreachable, 1)
            ])
        );
    }

    #[test]
    fn a_session_switched_off_is_counted_once_on_the_day_it_was_first_seen() {
        let dir = tempfile::tempdir().unwrap();
        let tallies = StateTallies::new(state(&dir));
        let today = ReportDay::of(NOW);
        for key in ["s-1", "s-1", "s-2", "s-1"] {
            tallies.switched_off(today, &session(key));
        }
        tallies.switched_off(today.next(), &session("s-1"));
        tallies.switched_off(today.next(), &session("s-3"));
        let reports = folded(&state(&dir), &HookTimings::default(), NOW);
        let counts: Vec<u64> = reports
            .iter()
            .map(|report| report.counts.counts[&Tally::SwitchedOff])
            .collect();
        assert_eq!(counts, [2, 1], "s-1 on the next day counts nothing again");
        let markers = std::fs::read_dir(spool_dir(&state(&dir)).join("sessions"))
            .unwrap()
            .count();
        assert_eq!(markers, 3, "one marker per session, on this machine");
        let text = std::fs::read_to_string(path(&state(&dir))).unwrap();
        assert!(
            !text.contains("s-1"),
            "no session's id is kept in the report"
        );
        let sent = serde_json::to_string(&reports).unwrap();
        assert!(!sent.contains("s-1"), "and are never sent: {sent}");
    }

    #[test]
    fn a_sent_day_is_due_again_when_its_counts_change_or_its_durations_are_an_hour_old() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let today = ReportDay::of(NOW);
        let yesterday = today.minus_days(1);
        count(&state, today, Tally::NeedsSlot);
        let first_timings = timings(&[(today, 1), (yesterday, 1)]);
        assert!(counts_unsent(&state));
        let first = folded(&state, &first_timings, NOW);
        assert_eq!(first.len(), 2, "never sent, both go");
        assert_eq!(
            first[1].counts.durations.len(),
            1,
            "with the machine's timings"
        );
        mark_sent(&state, &first, NOW);
        assert!(pending(&state, &first_timings, NOW).is_empty());
        assert!(!counts_unsent(&state));

        let more = timings(&[(today, 2), (yesterday, 2)]);
        let days: Vec<ReportDay> = pending(&state, &more, NOW + Duration::minutes(5))
            .iter()
            .map(|report| report.day)
            .collect();
        assert_eq!(
            days,
            [yesterday],
            "a day that is over goes for its durations"
        );
        assert!(!counts_unsent(&state), "durations alone start no sync");
        let later: Vec<ReportDay> = pending(&state, &more, NOW + Duration::hours(1))
            .iter()
            .map(|report| report.day)
            .collect();
        assert_eq!(later, [yesterday, today]);

        count(&state, today, Tally::NeedsSlot);
        assert!(counts_unsent(&state), "a count in the spool is unsent");
        assert!(fold(&state, NOW));
        assert!(counts_unsent(&state));
        assert!(
            pending(&state, &more, NOW + Duration::minutes(5))
                .iter()
                .any(|report| report.day == today),
            "a changed count goes at once"
        );
    }

    #[test]
    fn days_past_the_week_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let today = ReportDay::of(NOW);
        count(&state, today.minus_days(7), Tally::Frozen);
        count(&state, today.minus_days(6), Tally::Frozen);
        count(&state, today, Tally::Frozen);
        let days: Vec<ReportDay> = folded(&state, &HookTimings::default(), NOW)
            .iter()
            .map(|r| r.day)
            .collect();
        assert_eq!(days, [today.minus_days(6), today]);
    }

    #[test]
    fn counts_of_many_writers_at_once_are_all_kept_and_the_file_stays_whole() {
        let dir = tempfile::tempdir().unwrap();
        let today = ReportDay::of(NOW);
        let writers: Vec<_> = (0..8)
            .map(|_| {
                let state = state(&dir);
                thread::spawn(move || {
                    for _ in 0..25 {
                        count(&state, today, Tally::HeldBy);
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(spooled(&state(&dir)), 200);
        let reports = folded(&state(&dir), &HookTimings::default(), NOW);
        assert_eq!(reports[0].counts.counts[&Tally::HeldBy], 200);
        assert_eq!(spooled(&state(&dir)), 0, "the fold deletes what it counted");
        let text = std::fs::read(path(&state(&dir))).unwrap();
        assert!(serde_json::from_slice::<Value>(&text).is_ok());
    }

    #[test]
    fn what_another_version_wrote_is_kept_and_a_file_that_is_no_json_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let today = ReportDay::of(NOW);
        let written = serde_json::json!({
            "days": {
                today.to_string(): {"counts": {"held_by": 1, "kind_of_tomorrow": 4}, "tomorrow": true},
                today.minus_days(1).to_string(): {"counts": "a shape of tomorrow"}
            },
            "machine_notes": {"kept": 1},
            "refusals": [{"id": "r-0a0b0c", "tally": "kind_of_tomorrow"}]
        });
        std::fs::create_dir_all(path(&state).parent().unwrap()).unwrap();
        std::fs::write(path(&state), written.to_string()).unwrap();
        count(&state, today, Tally::HeldBy);
        assert!(fold(&state, NOW));
        let file: Value = serde_json::from_slice(&std::fs::read(path(&state)).unwrap()).unwrap();
        let day = &file["days"][today.to_string()];
        assert_eq!(day["counts"]["held_by"], 2);
        assert_eq!(
            day["counts"]["kind_of_tomorrow"], 4,
            "a kind this dibs does not know stays"
        );
        assert_eq!(day["tomorrow"], true);
        assert_eq!(
            file["days"][today.minus_days(1).to_string()]["counts"],
            "a shape of tomorrow",
            "a day it cannot read stays as it was"
        );
        assert_eq!(file["machine_notes"]["kept"], 1);
        let reports = folded(&state, &HookTimings::default(), NOW);
        assert_eq!(
            reports[0].counts.counts,
            BTreeMap::from([(Tally::HeldBy, 2)]),
            "only what this dibs knows is sent"
        );
        assert_eq!(
            refusal_tally(&state, &RefusalId::parse("r-0a0b0c").unwrap(), NOW),
            None
        );

        std::fs::write(path(&state), "{ half a file").unwrap();
        count(&state, today, Tally::Frozen);
        assert!(fold(&state, NOW));
        assert!(path(&state).with_extension("json.unread").exists());
        let reports = folded(&state, &HookTimings::default(), NOW);
        assert_eq!(
            reports[0].counts.counts,
            BTreeMap::from([(Tally::Frozen, 1)])
        );
    }

    #[test]
    fn after_a_failed_report_sync_another_waits_ten_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        assert!(report_sync_may_start(&state, NOW));
        note_report_sync_started(&state, NOW);
        assert!(report_sync_may_start(&state, NOW), "nothing failed");
        note_report_sync_ended(&state, Some(NOW));
        assert!(!report_sync_may_start(&state, NOW + Duration::minutes(9)));
        assert!(report_sync_may_start(&state, NOW + Duration::minutes(10)));
        note_report_sync_started(&state, NOW + Duration::minutes(10));
        assert!(!report_sync_may_start(&state, NOW + Duration::minutes(11)));
        note_report_sync_ended(&state, None);
        assert!(report_sync_may_start(&state, NOW + Duration::minutes(11)));
    }

    #[test]
    fn a_fold_that_stopped_before_deleting_counts_nothing_twice() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let today = ReportDay::of(NOW);
        for _ in 0..3 {
            count(&state, today, Tally::NeedsClaim);
        }
        let names: Vec<String> = spool_names(&spool_dir(&state));
        let copies: Vec<(String, Vec<u8>)> = names
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    std::fs::read(spool_dir(&state).join(name)).unwrap(),
                )
            })
            .collect();
        assert!(fold(&state, NOW));
        for (name, bytes) in &copies {
            std::fs::write(spool_dir(&state).join(name), bytes).unwrap();
        }
        count(&state, today, Tally::NeedsClaim);
        let reports = folded(&state, &HookTimings::default(), NOW);
        assert_eq!(
            reports[0].counts.counts[&Tally::NeedsClaim],
            4,
            "the files the first fold recorded were left behind and are not counted again"
        );
        assert_eq!(spooled(&state), 0);
        let reports = folded(&state, &HookTimings::default(), NOW);
        assert_eq!(reports[0].counts.counts[&Tally::NeedsClaim], 4);
    }

    #[test]
    fn a_full_spool_drops_the_next_count() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let today = ReportDay::of(NOW);
        let spool = spool_dir(&state);
        std::fs::create_dir_all(&spool).unwrap();
        for n in 0..SPOOL_CAP {
            std::fs::write(
                spool.join(format!("{n}-0-000000.json")),
                r#"{"day":"2024-03-09","kinds":["frozen"]}"#,
            )
            .unwrap();
        }
        count(&state, today, Tally::HeldBy);
        assert_eq!(
            spooled(&state),
            SPOOL_CAP,
            "the count past the cap is dropped"
        );
        let tallies = StateTallies::new(state.clone());
        assert_eq!(tallies.refused(today, &[Tally::HeldBy]), None);
        assert!(fold(&state, NOW));
        count(&state, today, Tally::HeldBy);
        assert_eq!(
            spooled(&state),
            1,
            "once folded, the spool takes counts again"
        );
    }

    #[test]
    fn a_hook_counts_while_another_process_holds_the_reports_lock() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(&dir);
        let held = take_lock(&state, StdDuration::ZERO).unwrap();
        let started = std::time::Instant::now();
        let tallies = StateTallies::new(state.clone());
        assert!(
            tallies
                .refused(ReportDay::of(NOW), &[Tally::Frozen])
                .is_some()
        );
        tallies.switched_off(ReportDay::of(NOW), &session("s-9"));
        assert!(
            started.elapsed() < StdDuration::from_millis(500),
            "no count waits for the lock"
        );
        assert_eq!(spooled(&state), 2);
        drop(held);
        let reports = folded(&state, &HookTimings::default(), NOW);
        assert_eq!(
            reports[0].counts.counts,
            BTreeMap::from([(Tally::Frozen, 1), (Tally::SwitchedOff, 1)])
        );
    }
}
