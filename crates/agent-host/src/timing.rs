//! How long hook runs take: a per-day histogram per host and event kind, so the speed budget of a
//! hook can be read from a person's own machine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::{Date, Duration};

pub use dibs_domain::report::BUCKET_BOUNDS_MS;
use dibs_domain::report::DurationBuckets;

use crate::event::EventKind;

/// How many days a histogram keeps.
pub const KEEP_DAYS: usize = 7;

/// Run counts per bucket of [`BUCKET_BOUNDS_MS`].
pub type Buckets = DurationBuckets;

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
