//! The guard: one use case for every host. It reads a host's event through the host's adapter,
//! decides from what this machine remembers and, only when that cannot answer, from the server,
//! and hands the adapter a neutral outcome to render.
//!
//! What it holds to, whatever the host:
//! - **Failure is never a verdict.** A configuration that cannot be used, an unreadable input, an
//!   unknown host or event, a server that cannot be reached: each becomes no objection with a
//!   one-line notice. Only the server's own refusal of an edit denies.
//! - **No upkeep is waited for.** Heartbeats and refreshed caches are started in the background
//!   and used from the next event on. The only call a hook waits for is the verdict on a path the
//!   cache cannot answer: a governed path in an area the member is not known to hold.
//! - **A server that failed is left alone for a minute**, and the person is warned at most once a
//!   minute meanwhile.
//! - **A checkout that governs no repository of the project** (its `repo` missing, unknown, or not
//!   the one its remote is) lets every edit through and says so at most once a minute, not on
//!   every edit.
//! - **Nothing is produced that the host cannot carry**, and nothing is fetched only to feed such
//!   an answer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::repo::RepoId;
use dibs_domain::settings::{EditPolicy, ProjectSettings};

use crate::adapter::{HostAdapter, HostReply};
use crate::capabilities::Capabilities;
use crate::event::{EventKind, HostEvent, ToolCall};
use crate::outcome::GuardOutcome;
use crate::ports::{
    Cached, Clock, CoordinationGateway, KnownSettings, PathAnswer, PortError, SessionRef,
    Workspace, Workspaces,
};
use crate::registry::HostRegistry;
use crate::text::Text;
use crate::upkeep::{
    DEFAULT_HEARTBEAT, Due, Freshness, INBOX_FRESH, STATUS_FRESH, WARN_EVERY, circuit_open,
    is_fresh,
};
use crate::wording;

/// One run of `dibs hook <host> <event>`.
#[derive(Debug, Clone, Copy)]
pub struct HookRequest<'a> {
    /// The host's id as given.
    pub host: &'a str,
    /// The event's name as given.
    pub event: &'a str,
    /// The host's input.
    pub input: &'a [u8],
    /// Whether the guard is switched off on this machine.
    pub switched_off: bool,
}

/// What a hook run answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAnswer {
    /// The host's reply.
    pub reply: HostReply,
    /// A line for standard error, when no adapter could carry it.
    pub diagnostic: Option<Text>,
    /// The kind of the event, when the input could be read.
    pub kind: Option<EventKind>,
}

/// Why the guard could not decide.
enum Problem {
    /// The server could not be reached, now or within the last minute.
    Unreachable(String),
    /// The checkout governs no repository of the project's settings.
    Repository(String),
    /// Anything else, with its sentence.
    Refused(String),
}

impl From<PortError> for Problem {
    fn from(error: PortError) -> Self {
        match error {
            PortError::Unavailable(sentence) => Self::Unreachable(sentence),
            PortError::Refused(sentence) => Self::Refused(sentence),
        }
    }
}

/// The guard.
pub struct Guard<'a> {
    workspaces: &'a dyn Workspaces,
    clock: &'a dyn Clock,
}

impl<'a> Guard<'a> {
    /// A guard over these ports.
    pub fn new(workspaces: &'a dyn Workspaces, clock: &'a dyn Clock) -> Self {
        Self { workspaces, clock }
    }

    /// Answers one hook run. Never fails: every problem is an answer of no objection.
    pub fn answer(&self, registry: &HostRegistry, request: &HookRequest<'_>) -> HookAnswer {
        let Some(adapter) = registry.get(request.host) else {
            let diagnostic = (!request.switched_off)
                .then(|| Text::new(wording::unknown_host(request.host, &registry.ids())));
            return HookAnswer {
                reply: HostReply::empty(),
                diagnostic,
                kind: None,
            };
        };
        let event = adapter.parse(request.event, request.input);
        let kind = event.as_ref().ok().map(HostEvent::kind);
        let outcome = match event {
            _ if request.switched_off => GuardOutcome::silent(),
            Ok(event) => self.decide(adapter, &event),
            Err(error) => GuardOutcome::notice(wording::unreadable(&error.to_string())),
        };
        HookAnswer {
            reply: adapter.render(request.event, &outcome),
            diagnostic: None,
            kind,
        }
    }

