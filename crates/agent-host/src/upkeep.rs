//! When the caches need the server again, when the signs of life left in pool worktrees are to be
//! sent, and when the server counts as unreachable. Both the guard, which starts the upkeep, and the
//! upkeep itself, which does what is due, decide by these rules, so the two never disagree.

use time::{Duration, OffsetDateTime};

/// How long cached settings answer without a refresh being due.
pub const SETTINGS_FRESH: Duration = Duration::minutes(5);

/// How long a cached status answers for the member's own holds, and how long before it is due again.
pub const STATUS_FRESH: Duration = Duration::seconds(30);

/// How long a cached inbox stands before a refresh is due.
pub const INBOX_FRESH: Duration = Duration::seconds(30);

/// How long after a failed call the server is not tried again.
pub const UNREACHABLE_FOR: Duration = Duration::seconds(60);

/// How often the person is warned, at most, while the server cannot be reached.
pub const WARN_EVERY: Duration = Duration::seconds(60);

/// The heartbeat interval while the project's settings are not known yet.
pub const DEFAULT_HEARTBEAT: Duration = Duration::minutes(1);

/// How often, at most, the server is asked which client it recommends, and a newer one installed.
pub const UPDATE_EVERY: Duration = Duration::days(1);

/// How often, at most, the signs of life left in pool worktrees are sent. It does not follow the
/// project's heartbeat interval: a hold of an agent ends after minutes of silence, and a long
/// interval must never bring a working agent near that.
pub const WORKTREE_BEATS_EVERY: Duration = Duration::minutes(1);

/// When each cache was last filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    /// The project's heartbeat interval.
    pub heartbeat_every: Duration,
    /// The last heartbeat from this machine.
    pub last_heartbeat: Option<OffsetDateTime>,
    /// When the settings were fetched.
    pub settings_at: Option<OffsetDateTime>,
    /// When the status was fetched.
    pub status_at: Option<OffsetDateTime>,
    /// When the inbox was fetched.
    pub inbox_at: Option<OffsetDateTime>,
    /// When the server was last asked which client it recommends.
    pub versions_at: Option<OffsetDateTime>,
    /// The newest sign of life left in a pool worktree on this machine.
    pub newest_worktree_beat: Option<OffsetDateTime>,
    /// When the signs of life in pool worktrees were last sent.
    pub worktree_beats_sent_at: Option<OffsetDateTime>,
}

/// What is due.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Due {
    /// The settings are old or unknown.
    pub settings: bool,
    /// A heartbeat is due.
    pub heartbeat: bool,
    /// The status is old or unknown.
    pub status: bool,
    /// The inbox is old or unknown.
    pub inbox: bool,
    /// The server was not asked within a day which client it recommends.
    pub update: bool,
    /// A sign of life in a pool worktree is newer than the last sending, and that sending is
    /// [`WORKTREE_BEATS_EVERY`] old.
    pub worktree_beats: bool,
}

impl Due {
    /// What is due at `now`. Something never fetched is due; something fetched in the future (a
    /// clock that moved back) is due too, so a wrong clock never freezes a cache.
    pub fn at(now: OffsetDateTime, freshness: &Freshness) -> Self {
        let older = |at: Option<OffsetDateTime>, limit: Duration| {
            at.is_none_or(|at| at > now || now - at >= limit)
        };
        Self {
            settings: older(freshness.settings_at, SETTINGS_FRESH),
            heartbeat: older(freshness.last_heartbeat, freshness.heartbeat_every),
            status: older(freshness.status_at, STATUS_FRESH),
            inbox: older(freshness.inbox_at, INBOX_FRESH),
            update: older(freshness.versions_at, UPDATE_EVERY),
            worktree_beats: freshness.newest_worktree_beat.is_some_and(|beat| {
                freshness
                    .worktree_beats_sent_at
                    .is_none_or(|sent| beat > sent)
            }) && older(freshness.worktree_beats_sent_at, WORKTREE_BEATS_EVERY),
        }
    }

    /// Whether anything is due.
    pub fn any(&self) -> bool {
        self.settings
            || self.heartbeat
            || self.status
            || self.inbox
            || self.update
            || self.worktree_beats
    }

    /// Only what asks the server nothing about the project: the check for a newer client, when it
    /// is due.
    #[must_use]
    pub fn update_only(self) -> Self {
        Self {
            update: self.update,
            ..Self::default()
        }
    }
}

/// Whether a value of `at` is still fresh at `now` for `limit`.
pub fn is_fresh(at: OffsetDateTime, now: OffsetDateTime, limit: Duration) -> bool {
    at <= now && now - at < limit
}

