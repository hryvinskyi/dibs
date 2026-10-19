//! What every command starts from: the environment as the process received it, the current
//! directory, and, for commands that talk to the server, a connection to the project the checkout
//! or the stored credential names.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_client::api::{Me, RegisterSession, Session};
use dibs_client::checkout::Checkout;
use dibs_client::credentials::{
    CredentialKey, Credentials, OsKeychain, ReadMode, TokenLookup, TokenSource,
};
use dibs_client::locations::Locations;
use dibs_client::state::StateDir;
use dibs_client::{Client, ClientError, ClientOptions};
use dibs_domain::ids::{ItemId, SessionId};
use dibs_domain::item::ItemKey;
use dibs_domain::message::SessionLabel;
use dibs_domain::session::{AgentLabel, HostName};

use crate::failure::Failure;
use crate::output::Printer;

/// The agent label of the sessions the command line registers.
pub const AGENT: &str = "cli";

/// The longest session label.
const MAX_LABEL: usize = 64;

/// The environment, the current directory and the output form.
#[derive(Clone)]
pub struct Context {
    env: HashMap<String, String>,
    /// The current directory.
    pub cwd: PathBuf,
    /// The output form.
    pub printer: Printer,
}

/// A signed-in connection to one project.
pub struct Connection {
    /// The client.
    pub client: Client,
    /// Who the token signs in as.
    pub me: Me,
    /// The checkout the command runs in, when there is one.
    pub checkout: Option<Checkout>,
    /// The project and server.
    pub key: CredentialKey,
    /// Where the token came from.
    pub source: TokenSource,
    /// What the client remembers for this project.
    pub state: StateDir,
}

impl Context {
    /// A context over this environment and directory.
    pub fn new(env: HashMap<String, String>, cwd: PathBuf, printer: Printer) -> Self {
        Self { env, cwd, printer }
    }

    /// A variable of the environment.
    pub fn var(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    /// The client's directories.
    ///
    /// # Errors
    /// A configuration [`Failure`] when the machine names none.
    pub fn locations(&self) -> Result<Locations, Failure> {
        Locations::from_env(&|name| self.var(name)).map_err(|error| {
            Failure::config("no_directory", crate::failure::sentence(&error.to_string()))
        })
    }

    /// The member's credentials on this machine.
    ///
    /// # Errors
    /// A configuration [`Failure`].
    pub fn credentials(&self) -> Result<Credentials, Failure> {
        let locations = self.locations()?;
        Credentials::new(
            locations.credentials_file(),
            Arc::new(OsKeychain),
            &|name| self.var(name),
        )
        .map_err(credential_failure)
    }

    /// The checkout whose `.dibs.toml` is nearest to the current directory.
    ///
    /// # Errors
    /// A configuration [`Failure`] when that file cannot be read or is not valid.
    pub fn checkout(&self) -> Result<Option<Checkout>, Failure> {
        Checkout::discover(&self.cwd).map_err(|error| {
            Failure::config(
                "invalid_checkout",
                crate::failure::sentence(&error.to_string()),
            )
        })
    }

    /// Signs in to the project the checkout names, or the one stored credential when the command
    /// runs outside any checkout, and refuses a token of another project than the checkout's.
    ///
    /// # Errors
    /// A [`Failure`] of the configuration, the credentials, or the first call.
    pub async fn connect(&self) -> Result<Connection, Failure> {
        let checkout = self.checkout()?;
        let credentials = self.credentials()?;
        let key = match &checkout {
            Some(checkout) => CredentialKey {
                server: checkout.config().server.clone(),
                project: checkout.config().project.clone(),
            },
            None => credentials.sole_key().map_err(credential_failure)?.ok_or_else(|| {
                Failure::config(
                    "no_checkout",
                    "No .dibs.toml was found here or above in this repository, and no single sign-in is stored; run the command inside a checkout, or sign in with dibs login --server <url>.",
                )
            })?,
        };
        let (token, source) = match credentials
            .token(&key, ReadMode::Interactive)
            .map_err(credential_failure)?
        {
            TokenLookup::Found { token, source } => (token, source),
            TokenLookup::Missing | TokenLookup::StoreUnavailable => {
                return Err(Failure::config(
                    "no_token",
                    format!(
                        "No token is stored for the project '{}' on {}; sign in with dibs login.",
                        key.project, key.server
                    ),
                ));
            }
        };
        let client = Client::new(key.server.clone(), token, ClientOptions::default())?;
        let state = StateDir::new(self.locations()?.state_dir(), &key);
        let me = match client.me().await {
            Ok(me) => me,
            Err(error) => return Err(record(&state, error)),
        };
        let _ = state.clear_unreachable();
        if let Some(checkout) = &checkout {
            checkout.verify_project(&me).map_err(|mismatch| {
                Failure::config(
                    "project_mismatch",
                    crate::failure::sentence(&mismatch.to_string()),
                )
            })?;
        }
        if let Err(error) = state.remember_settings(&me.settings, OffsetDateTime::now_utc()) {
            tracing::debug!(error = %error, "the settings could not be cached");
        }
        Ok(Connection {
            client,
            me,
            checkout,
            key,
            source,
            state,
        })
    }
}

impl Connection {
    /// The failure a client error stands for; an unreachable server is remembered as such.
    pub fn failed(&self, error: ClientError) -> Failure {
        record(&self.state, error)
    }

