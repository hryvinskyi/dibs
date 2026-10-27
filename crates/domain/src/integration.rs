//! Whether a project's integrations are heard from. An integration reports merges and sets pull
//! request statuses; while it is silent, merges complete no claims and statuses go stale, so the
//! people of the project are told.

use time::{Duration, OffsetDateTime};

use crate::claim::Claim;
use crate::member::Role;
use crate::settings::ProjectSettings;

/// How long an integration may go without a call before it counts as silent.
pub const SILENT_AFTER: Duration = Duration::minutes(15);

/// How often an integration's calls are written down: a call within this time of the one written
/// last is not written again, so a worker that polls does not write on every request.
pub const CALL_RECORDED_EVERY: Duration = Duration::minutes(1);

/// What a request is, as the integrations' sign of life goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// Any other request.
    Ordinary,
    /// A request only an integration's job makes: reporting the tracker's items, a merge, a close,
    /// a pull request's state, a branch's footprint or a run's end, or reading the claims in review
    /// or the statuses to set.
    Integration,
}

/// Whether a call by a member of `role` is a sign that an integration's job runs. Every call of an
/// integration's own token is; an admin may run that job under their own token, and then only the
/// calls the job makes are.
pub const fn signals_integration(role: Role, call: CallKind) -> bool {
    match call {
        CallKind::Ordinary => !role.is_person(),
        CallKind::Integration => !role.is_person() || role.may_import(),
    }
}

/// Whether a call at `now` is to be written, the last written being `last`.
pub fn call_due(last: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
    last.is_none_or(|last| now - last >= CALL_RECORDED_EVERY)
}

/// Whether the project relies on an integration for its reviews: a claim is in review, waiting for
/// a merge report, or a repository's pull requests get a status. Only then is its silence news; a
/// quiet importer of a project that uses neither is no alarm.
pub fn review_in_use(claims: &[Claim], settings: &ProjectSettings) -> bool {
    claims.iter().any(|claim| claim.state.is_in_review())
        || settings.repos().iter().any(|repo| repo.pr_status.enabled)
}

/// One integration's last sign of life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationSign {
    /// When it joined the project.
    pub joined: OffsetDateTime,
    /// When it last called, if it ever did.
    pub last_call: Option<OffsetDateTime>,
}

impl IntegrationSign {
    /// Its last call, or, when it never called, when it joined.
    pub fn last_heard(&self) -> OffsetDateTime {
        self.last_call.unwrap_or(self.joined)
    }
}

/// Since when the project's integrations are silent: the latest sign of life among them, when that
/// is more than [`SILENT_AFTER`] before `now`. `None` when one was heard from within that time or
/// the project has no integration.
pub fn silent_since(signs: &[IntegrationSign], now: OffsetDateTime) -> Option<OffsetDateTime> {
    let latest = signs.iter().map(IntegrationSign::last_heard).max()?;
    (now - latest > SILENT_AFTER).then_some(latest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::at;

    #[test]
    fn an_integrations_token_signals_on_every_call_and_an_admin_only_on_the_jobs_calls() {
        assert!(signals_integration(Role::Importer, CallKind::Ordinary));
        assert!(signals_integration(Role::Importer, CallKind::Integration));
        assert!(signals_integration(Role::Admin, CallKind::Integration));
        assert!(!signals_integration(Role::Admin, CallKind::Ordinary));
        assert!(!signals_integration(Role::Member, CallKind::Integration));
        assert!(!signals_integration(Role::Member, CallKind::Ordinary));
    }

    #[test]
    fn integrations_are_silent_once_none_called_for_fifteen_minutes() {
        let board = IntegrationSign {
            joined: at(0),
            last_call: Some(at(10)),
        };
        assert_eq!(
            silent_since(&[board], at(25)),
            None,
            "fifteen minutes is not yet"
        );
        assert_eq!(silent_since(&[board], at(26)), Some(at(10)));
        let worker = IntegrationSign {
            joined: at(0),
            last_call: Some(at(20)),
        };
        assert_eq!(
            silent_since(&[board, worker], at(30)),
            None,
            "one integration heard from is enough"
        );
        let never = IntegrationSign {
            joined: at(5),
            last_call: None,
        };
        assert_eq!(
            silent_since(&[never], at(30)),
            Some(at(5)),
            "silent since it joined"
        );
        assert_eq!(
            silent_since(&[], at(100)),
            None,
            "a project without integrations"
        );
    }

    #[test]
    fn a_call_is_written_at_most_once_a_minute() {
        assert!(call_due(None, at(0)));
        assert!(!call_due(Some(at(0)), at(0) + Duration::seconds(59)));
        assert!(call_due(Some(at(0)), at(1)));
    }

    #[test]
    fn silence_matters_only_where_reviews_rely_on_the_integration() {
        use crate::review::{ClaimState, PrRef, Review};
        use crate::testing::{ALICE, claim, settings};

        let plain = settings("{}");
        let working = [claim(1, ALICE, &[], 0)];
        assert!(!review_in_use(&working, &plain));
        let reviewing = [Claim {
            state: ClaimState::InReview(
                Review::new([PrRef::parse("acme/api#1").unwrap()].into(), at(0), false).unwrap(),
            ),
            ..claim(1, ALICE, &[], 0)
        }];
        assert!(review_in_use(&reviewing, &plain));
        let with_status = settings(r#"{"pr_status":{"enabled":true}}"#);
        assert!(review_in_use(&working, &with_status));
    }
}