/// Whether the server counts as unreachable at `now`, given when a call last failed.
pub fn circuit_open(unreachable_since: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
    unreachable_since.is_some_and(|since| is_fresh(since, now, UNREACHABLE_FOR))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

    fn fresh_all(now: OffsetDateTime) -> Freshness {
        Freshness {
            heartbeat_every: Duration::minutes(1),
            last_heartbeat: Some(now),
            settings_at: Some(now),
            status_at: Some(now),
            inbox_at: Some(now),
            versions_at: Some(now),
            newest_worktree_beat: None,
            worktree_beats_sent_at: None,
        }
    }

    #[test]
    fn nothing_is_due_right_after_everything_was_fetched() {
        let now = NOW + Duration::days(1);
        assert!(!Due::at(now, &fresh_all(now)).any());
    }

    #[test]
    fn each_cache_falls_due_at_its_own_age() {
        let then = NOW + Duration::days(1);
        let due = |after: Duration| Due::at(then + after, &fresh_all(then));
        assert_eq!(
            due(Duration::seconds(29)),
            Due::default(),
            "everything stands for 29 s"
        );
        let at_half_minute = due(Duration::seconds(30));
        assert!(at_half_minute.status && at_half_minute.inbox);
        assert!(!at_half_minute.heartbeat && !at_half_minute.settings);
        assert!(due(Duration::minutes(1)).heartbeat);
        assert!(!due(Duration::minutes(4)).settings);
        assert!(due(Duration::minutes(5)).settings);
    }

    #[test]
    fn the_heartbeat_follows_the_projects_interval() {
        let then = NOW + Duration::days(1);
        let mut freshness = fresh_all(then);
        freshness.heartbeat_every = Duration::minutes(5);
        assert!(!Due::at(then + Duration::minutes(4), &freshness).heartbeat);
        assert!(Due::at(then + Duration::minutes(5), &freshness).heartbeat);
    }

    #[test]
    fn what_was_never_fetched_or_comes_from_the_future_is_due() {
        let now = NOW + Duration::days(1);
        let never = Freshness {
            heartbeat_every: DEFAULT_HEARTBEAT,
            last_heartbeat: None,
            settings_at: None,
            status_at: None,
            inbox_at: None,
            versions_at: None,
            newest_worktree_beat: None,
            worktree_beats_sent_at: None,
        };
        let due = Due::at(now, &never);
        assert!(due.settings && due.heartbeat && due.status && due.inbox && due.update);
        assert!(!due.worktree_beats, "no sign of life left, nothing to send");
        let future = fresh_all(now + Duration::hours(1));
        assert!(Due::at(now, &future).status);
    }

    #[test]
    fn a_newer_client_is_looked_for_at_most_once_a_day() {
        let checked = NOW + Duration::days(10);
        let due = |after: Duration| Due::at(checked + after, &fresh_all(checked)).update;
        assert!(!due(Duration::ZERO));
        assert!(!due(Duration::hours(23) + Duration::minutes(59)));
        assert!(due(Duration::days(1)));
        assert!(due(-Duration::hours(1)), "a check from the future is due");
        let only = Due::at(checked + Duration::days(2), &fresh_all(checked)).update_only();
        assert_eq!(
            only,
            Due {
                update: true,
                ..Due::default()
            }
        );
        assert!(!Due::default().update_only().any());
    }

    #[test]
    fn signs_of_life_in_worktrees_are_due_when_newer_than_the_last_sending_a_minute_old() {
        let sent = NOW + Duration::days(3);
        let due = |beat: Option<OffsetDateTime>, sent_at: Option<OffsetDateTime>, now| {
            let mut freshness = fresh_all(now);
            freshness.newest_worktree_beat = beat;
            freshness.worktree_beats_sent_at = sent_at;
            Due::at(now, &freshness)
        };
        let later = sent + Duration::seconds(10);
        assert!(due(Some(later), None, later).worktree_beats, "never sent");
        assert!(
            !due(Some(later), Some(sent), later + Duration::seconds(49)).worktree_beats,
            "a sending less than a minute old holds the next one back"
        );
        let minute = sent + Duration::minutes(1);
        let fresh = due(Some(later), Some(sent), minute);
        assert!(fresh.worktree_beats && fresh.any());
        assert!(
            !due(Some(sent - Duration::seconds(1)), Some(sent), minute).worktree_beats,
            "every sign of life was sent"
        );
        assert!(
            !due(None, Some(sent), sent + Duration::hours(1)).worktree_beats,
            "no worktree, nothing to send"
        );
        assert!(
            due(
                Some(sent + Duration::hours(2)),
                Some(sent + Duration::hours(1)),
                sent
            )
            .worktree_beats,
            "a sending from the future is due"
        );
    }

    #[test]
    fn the_heartbeat_interval_never_holds_signs_of_life_in_worktrees_back() {
        let then = NOW + Duration::days(5);
        let mut freshness = fresh_all(then);
        freshness.heartbeat_every = Duration::hours(1);
        freshness.worktree_beats_sent_at = Some(then);
        freshness.newest_worktree_beat = Some(then + Duration::seconds(30));
        let due = Due::at(then + Duration::minutes(1), &freshness);
        assert!(due.worktree_beats);
        assert!(!due.heartbeat);
        assert!(
            !due.update_only().worktree_beats,
            "nothing for the project while the server is unreachable"
        );
    }

    #[test]
    fn the_circuit_stays_open_for_a_minute_after_a_failure() {
        let failed = NOW + Duration::days(1);
        assert!(!circuit_open(None, failed));
        assert!(circuit_open(Some(failed), failed));
        assert!(circuit_open(Some(failed), failed + Duration::seconds(59)));
        assert!(!circuit_open(Some(failed), failed + Duration::seconds(60)));
        assert!(!circuit_open(Some(failed + Duration::hours(1)), failed));
    }
}
