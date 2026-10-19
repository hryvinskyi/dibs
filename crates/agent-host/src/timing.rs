//! How long hook runs take: a per-day histogram per host and event kind, so the speed budget of a
//! hook can be read from a person's own machine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::{Date, Duration};

use crate::event::EventKind;

/// The upper bounds of the buckets, in milliseconds; a run longer than the last falls in one more
/// bucket after them.
pub const BUCKET_BOUNDS_MS: [u64; 6] = [10, 30, 100, 300, 1_000, 2_000];

/// How many days a histogram keeps.
pub const KEEP_DAYS: usize = 7;

/// The number of buckets: one per bound and one for longer runs.
const BUCKETS: usize = BUCKET_BOUNDS_MS.len() + 1;

/// Run counts per bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Buckets([u64; BUCKETS]);

impl Buckets {
    /// Counts one run of this length.
    pub fn record(&mut self, took: Duration) {
        let millis = u64::try_from(took.whole_milliseconds()).unwrap_or(0);
        let index = BUCKET_BOUNDS_MS
            .iter()
            .position(|bound| millis <= *bound)
            .unwrap_or(BUCKET_BOUNDS_MS.len());
        if let Some(count) = self.0.get_mut(index) {
            *count = count.saturating_add(1);
        }
    }

    /// The counts, one per bucket.
    pub fn counts(&self) -> [u64; BUCKETS] {
        self.0
    }

    /// How many runs were counted.
    pub fn total(&self) -> u64 {
        self.0.iter().sum()
    }

    /// The upper bound, in milliseconds, of the bucket the 95th percentile falls in; `None` for
    /// no runs, or when it falls past the last bound.
    pub fn p95_bound_ms(&self) -> Option<u64> {
        let total = self.total();
        if total == 0 {
            return None;
        }
        let needed = total.saturating_mul(95).div_ceil(100);
        let mut seen = 0_u64;
        for (index, count) in self.0.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= needed {
                return BUCKET_BOUNDS_MS.get(index).copied();
            }
        }
        None
    }
}

/// The histograms of the last days, per host and event kind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookTimings {
    /// Per day (`2026-03-09`), per `<host> <event kind>`.
    pub days: BTreeMap<String, BTreeMap<String, Buckets>>,
}

impl HookTimings {
    /// Counts one run of `host`'s `kind` on `day` that took `took`, and forgets days past the
    /// [`KEEP_DAYS`] newest.
    pub fn record(&mut self, day: Date, host: &str, kind: EventKind, took: Duration) {
        self.days
            .entry(day_key(day))
            .or_default()
            .entry(key(host, kind))
            .or_default()
            .record(took);
        while self.days.len() > KEEP_DAYS {
            if self.days.pop_first().is_none() {
                break;
            }
        }
    }

    /// The histograms of one day.
    pub fn day(&self, day: Date) -> Option<&BTreeMap<String, Buckets>> {
        self.days.get(&day_key(day))
    }
}

/// A day as its key: `2026-03-09`, which sorts as the days do.
pub fn day_key(day: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        day.year(),
        u8::from(day.month()),
        day.day()
    )
}

/// The key of a host's event kind in a day's histograms.
pub fn key(host: &str, kind: EventKind) -> String {
    format!("{host} {kind}")
}

#[cfg(test)]
mod tests {
    use time::Month;

    use super::*;

    fn day(n: u8) -> Date {
        Date::from_calendar_date(2026, Month::March, n).unwrap()
    }

    #[test]
    fn a_run_falls_in_the_first_bucket_that_holds_it() {
        let mut buckets = Buckets::default();
        for millis in [0, 10, 11, 30, 31, 300, 1_000, 2_000, 2_001, 60_000] {
            buckets.record(Duration::milliseconds(millis));
        }
        assert_eq!(buckets.counts(), [2, 2, 1, 1, 1, 1, 2]);
        assert_eq!(buckets.total(), 10);
    }

    #[test]
    fn the_95th_percentile_names_its_bucket() {
        let mut buckets = Buckets::default();
        assert_eq!(buckets.p95_bound_ms(), None);
        for _ in 0..95 {
            buckets.record(Duration::milliseconds(5));
        }
        for _ in 0..5 {
            buckets.record(Duration::milliseconds(250));
        }
        assert_eq!(buckets.p95_bound_ms(), Some(10));
        buckets.record(Duration::milliseconds(250));
        assert_eq!(buckets.p95_bound_ms(), Some(300));
        let mut slow = Buckets::default();
        slow.record(Duration::seconds(5));
        assert_eq!(slow.p95_bound_ms(), None);
    }

    #[test]
    fn a_week_of_days_is_kept_per_host_and_event() {
        let mut timings = HookTimings::default();
        for n in 1..=9 {
            timings.record(
                day(n),
                "neutral",
                EventKind::BeforeTool,
                Duration::milliseconds(3),
            );
        }
        timings.record(
            day(9),
            "neutral",
            EventKind::AfterTool,
            Duration::milliseconds(3),
        );
        assert_eq!(timings.days.len(), KEEP_DAYS);
        assert!(timings.day(day(2)).is_none());
        let last = timings.day(day(9)).unwrap();
        assert_eq!(last["neutral before_tool"].total(), 1);
        assert_eq!(last["neutral after_tool"].total(), 1);
        let text = serde_json::to_string(&timings).unwrap();
        assert_eq!(serde_json::from_str::<HookTimings>(&text).unwrap(), timings);
    }
}
