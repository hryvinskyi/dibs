//! A member's tokens on this machine, one per server and project: one person may be a member of
//! several projects on one server.
//!
//! A token is found, in order, in `DIBS_TOKEN`, then in the credentials file, which holds either
//! the token itself or a note that it lives in the operating system's credential store. The file
//! is private to its user ([`crate::private_file`]); the OS store is used only when the person
//! asked for it at sign-in. A read of the OS store from a hook never waits on a prompt: it has a
//! deadline, and a read that misses it finds no token.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use dibs_domain::project::ProjectSlug;

use crate::locations::Lookup;
use crate::private_file::{self, Principal, PrivateFileError};
use crate::server_url::ServerUrl;
use crate::token::Token;

/// The variable that hands the client a token directly.
pub const TOKEN_VAR: &str = "DIBS_TOKEN";

/// How long a hook waits for the OS credential store.
pub const HOOK_DEADLINE: Duration = Duration::from_secs(1);

/// The service name tokens are filed under in the OS credential store.
pub const KEYCHAIN_SERVICE: &str = "dibs";

/// Which token: the one of a project on a server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CredentialKey {
    /// The server.
    pub server: ServerUrl,
    /// The project.
    pub project: ProjectSlug,
}

impl CredentialKey {
    /// The account a token is filed under in the OS credential store.
    pub fn account(&self) -> String {
        format!("{}@{}", self.project, self.server)
    }
}

/// Where a token was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// The `DIBS_TOKEN` variable.
    Environment,
    /// The credentials file.
    File(PathBuf),
    /// The OS credential store.
    Keychain,
}

/// How a token is looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// A person at a terminal: the OS store may ask them, and the read waits.
    Interactive,
    /// A hook: the OS store is read with [`HOOK_DEADLINE`], and a prompt or a slow store finds no
    /// token.
    Hook,
}

/// What a lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenLookup {
    /// A token.
    Found {
        /// The token.
        token: Token,
        /// Where.
        source: TokenSource,
    },
    /// No token is stored for the key.
    Missing,
    /// The token is in the OS store, which did not answer in time or refused.
    StoreUnavailable,
}

/// Why the credentials could not be read or written.
#[derive(Debug, Error)]
pub enum CredentialError {
    /// The credentials file is exposed or cannot be read or written.
    #[error(transparent)]
    File(#[from] PrivateFileError),
    /// The credentials file is not valid.
    #[error("{path} is not valid: {reason}")]
    Invalid {
        /// The file.
        path: PathBuf,
        /// The rule broken.
        reason: String,
    },
    /// `DIBS_TOKEN` holds no token.
    #[error("DIBS_TOKEN is not valid: {0}")]
    Environment(crate::token::TokenError),
    /// The OS credential store failed.
    #[error("the operating system's credential store could not be used: {0}")]
    Store(String),
}

/// The OS credential store, as the credentials need it.
pub trait SecretStore: Send + Sync {
    /// The token filed for the key, `None` when there is none.
    ///
    /// # Errors
    /// The store's failure, in words.
    fn read(&self, key: &CredentialKey) -> Result<Option<Token>, String>;

    /// Files the token for the key, replacing one filed before.
    ///
    /// # Errors
    /// The store's failure, in words.
    fn write(&self, key: &CredentialKey, token: &Token) -> Result<(), String>;
}

/// The credentials file as written.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default, rename = "credential", skip_serializing_if = "Vec::is_empty")]
    credentials: Vec<Entry>,
}

/// One project's credential: the token itself, or `keychain = true` when the OS store holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    server: ServerUrl,
    project: ProjectSlug,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<Token>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    keychain: bool,
}

impl Entry {
    fn key(&self) -> CredentialKey {
        CredentialKey {
            server: self.server.clone(),
            project: self.project.clone(),
        }
    }
}

/// A member's credentials on this machine.
pub struct Credentials {
    path: PathBuf,
    principal: Principal,
    store: Arc<dyn SecretStore>,
    environment: Option<Token>,
    deadline: Duration,
}

impl Credentials {
    /// The credentials kept in the file at `path` and in `store`, with `DIBS_TOKEN` read through
    /// `lookup`.
    ///
    /// # Errors
    /// [`CredentialError::Environment`] when `DIBS_TOKEN` is set and holds no token.
    pub fn new(
        path: PathBuf,
        store: Arc<dyn SecretStore>,
        lookup: Lookup<'_>,
    ) -> Result<Self, CredentialError> {
        let environment = lookup(TOKEN_VAR)
            .filter(|value| !value.trim().is_empty())
            .map(|value| Token::parse(&value))
            .transpose()
            .map_err(CredentialError::Environment)?;
        Ok(Self {
            path,
            principal: Principal::from_env(lookup),
            store,
            environment,
            deadline: HOOK_DEADLINE,
        })
    }