    /// The outcome of one event of the host `adapter` speaks for.
    pub fn decide(&self, adapter: &dyn HostAdapter, event: &HostEvent) -> GuardOutcome {
        let session = SessionRef {
            host: adapter.id(),
            key: event.session_key().clone(),
        };
        let workspace = match self.workspaces.open(event.cwd()) {
            Ok(Some(workspace)) => workspace,
            Ok(None) => return GuardOutcome::silent(),
            Err(error) => return GuardOutcome::notice(wording::failed(&error.to_string())),
        };
        let run = Run {
            workspace: workspace.as_ref(),
            capabilities: adapter.capabilities(),
            session,
            now: self.clock.now(),
        };
        run.event(event)
    }
}

/// One event in one governed checkout.
struct Run<'w> {
    workspace: &'w dyn Workspace,
    capabilities: Capabilities,
    session: SessionRef,
    now: OffsetDateTime,
}

impl Run<'_> {
    fn event(&self, event: &HostEvent) -> GuardOutcome {
        let decided = match event {
            HostEvent::SessionStarted { .. } => self.session_started(),
            HostEvent::BeforeTool { cwd, call, .. } => self.before_tool(cwd, call),
            HostEvent::AfterTool { .. } | HostEvent::PromptSubmitted { .. } => {
                Ok(GuardOutcome::silent())
            }
        };
        let outcome = match decided {
            Ok(outcome) => outcome,
            Err(problem) => self.no_decision(problem),
        };
        if matches!(event, HostEvent::BeforeTool { .. }) {
            return outcome;
        }
        self.keep_up(outcome)
    }

    fn session_started(&self) -> Result<GuardOutcome, Problem> {
        if !self.capabilities.context_at_start {
            return Ok(GuardOutcome::silent());
        }
        let known = self.settings()?;
        let cache = self.workspace.cache();
        let mut status = cache.status();
        let mut inbox = cache.inbox();
        let stale =
            |at: Option<OffsetDateTime>, limit| at.is_none_or(|at| !is_fresh(at, self.now, limit));
        let mut notice = None;
        if stale(status.as_ref().map(|s| s.at), STATUS_FRESH)
            || stale(inbox.as_ref().map(|i| i.at), INBOX_FRESH)
        {
            match self.call(|gateway| Ok((gateway.status()?, gateway.inbox()?))) {
                Ok((fresh_status, fresh_inbox)) => {
                    status = Some(Cached {
                        value: fresh_status,
                        at: self.now,
                    });
                    inbox = Some(Cached {
                        value: fresh_inbox,
                        at: self.now,
                    });
                }
                Err(problem) => {
                    notice = self.problem_notice(problem);
                }
            }
        }
        let status_age = status
            .as_ref()
            .filter(|cached| !is_fresh(cached.at, self.now, STATUS_FRESH))
            .map(|cached| self.now - cached.at);
        let brief = wording::Brief {
            member: &known.value.member,
            policy: known.value.settings.edit_policy(),
            status: status.as_ref().map(|cached| &cached.value),
            inbox: inbox.as_ref().map(|cached| &cached.value),
            status_age,
        };
        Ok(GuardOutcome::NoObjection {
            context: Some(Text::new(brief.text())),
            notice,
        })
    }

    fn before_tool(&self, cwd: &Path, call: &ToolCall) -> Result<GuardOutcome, Problem> {
        match call {
            ToolCall::Edit { paths } if self.capabilities.deny_edit => self.edits(cwd, paths),
            ToolCall::Edit { .. } | ToolCall::Shell { .. } | ToolCall::Other => {
                Ok(GuardOutcome::silent())
            }
        }
    }

    fn edits(&self, cwd: &Path, paths: &[PathBuf]) -> Result<GuardOutcome, Problem> {
        let mut inside: Vec<RelPath> = Vec::new();
        for path in paths {
            if let Some(relative) = self.workspace.relative(cwd, path)
                && !inside.contains(&relative)
            {
                inside.push(relative);
            }
        }
        if inside.is_empty() {
            return Ok(GuardOutcome::silent());
        }
        let known = self.settings()?;
        let settings = &known.value.settings;
        let repo = self
            .workspace
            .repo(settings)
            .map_err(|error| Problem::Repository(error.to_string()))?;
        let governed: Vec<RelPath> = inside
            .into_iter()
            .filter(|path| {
                settings.is_governed(&RepoPath {
                    repo: repo.clone(),
                    path: path.clone(),
                })
            })
            .collect();
        if governed.is_empty() {
            return Ok(GuardOutcome::silent());
        }
        let held = self.held_by_me(&known.value);
        let ask: Vec<RelPath> = governed
            .into_iter()
            .filter(|path| !answered_by_hold(settings, &repo, path, &held))
            .collect();
        if ask.is_empty() {
            return Ok(GuardOutcome::silent());
        }
        let answers = self.call(|gateway| gateway.edit_check(&self.session, &repo, &ask))?;
        if answers.len() != ask.len() {
            return Err(Problem::Unreachable(
                "the server answered for a different number of files than it was asked about."
                    .to_owned(),
            ));
        }
        Ok(self.verdicts(settings, &repo, &ask, &answers))
    }

    fn verdicts(
        &self,
        settings: &ProjectSettings,
        repo: &RepoId,
        paths: &[RelPath],
        answers: &[PathAnswer],
    ) -> GuardOutcome {
        let mut refusals = Vec::new();
        let mut notes = Vec::new();
        let mut now_held = Vec::new();
        for (path, answer) in paths.iter().zip(answers) {
            let area = area_of(settings, repo, path);
            match answer {
                PathAnswer::Allowed => now_held.extend(area),
                PathAnswer::Granted { .. } => {}
                PathAnswer::Noted {
                    rule,
                    note,
                    touched_by,
                } => notes.push(wording::noted(path, rule, note.as_ref(), touched_by)),
                PathAnswer::NeedsClaim => refusals.push(wording::needs_claim(path, area.as_ref())),
                PathAnswer::HeldBy { holder, item, area } => {
                    refusals.push(wording::held_by(path, holder.as_ref(), *item, area));
                }
            }
        }
        if settings.edit_policy() == EditPolicy::RequireClaim && !now_held.is_empty() {
            self.workspace.cache().note_holds(&now_held, self.now);
        }
        if !refusals.is_empty() {
            return GuardOutcome::Deny {
                reason: Text::new(refusals.join("\n")),
            };
        }
        if notes.is_empty() {
            GuardOutcome::silent()
        } else {
            GuardOutcome::context(notes.join("\n"))
        }
    }

    /// The areas the member is known to hold, from a fresh status or a recent edit check.
    fn held_by_me(&self, known: &KnownSettings) -> BTreeSet<QualifiedArea> {
        let cache = self.workspace.cache();
        let mut held: BTreeSet<QualifiedArea> = cache
            .noted_holds()
            .into_iter()
            .filter(|noted| is_fresh(noted.at, self.now, STATUS_FRESH))
            .map(|noted| noted.value)
            .collect();
        if let Some(status) = cache.status()
            && is_fresh(status.at, self.now, STATUS_FRESH)
        {
            held.extend(
                status
                    .value
                    .holds
                    .into_iter()
                    .filter(|hold| hold.holder == known.member)
                    .map(|hold| hold.area),
            );
        }
        held
    }

    /// The settings as cached, or fetched when this machine has none yet.
    fn settings(&self) -> Result<Cached<KnownSettings>, Problem> {
        if let Some(cached) = self.workspace.cache().settings()? {
            return Ok(cached);
        }
        let value = self.call(|gateway| gateway.settings())?;
        Ok(Cached {
            value,
            at: self.now,
        })
    }

    /// A call to the server, unless it failed within the last minute. A failure opens the
    /// circuit; a success closes it.
    fn call<T>(
        &self,
        call: impl FnOnce(&dyn CoordinationGateway) -> Result<T, PortError>,
    ) -> Result<T, Problem> {
        let cache = self.workspace.cache();
        let since = cache.unreachable_since();
        if circuit_open(since, self.now) {
            return Err(Problem::Unreachable(
                "the server could not be reached a moment ago.".to_owned(),
            ));
        }
        match call(self.workspace.gateway()) {
            Ok(value) => {
                if since.is_some() {
                    cache.clear_unreachable();
                }
                Ok(value)
            }
            Err(PortError::Unavailable(sentence)) => {
                cache.mark_unreachable(self.now);
                Err(Problem::Unreachable(sentence))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn no_decision(&self, problem: Problem) -> GuardOutcome {
        GuardOutcome::NoObjection {
            context: None,
            notice: self.problem_notice(problem),
        }
    }

    /// The notice for a problem; for an unreachable server and for a checkout that governs no
    /// repository, only once a minute.
    fn problem_notice(&self, problem: Problem) -> Option<Text> {
        match problem {
            Problem::Repository(sentence) => {
                let cache = self.workspace.cache();
                if cache
                    .last_repo_warning()
                    .is_some_and(|at| is_fresh(at, self.now, WARN_EVERY))
                {
                    return None;
                }
                cache.mark_repo_warning(self.now);
                Some(Text::new(wording::failed(&sentence)))
            }
            Problem::Unreachable(sentence) => {
                let cache = self.workspace.cache();
                if cache
                    .last_warning()
                    .is_some_and(|at| is_fresh(at, self.now, WARN_EVERY))
                {
                    return None;
                }
                cache.mark_warning(self.now);
                Some(Text::new(wording::unreachable(&sentence)))
            }
            Problem::Refused(sentence) => Some(Text::new(wording::failed(&sentence))),
        }
    }

    /// Starts the background upkeep when something is due and the server is not known to be
    /// unreachable; a failure to start it joins the outcome as a notice.
    fn keep_up(&self, outcome: GuardOutcome) -> GuardOutcome {
        let cache = self.workspace.cache();
        if circuit_open(cache.unreachable_since(), self.now) {
            return outcome;
        }
        let settings = cache.settings().ok().flatten();
        let freshness = Freshness {
            heartbeat_every: settings.as_ref().map_or(DEFAULT_HEARTBEAT, |cached| {
                cached.value.settings.heartbeat_every()
            }),
            last_heartbeat: cache.last_heartbeat(),
            settings_at: settings.as_ref().map(|cached| cached.at),
            status_at: cache.status().map(|cached| cached.at),
            inbox_at: cache.inbox().map(|cached| cached.at),
        };
        if !Due::at(self.now, &freshness).any() {
            return outcome;
        }
        match self.workspace.upkeep().start(&self.session, self.now) {
            Ok(_) => outcome,
            Err(error) => with_notice(outcome, wording::upkeep_failed(&error.to_string())),
        }
    }
}

/// The outcome with `notice` added, unless it already carries one or denies.
fn with_notice(outcome: GuardOutcome, notice: String) -> GuardOutcome {
    match outcome {
        GuardOutcome::NoObjection {
            context,
            notice: None,
        } => GuardOutcome::NoObjection {
            context,
            notice: Some(Text::new(notice)),
        },
        other => other,
    }
}

/// The area containing a path of the repository, when one does.
fn area_of(settings: &ProjectSettings, repo: &RepoId, path: &RelPath) -> Option<QualifiedArea> {
    let spec = settings.repo(repo)?;
    spec.area_for(path).map(|area| spec.qualify(&area.name))
}

/// Whether the member's own hold answers for a path: it lies in an area they hold and no
/// hot-file rule has a say on it.
fn answered_by_hold(
    settings: &ProjectSettings,
    repo: &RepoId,
    path: &RelPath,
    held: &BTreeSet<QualifiedArea>,
) -> bool {
    let Some(spec) = settings.repo(repo) else {
        return false;
    };
    spec.hot_rule_for(path).is_none()
        && area_of(settings, repo, path).is_some_and(|area| held.contains(&area))
}

#[cfg(test)]
mod tests;
