//! How long a project's records are kept: conversations a while after their last message, the
//! event log for less, and what one pass of the daily job removed.

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

/// How long a conversation is kept after its last message (or its start, when it has none).
pub const THREADS_KEPT: Duration = Duration::days(180);

/// How long an event of the log is kept.
pub const EVENTS_KEPT: Duration = Duration::days(90);

/// The moments before which records are past their date, for a pass at one time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cutoffs {
    /// A conversation quiet since before this is past its date.
    pub threads: OffsetDateTime,
    /// An event appended before this is past its date.
    pub events: OffsetDateTime,
}

impl Cutoffs {
    /// The cutoffs of a pass at `now`.
    pub fn at(now: OffsetDateTime) -> Self {
        Self {
            threads: now - THREADS_KEPT,
            events: now - EVENTS_KEPT,
        }
    }
}

/// What keeps a conversation past its date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keeps {
    /// It is about a work item that is still open: kept as long as the item is.
    pub open_item: bool,
    /// It carries an escalation no arbiter has settled: kept until one does.
    pub open_escalation: bool,
}

/// Whether a conversation is past its date: quiet since before the cutoff, and nothing keeps it.
pub fn thread_expired(last_activity: OffsetDateTime, keeps: Keeps, cutoffs: &Cutoffs) -> bool {
    !(keeps.open_item || keeps.open_escalation) && last_activity < cutoffs.threads
}

/// What one pass removed from a project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneCounts {
    /// Conversations.
    pub threads: u64,
    /// Messages, those of the conversations removed.
    pub messages: u64,
    /// Events of the log.
    pub events: u64,
}

impl PruneCounts {
    /// Whether the pass removed nothing.
    pub fn is_empty(&self) -> bool {
        self.threads == 0 && self.messages == 0 && self.events == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: i64) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(n)
    }

    #[test]
    fn a_quiet_thread_goes_after_180_days_unless_its_item_or_escalation_is_open() {
        let cutoffs = Cutoffs::at(day(400));
        assert_eq!(cutoffs.threads, day(220));
        assert_eq!(cutoffs.events, day(310));
        let nothing = Keeps::default();
        assert!(thread_expired(day(219), nothing, &cutoffs));
        assert!(
            !thread_expired(day(220), nothing, &cutoffs),
            "180 days to the moment is kept"
        );
        let item = Keeps {
            open_item: true,
            ..Keeps::default()
        };
        assert!(
            !thread_expired(day(10), item, &cutoffs),
            "an open item's thread is kept"
        );
        let escalation = Keeps {
            open_escalation: true,
            ..Keeps::default()
        };
        assert!(
            !thread_expired(day(10), escalation, &cutoffs),
            "an escalation waiting for an arbiter is kept"
        );
    }

    #[test]
    fn counts_say_when_nothing_went() {
        assert!(PruneCounts::default().is_empty());
        let counts = PruneCounts {
            threads: 1,
            messages: 3,
            events: 0,
        };
        assert!(!counts.is_empty());
        assert_eq!(
            serde_json::to_value(counts).unwrap(),
            serde_json::json!({"threads": 1, "messages": 3, "events": 0})
        );
    }
}