    /// The same credentials with another deadline for hook reads of the OS store.
    #[must_use]
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// The credentials file.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// The token for a project on a server.
    ///
    /// # Errors
    /// [`CredentialError`] when the file is exposed, unreadable or invalid, or, in
    /// [`ReadMode::Interactive`], when the OS store fails.
    pub fn token(
        &self,
        key: &CredentialKey,
        mode: ReadMode,
    ) -> Result<TokenLookup, CredentialError> {
        if let Some(token) = &self.environment {
            return Ok(TokenLookup::Found {
                token: token.clone(),
                source: TokenSource::Environment,
            });
        }
        let document = self.load()?;
        let Some(entry) = document
            .credentials
            .iter()
            .find(|entry| entry.key() == *key)
        else {
            return Ok(TokenLookup::Missing);
        };
        if let Some(token) = &entry.token {
            return Ok(TokenLookup::Found {
                token: token.clone(),
                source: TokenSource::File(self.path.clone()),
            });
        }
        let read = match mode {
            ReadMode::Interactive => self.store.read(key).map_err(CredentialError::Store)?,
            ReadMode::Hook => match read_within(self.store.clone(), key.clone(), self.deadline) {
                Some(Ok(token)) => token,
                Some(Err(reason)) => {
                    tracing::debug!(reason = %reason, "the credential store refused a read");
                    return Ok(TokenLookup::StoreUnavailable);
                }
                None => return Ok(TokenLookup::StoreUnavailable),
            },
        };
        Ok(
            read.map_or(TokenLookup::Missing, |token| TokenLookup::Found {
                token,
                source: TokenSource::Keychain,
            }),
        )
    }

    /// The key of the one credential the file holds, when it holds exactly one: what a command run
    /// outside any checkout uses.
    ///
    /// # Errors
    /// [`CredentialError`] when the file is exposed, unreadable or invalid.
    pub fn sole_key(&self) -> Result<Option<CredentialKey>, CredentialError> {
        let document = self.load()?;
        Ok(match document.credentials.as_slice() {
            [entry] => Some(entry.key()),
            _ => None,
        })
    }

    /// Stores the token in the credentials file, replacing what was stored for the key.
    ///
    /// # Errors
    /// [`CredentialError`].
    pub fn save_in_file(&self, key: &CredentialKey, token: &Token) -> Result<(), CredentialError> {
        self.save(Entry {
            server: key.server.clone(),
            project: key.project.clone(),
            token: Some(token.clone()),
            keychain: false,
        })
    }

    /// Stores the token in the OS credential store, and in the file a note that it is there.
    ///
    /// # Errors
    /// [`CredentialError`].
    pub fn save_in_store(&self, key: &CredentialKey, token: &Token) -> Result<(), CredentialError> {
        self.store
            .write(key, token)
            .map_err(CredentialError::Store)?;
        self.save(Entry {
            server: key.server.clone(),
            project: key.project.clone(),
            token: None,
            keychain: true,
        })
    }

    fn save(&self, entry: Entry) -> Result<(), CredentialError> {
        let mut document = self.load()?;
        let key = entry.key();
        document.credentials.retain(|known| known.key() != key);
        document.credentials.push(entry);
        document.credentials.sort_by_key(Entry::key);
        let text = toml::to_string(&document).map_err(|error| CredentialError::Invalid {
            path: self.path.clone(),
            reason: error.to_string(),
        })?;
        private_file::write(&self.path, &text, &self.principal)?;
        Ok(())
    }

    fn load(&self) -> Result<Document, CredentialError> {
        let Some(text) = private_file::read(&self.path, &self.principal)? else {
            return Ok(Document::default());
        };
        let invalid = |reason: String| CredentialError::Invalid {
            path: self.path.clone(),
            reason,
        };
        let document: Document =
            toml::from_str(&text).map_err(|error| invalid(error.message().to_owned()))?;
        for entry in &document.credentials {
            if entry.token.is_some() == entry.keychain {
                return Err(invalid(format!(
                    "the credential for {} holds either a token or keychain = true",
                    entry.key().account()
                )));
            }
        }
        Ok(document)
    }
}

