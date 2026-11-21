//! The project's server as the guard reaches it: the client, signed in on first use, each answer
//! remembered in the state directory as it arrives.

use std::cell::OnceCell;
use std::path::PathBuf;

use time::OffsetDateTime;
use tokio::runtime::Runtime;

use dibs_agent_host::ports::{
    CoordinationGateway, InboxView, KnownSettings, PathAnswer, PortError, SessionRef, SlotState,
    StatusView,
};
use dibs_client::api::{EditCheckRequest, InWorktree};
use dibs_client::checkout::Checkout;
use dibs_client::checkout_token::CheckoutKey;
use dibs_client::git_dirs::GitDirs;
use dibs_client::worktree_pool::{pool_id, read_marker, read_settings};
use dibs_client::{Client, ClientError};
use dibs_domain::paths::RelPath;
use dibs_domain::radar::BranchName;
use dibs_domain::repo::RepoId;
use dibs_domain::report::MachineId;
use dibs_domain::session::CheckoutToken;
use dibs_domain::slot::SlotName;

use crate::failure::sentence;
use crate::guard::checkout_key::checkout_key;
use crate::guard::machine::machine_id;
use crate::guard::remote::{Remote, RemoteError, session_label};
use crate::guard::views;

/// The server of one checkout's project. Nothing is read or built until the first call.
pub struct HookGateway {
    remote: Remote,
    checkout: Checkout,
    state_base: PathBuf,
    runtime: OnceCell<Runtime>,
    client: OnceCell<Client>,
    machine: OnceCell<Option<MachineId>>,
    key: OnceCell<Option<CheckoutKey>>,
    token: OnceCell<Option<CheckoutToken>>,
    worktree: OnceCell<Option<InWorktree>>,
}

impl HookGateway {
    /// The gateway of `checkout`'s project; `state_base` is the state directory that keeps this
    /// machine's id and its checkout key.
    pub fn new(remote: Remote, checkout: Checkout, state_base: PathBuf) -> Self {
        Self {
            remote,
            checkout,
            state_base,
            runtime: OnceCell::new(),
            client: OnceCell::new(),
            machine: OnceCell::new(),
            key: OnceCell::new(),
            token: OnceCell::new(),
            worktree: OnceCell::new(),
        }
    }

    /// This machine's id, read or made once; none when it can be neither read nor kept.
    fn machine(&self) -> Option<&MachineId> {
        self.machine
            .get_or_init(|| machine_id(&self.state_base))
            .as_ref()
    }

    /// This machine's checkout key, read or made once; none when it can be neither read nor kept.
    fn key(&self) -> Option<&CheckoutKey> {
        self.key
            .get_or_init(|| checkout_key(&self.state_base, &self.remote.principal()))
            .as_ref()
    }

    /// The token the checkout is named by on this machine, worked out once; none when the
    /// machine has no checkout key.
    fn token(&self) -> Option<&CheckoutToken> {
        self.token
            .get_or_init(|| self.checkout.token(self.key()?))
            .as_ref()
    }

    /// The pool worktree the checkout is, worked out once: its git directory's mark names the
    /// checkout that lends it and its name, and the pool's settings, read for that checkout, give
    /// the pool's id. None when the checkout bears no mark, the settings cannot be read, or the
    /// machine has no id.
    fn worktree(&self) -> Option<&InWorktree> {
        self.worktree
            .get_or_init(|| {
                let dirs = GitDirs::around(self.checkout.root())?;
                let mark = read_marker(&dirs)?;
                let settings = read_settings(&dirs, &mark.checkout).ok().flatten()?;
                Some(InWorktree {
                    machine: self.machine()?.clone(),
                    pool: pool_id(&settings.root).ok()?,
                    name: mark.name,
                })
            })
            .as_ref()
    }

    fn runtime(&self) -> Result<&Runtime, PortError> {
        if let Some(runtime) = self.runtime.get() {
            return Ok(runtime);
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| PortError::Refused("dibs could not start its runtime.".to_owned()))?;
        Ok(self.runtime.get_or_init(|| runtime))
    }