    /// The label of this machine's command-line session: `<host>/cli`.
    ///
    /// # Errors
    /// A [`Failure`] when no label can be made of the machine's name.
    pub fn label(&self) -> Result<SessionLabel, Failure> {
        label_named(AGENT)
    }

    /// Registers this machine's command-line session (registering again keeps its id) and
    /// remembers it.
    ///
    /// # Errors
    /// The [`Failure`] of the call.
    pub async fn session(&self) -> Result<SessionId, Failure> {
        let session = self.register(AGENT, AGENT).await?;
        if let Err(error) = self.state.remember_session(&session) {
            tracing::debug!(error = %error, "the session could not be remembered");
        }
        Ok(session.session_id)
    }

    /// Registers a session of the given kind of agent, labelled `<host>/<name>` (registering the
    /// same label again keeps its id). The session cannot be woken while idle.
    ///
    /// # Errors
    /// The [`Failure`] of the call, or a configuration one when the agent kind or the machine's
    /// name cannot make a valid label.
    pub async fn register(&self, agent: &str, name: &str) -> Result<Session, Failure> {
        let request = RegisterSession {
            host: host_name()?,
            label: label_named(name)?,
            agent: AgentLabel::parse(agent).map_err(|_| unnamed_machine())?,
            push: false,
        };
        self.client
            .register_session(&request)
            .await
            .map_err(|error| self.failed(error))
    }

    /// The local id of the item a key names.
    ///
    /// # Errors
    /// The [`Failure`] of the lookup.
    pub async fn resolve(&self, key: &ItemKey) -> Result<ItemId, Failure> {
        self.client
            .resolve(key)
            .await
            .map_err(|error| self.failed(error))
    }
}

/// The machine's name as a session reports it, `unknown-host` when it has none a session may
/// report.
///
/// # Errors
/// A [`Failure`] when not even that can be made into a host name.
pub fn host_name() -> Result<HostName, Failure> {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .take(255)
        .collect();
    HostName::parse(&cleaned)
        .or_else(|_| HostName::parse("unknown-host"))
        .map_err(|_| unnamed_machine())
}

/// The label `<host>/<name>`, the host cut short enough for the whole to be a valid label.
fn label_named(name: &str) -> Result<SessionLabel, Failure> {
    let room = MAX_LABEL.saturating_sub(name.chars().count() + 1);
    let host: String = host_name()?.as_str().chars().take(room).collect();
    SessionLabel::parse(&format!("{host}/{name}")).map_err(|_| unnamed_machine())
}

fn unnamed_machine() -> Failure {
    Failure::config(
        "no_host_name",
        "This machine's name cannot be used to label a session.",
    )
}

fn record(state: &StateDir, error: ClientError) -> Failure {
    if error.is_unavailable()
        && let Err(io) = state.mark_unreachable(OffsetDateTime::now_utc())
    {
        tracing::debug!(error = %io, "the unreachable mark could not be written");
    }
    Failure::from(error)
}

fn credential_failure(error: dibs_client::credentials::CredentialError) -> Failure {
    Failure::config("credentials", crate::failure::sentence(&error.to_string()))
}