/// Reads the store on a thread of its own and waits at most `deadline` for it. `None` when the
/// read did not finish in time; the thread is left to end on its own.
fn read_within(
    store: Arc<dyn SecretStore>,
    key: CredentialKey,
    deadline: Duration,
) -> Option<Result<Option<Token>, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(store.read(&key));
    });
    receiver.recv_timeout(deadline).ok()
}

/// The operating system's credential store: the macOS Keychain, the Windows Credential Manager,
/// the Secret Service on Linux.
#[derive(Debug, Default)]
pub struct OsKeychain;

impl OsKeychain {
    fn entry(key: &CredentialKey) -> Result<keyring_core::Entry, String> {
        let store = native_store()?;
        store
            .build(KEYCHAIN_SERVICE, &key.account(), None)
            .map_err(|error| error.to_string())
    }
}

impl SecretStore for OsKeychain {
    fn read(&self, key: &CredentialKey) -> Result<Option<Token>, String> {
        match Self::entry(key)?.get_password() {
            Ok(text) => Token::parse(&text)
                .map(Some)
                .map_err(|error| error.to_string()),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn write(&self, key: &CredentialKey, token: &Token) -> Result<(), String> {
        Self::entry(key)?
            .set_password(token.expose())
            .map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "macos")]
fn native_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    apple_native_keyring_store::keychain::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn native_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    windows_native_keyring_store::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn native_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    dbus_secret_service_keyring_store::Store::new()
        .map(|store| store as Arc<keyring_core::CredentialStore>)
        .map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn native_store() -> Result<Arc<keyring_core::CredentialStore>, String> {
    Err("this operating system has no credential store dibs can use".to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// A store in memory that can be made slow, and counts its reads.
    #[derive(Default)]
    struct MemoryStore {
        tokens: Mutex<HashMap<String, Token>>,
        delay: Option<Duration>,
        reads: Mutex<u32>,
    }

    impl SecretStore for MemoryStore {
        fn read(&self, key: &CredentialKey) -> Result<Option<Token>, String> {
            *self.reads.lock().unwrap() += 1;
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            Ok(self.tokens.lock().unwrap().get(&key.account()).cloned())
        }

        fn write(&self, key: &CredentialKey, token: &Token) -> Result<(), String> {
            self.tokens
                .lock()
                .unwrap()
                .insert(key.account(), token.clone());
            Ok(())
        }
    }

    fn key(server: &str, project: &str) -> CredentialKey {
        CredentialKey {
            server: ServerUrl::parse(server).unwrap(),
            project: ProjectSlug::parse(project).unwrap(),
        }
    }

    fn token(text: &str) -> Token {
        Token::parse(text).unwrap()
    }

    fn credentials(
        dir: &std::path::Path,
        store: Arc<MemoryStore>,
        env: &[(&str, &str)],
    ) -> Credentials {
        let env: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let mut lookup_env = env.clone();
        for var in ["USERNAME", "USERDOMAIN"] {
            if let Ok(value) = std::env::var(var) {
                lookup_env.entry(var.to_owned()).or_insert(value);
            }
        }
        let lookup = move |var: &str| lookup_env.get(var).cloned();
        Credentials::new(dir.join("credentials.toml"), store, &lookup).unwrap()
    }

    #[test]
    fn one_server_holds_a_token_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let creds = credentials(dir.path(), Arc::default(), &[]);
        let acme = key("https://dibs.example.com", "acme");
        let globex = key("https://dibs.example.com/", "globex");
        let other = key("https://other.example.com", "acme");
        creds.save_in_file(&acme, &token("dibs_acme")).unwrap();
        creds.save_in_file(&globex, &token("dibs_globex")).unwrap();
        let found = |key: &CredentialKey| match creds.token(key, ReadMode::Interactive).unwrap() {
            TokenLookup::Found { token, .. } => Some(token.expose().to_owned()),
            _ => None,
        };
        assert_eq!(found(&acme).as_deref(), Some("dibs_acme"));
        assert_eq!(found(&globex).as_deref(), Some("dibs_globex"));
        assert_eq!(found(&other), None);
        creds.save_in_file(&acme, &token("dibs_acme_2")).unwrap();
        assert_eq!(found(&acme).as_deref(), Some("dibs_acme_2"));
        assert_eq!(creds.sole_key().unwrap(), None);
    }

    #[test]
    fn the_sole_credential_is_known_without_a_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let creds = credentials(dir.path(), Arc::default(), &[]);
        assert_eq!(creds.sole_key().unwrap(), None);
        let acme = key("https://dibs.example.com", "acme");
        creds.save_in_file(&acme, &token("dibs_acme")).unwrap();
        assert_eq!(creds.sole_key().unwrap(), Some(acme));
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let acme = key("https://dibs.example.com", "acme");
        credentials(dir.path(), Arc::default(), &[])
            .save_in_file(&acme, &token("dibs_file"))
            .unwrap();
        let creds = credentials(dir.path(), Arc::default(), &[(TOKEN_VAR, "dibs_env\n")]);
        assert_eq!(
            creds.token(&acme, ReadMode::Hook).unwrap(),
            TokenLookup::Found {
                token: token("dibs_env"),
                source: TokenSource::Environment
            }
        );
        let lookup = |var: &str| (var == TOKEN_VAR).then(|| "not a token".to_owned());
        assert!(matches!(
            Credentials::new(
                dir.path().join("c.toml"),
                Arc::new(MemoryStore::default()),
                &lookup
            ),
            Err(CredentialError::Environment(_))
        ));
    }

    #[test]
    fn a_keychain_credential_is_read_from_the_store_and_never_written_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::default());
        let creds = credentials(dir.path(), store.clone(), &[]);
        let acme = key("https://dibs.example.com", "acme");
        creds.save_in_store(&acme, &token("dibs_secret")).unwrap();
        let text = std::fs::read_to_string(creds.path()).unwrap();
        assert!(!text.contains("dibs_secret"), "{text}");
        assert!(text.contains("keychain = true"), "{text}");
        assert_eq!(
            creds.token(&acme, ReadMode::Hook).unwrap(),
            TokenLookup::Found {
                token: token("dibs_secret"),
                source: TokenSource::Keychain
            }
        );
    }

    #[test]
    fn a_file_credential_never_touches_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::default());
        let creds = credentials(dir.path(), store.clone(), &[]);
        let acme = key("https://dibs.example.com", "acme");
        creds.save_in_file(&acme, &token("dibs_acme")).unwrap();
        creds.token(&acme, ReadMode::Hook).unwrap();
        creds.token(&acme, ReadMode::Interactive).unwrap();
        assert_eq!(*store.reads.lock().unwrap(), 0);
        assert!(store.tokens.lock().unwrap().is_empty());
    }

    #[test]
    fn a_slow_store_finds_no_token_in_a_hook() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore {
            delay: Some(Duration::from_millis(500)),
            ..MemoryStore::default()
        });
        let acme = key("https://dibs.example.com", "acme");
        let creds = credentials(dir.path(), store, &[]).with_deadline(Duration::from_millis(50));
        creds.save_in_store(&acme, &token("dibs_secret")).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(
            creds.token(&acme, ReadMode::Hook).unwrap(),
            TokenLookup::StoreUnavailable
        );
        assert!(started.elapsed() < Duration::from_millis(400));
        assert!(matches!(
            creds.token(&acme, ReadMode::Interactive).unwrap(),
            TokenLookup::Found { .. }
        ));
    }

    #[test]
    fn the_default_hook_deadline_is_one_second() {
        assert_eq!(HOOK_DEADLINE, Duration::from_secs(1));
    }

    #[test]
    fn an_entry_with_both_or_neither_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let creds = credentials(dir.path(), Arc::default(), &[]);
        let principal = Principal::from_env(&|var| std::env::var(var).ok());
        for text in [
            "[[credential]]\nserver = \"https://x.example.com\"\nproject = \"acme\"\n",
            "[[credential]]\nserver = \"https://x.example.com\"\nproject = \"acme\"\ntoken = \"t\"\nkeychain = true\n",
            "[[credential]]\nserver = \"http://x.example.com\"\nproject = \"acme\"\ntoken = \"t\"\n",
        ] {
            private_file::write(creds.path(), text, &principal).unwrap();
            let error = creds.sole_key().unwrap_err();
            assert!(
                matches!(error, CredentialError::Invalid { .. }),
                "{text}: {error}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_exposed_file_is_refused_before_it_is_read() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let creds = credentials(dir.path(), Arc::default(), &[]);
        let acme = key("https://dibs.example.com", "acme");
        creds.save_in_file(&acme, &token("dibs_acme")).unwrap();
        std::fs::set_permissions(creds.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = creds.token(&acme, ReadMode::Interactive).unwrap_err();
        assert!(matches!(
            error,
            CredentialError::File(PrivateFileError::Exposed { .. })
        ));
    }
}
