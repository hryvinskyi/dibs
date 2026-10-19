//! Moments and spans of time as the pages show them: a wall-clock text in UTC that the page's
//! script rewrites into the viewer's zone, the machine-readable time beside it, and how long ago it
//! was, counted on the server's clock. The words around the numbers are the templates'.

use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime, UtcOffset};

/// A moment as a page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// The time in RFC 3339, UTC, for `<time datetime>`.
    pub iso: String,
    /// `HH:MM` when it falls on the same UTC day as now, else `YYYY-MM-DD HH:MM`.
    pub clock: String,
    /// How long before now it was.
    pub ago: Ago,
}

/// How long ago a moment was, in the units a person reads at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ago {
    /// Less than a minute ago, or a moment the clock places after now.
    JustNow,
    /// Whole minutes, under an hour.
    Minutes(i64),
    /// Hours and minutes, under three hours, with at least one minute.
    HoursMinutes(i64, i64),
    /// Whole hours, under a day.
    Hours(i64),
    /// Whole days.
    Days(i64),
}

/// A length of time in days, hours and minutes, the seconds dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Whole days.
    pub days: i64,
    /// Hours past the days.
    pub hours: i64,
    /// Minutes past the hours.
    pub minutes: i64,
}

/// `at` as a page shows it when the time is `now`.
pub fn moment(at: OffsetDateTime, now: OffsetDateTime) -> Moment {
    let at = at.to_offset(UtcOffset::UTC);
    let now = now.to_offset(UtcOffset::UTC);
    let time = format!("{:02}:{:02}", at.hour(), at.minute());
    let clock = if at.date() == now.date() {
        time
    } else {
        format!(
            "{:04}-{:02}-{:02} {time}",
            at.year(),
            u8::from(at.month()),
            at.day()
        )
    };
    Moment {
        iso: at.format(&Rfc3339).unwrap_or_default(),
        clock,
        ago: ago(now - at),
    }
}

/// How long ago something was that happened `elapsed` before now.
pub fn ago(elapsed: Duration) -> Ago {
    let minutes = elapsed.whole_minutes();
    let hours = elapsed.whole_hours();
    if minutes < 1 {
        Ago::JustNow
    } else if minutes < 60 {
        Ago::Minutes(minutes)
    } else if hours < 3 && minutes % 60 != 0 {
        Ago::HoursMinutes(hours, minutes % 60)
    } else if hours < 24 {
        Ago::Hours(hours)
    } else {
        Ago::Days(elapsed.whole_days())
    }
}

/// A duration as days, hours and minutes; a negative one is nothing.
pub fn span(duration: Duration) -> Span {
    let minutes = duration.whole_minutes().max(0);
    Span {
        days: minutes / (24 * 60),
        hours: minutes / 60 % 24,
        minutes: minutes % 60,
    }
}

/// The start of the day `now` falls on at this offset from UTC.
pub fn start_of_day(now: OffsetDateTime, offset: UtcOffset) -> OffsetDateTime {
    now.to_offset(offset).replace_time(time::Time::MIDNIGHT)
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn a_moment_of_today_shows_its_time_and_another_day_its_date() {
        let now = datetime!(2026-10-19 14:32:10 UTC);
        let today = moment(datetime!(2026-10-19 09:05:59 UTC), now);
        assert_eq!(today.clock, "09:05");
        assert_eq!(today.iso, "2026-10-19T09:05:59Z");
        assert_eq!(today.ago, Ago::Hours(5));
        let earlier = moment(datetime!(2026-10-18 23:59:00 +00:00), now);
        assert_eq!(earlier.clock, "2026-10-18 23:59");
        let zoned = moment(datetime!(2026-10-19 16:00:00 +02:00), now);
        assert_eq!(zoned.clock, "14:00");
        assert_eq!(zoned.iso, "2026-10-19T14:00:00Z");
    }

    #[test]
    fn ago_reads_in_the_largest_sensible_unit() {
        let cases = [
            (Duration::seconds(-30), Ago::JustNow),
            (Duration::seconds(59), Ago::JustNow),
            (Duration::minutes(1), Ago::Minutes(1)),
            (Duration::minutes(59), Ago::Minutes(59)),
            (Duration::minutes(60), Ago::Hours(1)),
            (Duration::minutes(87), Ago::HoursMinutes(1, 27)),
            (Duration::minutes(179), Ago::HoursMinutes(2, 59)),
            (Duration::minutes(181), Ago::Hours(3)),
            (Duration::hours(23), Ago::Hours(23)),
            (Duration::hours(24), Ago::Days(1)),
            (Duration::hours(80), Ago::Days(3)),
        ];
        for (elapsed, expected) in cases {
            assert_eq!(ago(elapsed), expected, "{elapsed}");
        }
    }

    #[test]
    fn a_span_splits_into_days_hours_and_minutes() {
        assert_eq!(
            span(Duration::hours(24)),
            Span {
                days: 1,
                hours: 0,
                minutes: 0
            }
        );
        assert_eq!(
            span(Duration::minutes(90) + Duration::seconds(30)),
            Span {
                days: 0,
                hours: 1,
                minutes: 30
            }
        );
        assert_eq!(
            span(Duration::seconds(-5)),
            Span {
                days: 0,
                hours: 0,
                minutes: 0
            }
        );
    }

    #[test]
    fn the_day_starts_at_midnight_of_the_offset() {
        let now = datetime!(2026-10-18 22:30:00 UTC);
        assert_eq!(
            start_of_day(now, UtcOffset::UTC),
            datetime!(2026-10-18 00:00:00 UTC)
        );
        assert_eq!(
            start_of_day(now, time::macros::offset!(+3)),
            datetime!(2026-10-19 00:00:00 +03:00)
        );
        assert_eq!(
            start_of_day(now, time::macros::offset!(-5)),
            datetime!(2026-10-18 00:00:00 -05:00)
        );
    }
}
