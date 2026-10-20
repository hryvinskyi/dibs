//! The connection hooks and the background sync use: signed in without asking anyone (the OS
//! credential store is read within its deadline, a prompt counts as no token), with every call
//! bounded to two seconds, and the session a host's session is registered as.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;

use dibs_agent_host::ports::SessionRef;
use dibs_client::api::{Me, RegisterSession};
use dibs_client::checkout::Checkout;
use dibs_client::credentials::{CredentialKey, Credentials, OsKeychain, ReadMode, TokenLookup};
use dibs_client::locations::Locations;
use dibs_client::state::StateDir;
use dibs_client::{Client, ClientError, ClientOptions};
use dibs_domain::ids::SessionId;
use dibs_domain::message::SessionLabel;
use dibs_domain::session::{AgentLabel, HostName};

use crate::context::{host_name, label_on};
use crate::failure::sentence;

/// How long a call of a hook or of the background sync may take: to connect, and in all.
pub const CALL_LIMIT: Duration = Duration::from_secs(2);

/// Why the connection could not be made or a call failed.
#[derive(Debug, Clone)]
pub enum RemoteError {
    /// A call failed.
    Client(ClientError),
    /// The configuration or the credentials cannot be used; the sentence says how to fix it.
    Config(String),
}

impl From<ClientError> for RemoteError {
    fn from(error: ClientError) -> Self {
        Self::Client(error)
    }
}

/// The project of a checkout, as the hooks and the background sync reach it.
#[derive(Clone)]
pub struct Remote {
    /// The project and server.
    pub key: CredentialKey,
    /// What this machine remembers about it.
    pub state: StateDir,
    credentials_file: std::path::PathBuf,
    env: HashMap<String, String>,
}

impl Remote {
    /// The project `checkout` names, with its state under `locations`.
    pub fn new(checkout: &Checkout, locations: &Locations, env: &HashMap<String, String>) -> Self {
        let key = CredentialKey {
            server: checkout.config().server.clone(),
            project: checkout.config().project.clone(),
        };
        Self {
            state: StateDir::new(locations.state_dir(), &key),
            credentials_file: locations.credentials_file(),
            key,
            env: env.clone(),
        }
    }

    /// A client signed in with the stored token, its calls bounded by [`CALL_LIMIT`].
    ///
    /// # Errors
    /// [`RemoteError::Config`] when no token can be read without asking.
    pub fn client(&self) -> Result<Client, RemoteError> {
        let lookup = |name: &str| self.env.get(name).cloned();
        let credentials =
            Credentials::new(self.credentials_file.clone(), Arc::new(OsKeychain), &lookup)
                .map_err(|error| RemoteError::Config(sentence(&error.to_string())))?;
        let token = match credentials
            .token(&self.key, ReadMode::Hook)
            .map_err(|error| RemoteError::Config(sentence(&error.to_string())))?
        {
            TokenLookup::Found { token, .. } => token,
            TokenLookup::Missing | TokenLookup::StoreUnavailable => {
                return Err(RemoteError::Config(format!(
                    "No token is stored for the project '{}' on {}; sign in with dibs login.",
                    self.key.project, self.key.server
                )));
            }
        };
        let options = ClientOptions {
            connect_timeout: CALL_LIMIT,
            timeout: CALL_LIMIT,
            ..ClientOptions::default()
        };
        Ok(Client::new(self.key.server.clone(), token, options)?)
    }

    /// `GET /v1/me`, the project confirmed to be the checkout's, its settings and the member
    /// remembered.
    ///
    /// # Errors
    /// [`RemoteError`].
    pub async fn me(
        &self,
        client: &Client,
        checkout: &Checkout,
        now: OffsetDateTime,
    ) -> Result<Me, RemoteError> {
        let me = client.me().await?;
        checkout
            .verify_project(&me)
            .map_err(|mismatch| RemoteError::Config(sentence(&mismatch.to_string())))?;
        if let Err(error) = self.state.remember_settings(&me.settings, now) {
            tracing::debug!(error = %error, "the settings could not be cached");
        }
        if let Err(error) = self.state.remember_member(&me.member.handle) {
            tracing::debug!(error = %error, "the member could not be cached");
        }
        Ok(me)
    }

