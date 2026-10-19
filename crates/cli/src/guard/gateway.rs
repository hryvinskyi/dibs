//! The project's server as the guard reaches it: the client, signed in on first use, each answer
//! remembered in the state directory as it arrives.

use std::cell::OnceCell;

use time::OffsetDateTime;
use tokio::runtime::Runtime;

use dibs_agent_host::ports::{
    CoordinationGateway, InboxView, KnownSettings, PathAnswer, PortError, SessionRef, StatusView,
};
use dibs_client::Client;
use dibs_client::api::EditCheckRequest;
use dibs_client::checkout::Checkout;
use dibs_domain::paths::RelPath;
use dibs_domain::repo::RepoId;

use crate::failure::sentence;
use crate::guard::remote::{Remote, RemoteError};
use crate::guard::views;

/// The server of one checkout's project. Nothing is read or built until the first call.
pub struct HookGateway {
    remote: Remote,
    checkout: Checkout,
    runtime: OnceCell<Runtime>,
    client: OnceCell<Client>,
}

impl HookGateway {
    /// The gateway of `checkout`'s project.
    pub fn new(remote: Remote, checkout: Checkout) -> Self {
        Self {
            remote,
            checkout,
            runtime: OnceCell::new(),
            client: OnceCell::new(),
        }
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

fn port_error(error: &RemoteError) -> PortError {
    match error {
        RemoteError::Client(error) => views::port_error(error),
        RemoteError::Config(text) => PortError::Refused(sentence(text)),
    }
}

impl CoordinationGateway for HookGateway {
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
        Ok(views::inbox(&inbox))
    }

    fn edit_check(
        &self,
        session: &SessionRef,
        repo: &RepoId,
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
