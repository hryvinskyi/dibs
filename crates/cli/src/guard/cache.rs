//! What the guard remembers, kept in the client's state directory of the project.

use time::OffsetDateTime;

use dibs_agent_host::ports::{Cached, InboxView, KnownSettings, LocalCache, PortError, StatusView};
use dibs_agent_host::upkeep::STATUS_FRESH;
use dibs_client::state::StateDir;
use dibs_domain::area::QualifiedArea;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::settings::ProjectSettings;

use crate::guard::views;

/// The project's state directory, read and written for the guard. A write that fails is logged
/// and otherwise ignored: what was not remembered is fetched again later.
pub struct StateCache {
    state: StateDir,
}

impl StateCache {
    /// The cache over `state`.
    pub fn new(state: StateDir) -> Self {
        Self { state }
    }
}

fn logged(result: std::io::Result<()>, what: &str) {
    if let Err(error) = result {
        tracing::debug!(error = %error, what, "the state could not be written");
    }
}

impl LocalCache for StateCache {
    fn settings(&self) -> Result<Option<Cached<KnownSettings>>, PortError> {
        let (Some(cached), Some(member)) = (self.state.settings(), self.state.member()) else {
            return Ok(None);
        };
        let settings = ProjectSettings::parse(&cached.settings, &HotRuleRegistry::standard())
            .map_err(|error| {
                PortError::Refused(format!(
                    "the project's settings use something this dibs does not know ({error}); update dibs."
                ))
            })?;
        Ok(Some(Cached {
            value: KnownSettings { member, settings },
            at: cached.fetched_at,
        }))
    }

    fn status(&self) -> Option<Cached<StatusView>> {
        self.state.status().map(|cached| Cached {
            value: views::status(&cached.status),
            at: cached.fetched_at,
        })
    }

    fn inbox(&self) -> Option<Cached<InboxView>> {
        self.state.inbox().map(|cached| Cached {
            value: views::inbox(&cached.inbox),
            at: cached.fetched_at,
        })
    }

    fn noted_holds(&self) -> Vec<Cached<QualifiedArea>> {
        self.state
            .noted_holds()
            .into_iter()
            .map(|hold| Cached {
                value: hold.area,
                at: hold.at,
            })
            .collect()
    }

    fn note_holds(&self, areas: &[QualifiedArea], at: OffsetDateTime) {
        logged(self.state.note_holds(areas, at, STATUS_FRESH), "held areas");
    }

    fn last_heartbeat(&self) -> Option<OffsetDateTime> {
        self.state.heartbeat_at()
    }

    fn unreachable_since(&self) -> Option<OffsetDateTime> {
        self.state.unreachable_since()
    }

    fn mark_unreachable(&self, at: OffsetDateTime) {
        logged(self.state.mark_unreachable(at), "the unreachable mark");
    }

    fn clear_unreachable(&self) {
        logged(self.state.clear_unreachable(), "the unreachable mark");
    }

    fn last_warning(&self) -> Option<OffsetDateTime> {
        self.state.warned_at()
    }

    fn mark_warning(&self, at: OffsetDateTime) {
        logged(self.state.remember_warning(at), "the warning mark");
    }
}