    fn client(&self) -> Result<&Client, PortError> {
        if let Some(client) = self.client.get() {
            return Ok(client);
        }
        let client = self.remote.client().map_err(|error| port_error(&error))?;
        Ok(self.client.get_or_init(|| client))
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Whether a failed end leaves nothing to end: the server does not know the session, or is older
/// than the call.
fn nothing_to_end(error: &ClientError) -> bool {
    matches!(error, ClientError::ServerTooOld { .. }) || error.code() == "not_found"
}

fn port_error(error: &RemoteError) -> PortError {
    match error {
        RemoteError::Client(error) => views::port_error(error),
        RemoteError::Config(text) => PortError::Refused(sentence(text)),
    }
}

impl CoordinationGateway for HookGateway {
    fn end_session(&self, session: &SessionRef) -> Result<(), PortError> {
        let Some(id) = session_label(session)
            .ok()
            .and_then(|label| self.remote.state.session(&label))
        else {
            return Ok(());
        };
        let client = self.client()?;
        match self.runtime()?.block_on(client.end_session(id)) {
            Ok(_) => Ok(()),
            Err(error) if nothing_to_end(&error) => Ok(()),
            Err(error) => Err(views::port_error(&error)),
        }
    }

    fn settings(&self) -> Result<KnownSettings, PortError> {
        let client = self.client()?;
        let me = self
            .runtime()?
            .block_on(self.remote.me(client, &self.checkout, Self::now()))
            .map_err(|error| port_error(&error))?;
        let settings = me.project_settings().map_err(|error| {
            PortError::Refused(format!(
                "the project's settings use something this dibs does not know ({error}); update dibs."
            ))
        })?;
        Ok(KnownSettings {
            member: me.member.handle,
            settings,
        })
    }

    fn status(&self) -> Result<StatusView, PortError> {
        let client = self.client()?;
        let status = self
            .runtime()?
            .block_on(client.status())
            .map_err(|error| views::port_error(&error))?;
        if let Err(error) = self.remote.state.remember_status(&status, Self::now()) {
            tracing::debug!(error = %error, "the status could not be cached");
        }
        Ok(views::status(&status))
    }

    fn inbox(&self) -> Result<InboxView, PortError> {
        let client = self.client()?;
        let inbox = self
            .runtime()?
            .block_on(client.inbox(views::GUARD_INBOX))
            .map_err(|error| views::port_error(&error))?;
        if let Err(error) = self.remote.state.remember_inbox(&inbox, Self::now()) {
            tracing::debug!(error = %error, "the inbox could not be cached");
        }
        Ok(views::inbox(&inbox, &[]))
    }

    fn slots(&self, names: &[SlotName]) -> Result<Vec<SlotState>, PortError> {
        let client = self.client()?;
        self.runtime()?.block_on(async {
            let mut states = Vec::with_capacity(names.len());
            for name in names {
                let answer = client
                    .slot(name)
                    .await
                    .map_err(|error| views::port_error(&error))?;
                states.push(views::slot_state(&answer.slot));
            }
            Ok(states)
        })
    }

    fn edit_check(
        &self,
        session: &SessionRef,
        repo: &RepoId,
        branch: Option<&BranchName>,
        paths: &[RelPath],
    ) -> Result<Vec<PathAnswer>, PortError> {
        let client = self.client()?;
        let runtime = self.runtime()?;
        let checked = runtime.block_on(async {
            let session_id = self.remote.session(client, session).await?;
            let request = EditCheckRequest {
                session_id,
                repo: repo.clone(),
                paths: paths.to_vec(),
                reserve: true,
                checkout: self.token().cloned(),
                branch: branch.cloned(),
                worktree: self.worktree().cloned(),
            };
            Ok::<_, RemoteError>(client.edit_check(&request).await?)
        });
        let checked = checked.map_err(|error| port_error(&error))?;
        Ok(checked
            .verdicts
            .iter()
            .map(|verdict| views::answer(&verdict.verdict))
            .collect())
    }
}