    /// The id of the session a host's session is registered as, registered first when this
    /// machine does not know it yet.
    ///
    /// # Errors
    /// [`RemoteError`].
    pub async fn session(
        &self,
        client: &Client,
        session: &SessionRef,
    ) -> Result<SessionId, RemoteError> {
        let label = session_label(session)?;
        if let Some(id) = self.state.session(&label) {
            return Ok(id);
        }
        let agent = AgentLabel::parse(session.host.as_str()).map_err(|_| {
            RemoteError::Config(format!(
                "The agent host '{}' has no name a session may report.",
                session.host
            ))
        })?;
        let host = host_name().map_err(|failure| RemoteError::Config(failure.message))?;
        let registered = client
            .register_session(&RegisterSession {
                host,
                label,
                agent,
                push: false,
            })
            .await?;
        if let Err(error) = self.state.remember_session(&registered) {
            tracing::debug!(error = %error, "the session could not be remembered");
        }
        Ok(registered.session_id)
    }
}

/// The label of a host's session: `<machine>/<the first 8 letters, digits, '-' or '_' of the
/// host's session id>`.
///
/// # Errors
/// [`RemoteError::Config`] when the machine's name cannot be used.
pub fn session_label(session: &SessionRef) -> Result<SessionLabel, RemoteError> {
    let machine = host_name().map_err(|failure| RemoteError::Config(failure.message))?;
    session_label_on(&machine, session)
}

/// The label of a host's session on the machine `host`, made as every label this binary
/// registers is made ([`label_on`]): the command line's own session reached through a sync is
/// then the session its commands registered, so its claims are the sync's own.
///
/// # Errors
/// [`RemoteError::Config`] when the machine's name cannot be used.
pub fn session_label_on(
    host: &HostName,
    session: &SessionRef,
) -> Result<SessionLabel, RemoteError> {
    let mut short: String = session
        .key
        .as_str()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(8)
        .collect();
    if short.is_empty() {
        short.push_str("session");
    }
    label_on(host, &short).ok_or_else(|| {
        RemoteError::Config("This machine's name cannot label a session.".to_owned())
    })
}

/// The checkout governing `cwd`, its configuration refused with a sentence.
///
/// # Errors
/// [`RemoteError::Config`] when the nearest `.dibs.toml` cannot be read or is not valid.
pub fn checkout_of(cwd: &Path) -> Result<Option<Checkout>, RemoteError> {
    Checkout::discover(cwd).map_err(|error| RemoteError::Config(sentence(&error.to_string())))
}

#[cfg(test)]
mod tests {
    use dibs_agent_host::{HostId, SessionKey};

    use super::*;

    fn session(key: &str) -> SessionRef {
        SessionRef {
            host: HostId::new("neutral"),
            key: SessionKey::parse(key).unwrap(),
        }
    }

    #[test]
    fn a_sync_names_the_command_lines_session_as_its_commands_do_however_long_the_machines_name() {
        let long =
            HostName::parse("sat12-bq155-4f09aa34-7c25-4994-a100-c10c44779306-DA09624ED7EE.local")
                .unwrap();
        let command_line = SessionRef {
            host: HostId::new(crate::context::AGENT),
            key: SessionKey::parse(crate::context::AGENT).unwrap(),
        };
        let synced = session_label_on(&long, &command_line).unwrap();
        assert_eq!(
            Some(synced.clone()),
            label_on(&long, crate::context::AGENT),
            "one session under one label, or a heartbeat keeps the claims of another"
        );
        assert!(synced.as_str().ends_with("/cli"), "{synced}");
        for host in [
            "laptop",
            "a-very-long-machine-name-of-a-build-runner-that-goes-on-and-on",
        ] {
            let host = HostName::parse(host).unwrap();
            assert_eq!(
                Some(session_label_on(&host, &command_line).unwrap()),
                label_on(&host, crate::context::AGENT)
            );
        }
        let hooked = session_label_on(&long, &session("3f2a9c-b1e7d0")).unwrap();
        assert!(hooked.as_str().ends_with("/3f2a9c-b"), "{hooked}");
        assert!(hooked.as_str().chars().count() <= 64);
    }

    #[test]
    fn a_label_takes_the_first_eight_safe_characters_of_the_session_id() {
        let label = session_label(&session("3f2a/9c-b1_e7d0ffff")).unwrap();
        assert!(label.as_str().ends_with("/3f2a9c-b"), "{label}");
        let odd = session_label(&session("::::")).unwrap();
        assert!(odd.as_str().ends_with("/session"), "{odd}");
    }
}
