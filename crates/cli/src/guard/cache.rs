//! What the guard remembers, kept in the client's state directory of the project.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use dibs_agent_host::ports::{
    Cached, InboxView, KnownSettings, LocalCache, NewerRelease, PortError, SlotState, StatusView,
};
use dibs_agent_host::upkeep::STATUS_FRESH;
use dibs_client::state::{StateDir, read_json, write_json};
use dibs_domain::area::QualifiedArea;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::settings::ProjectSettings;

use crate::guard::views;
use crate::release;

/// The project's state directory, read and written for the guard, for the checkout at `root`. A
/// write that fails is logged and otherwise ignored: what was not remembered is fetched again
/// later.
pub struct StateCache {
    state: StateDir,
    root: PathBuf,
}

/// The file holding, per checkout root, when the person was last told that the checkout governs
/// no repository of the project: several checkouts of one project share its state directory.
const REPO_WARNINGS: &str = "repo-warned.json";

/// When each checkout's repository warning was last given.
#[derive(Default, Serialize, Deserialize)]
struct RepoWarnings {
    #[serde(default)]
    checkouts: BTreeMap<PathBuf, Moment>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Moment {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

impl StateCache {
    /// The cache over `state`, for the checkout at `root`.
    pub fn new(state: StateDir, root: &Path) -> Self {
        Self {
            state,
            root: root.to_path_buf(),
        }
    }

    fn repo_warnings(&self) -> RepoWarnings {
        read_json(&self.state.file(REPO_WARNINGS)).unwrap_or_default()
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

    fn slot_states(&self) -> Option<Cached<Vec<SlotState>>> {
        self.state.status().map(|cached| Cached {
            value: cached.status.slots.iter().map(views::slot_state).collect(),
            at: cached.fetched_at,
        })
    }

    fn inbox(&self) -> Option<Cached<InboxView>> {
        self.state.inbox().map(|cached| Cached {
            value: views::inbox(&cached.inbox, &cached.claims),
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

    fn last_repo_warning(&self) -> Option<OffsetDateTime> {
        self.repo_warnings()
            .checkouts
            .get(&self.root)
            .map(|moment| moment.at)
    }

    fn mark_repo_warning(&self, at: OffsetDateTime) {
        let mut warnings = self.repo_warnings();
        warnings.checkouts.insert(self.root.clone(), Moment { at });
        logged(
            write_json(&self.state.file(REPO_WARNINGS), &warnings),
            "the repository warning mark",
        );
    }

    fn versions_checked_at(&self) -> Option<OffsetDateTime> {
        self.state.versions().map(|cached| cached.checked_at)
    }

    fn newer_release(&self) -> Option<NewerRelease> {
        let installed = release::installed().ok()?;
        let latest = self.state.versions()?.versions.latest_client?;
        (latest > installed).then(|| NewerRelease {
            installed: installed.to_string(),
            latest: latest.to_string(),
        })
    }
}
