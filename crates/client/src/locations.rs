//! Where the client keeps its files on this machine: the configuration directory, holding the
//! credentials file, and the state directory, holding sessions and caches.
//!
//! Every lookup of the environment goes through an injected function, so the rules are a pure
//! function of what it answers: the binary passes the process environment, tests pass a map.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// A lookup of an environment variable: its value, or `None` when it is not set.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The variable that moves the configuration directory.
pub const CONFIG_DIR_VAR: &str = "DIBS_CONFIG_DIR";

/// The variable that moves the state directory.
pub const STATE_DIR_VAR: &str = "DIBS_STATE_DIR";

/// The client's directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locations {
    /// The configuration directory.
    pub config_dir: PathBuf,
    /// The state directory.
    pub state_dir: PathBuf,
}

/// The machine gives no directory to put the client's files in.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("no home directory is known on this machine; set {var} to a directory dibs may write to")]
pub struct NoDirectory {
    /// The variable that would name one.
    pub var: &'static str,
}

impl Locations {
    /// The directories, from the environment and the platform's conventions:
    ///
    /// - configuration: `DIBS_CONFIG_DIR`; else on Windows `%APPDATA%\dibs`; else
    ///   `$XDG_CONFIG_HOME/dibs`, else `~/.config/dibs` (macOS included);
    /// - state: `DIBS_STATE_DIR`; else `$XDG_STATE_HOME/dibs` or `~/.local/state/dibs` on Linux,
    ///   `~/Library/Application Support/dibs` on macOS, `%LOCALAPPDATA%\dibs` on Windows.
    ///
    /// An empty variable counts as not set.
    ///
    /// # Errors
    /// [`NoDirectory`] when neither a variable nor the platform names a directory.
    pub fn from_env(lookup: Lookup<'_>) -> Result<Self, NoDirectory> {
        let get = |var: &str| lookup(var).filter(|value| !value.trim().is_empty());
        let home = || get("HOME").map(PathBuf::from).or_else(dirs::home_dir);
        let config_dir = match get(CONFIG_DIR_VAR) {
            Some(dir) => PathBuf::from(dir),
            None => platform_config_dir(&get, &home).ok_or(NoDirectory {
                var: CONFIG_DIR_VAR,
            })?,
        };
        let state_dir = match get(STATE_DIR_VAR) {
            Some(dir) => PathBuf::from(dir),
            None => platform_state_dir(&get, &home).ok_or(NoDirectory { var: STATE_DIR_VAR })?,
        };
        Ok(Self {
            config_dir,
            state_dir,
        })
    }

    /// The credentials file.
    pub fn credentials_file(&self) -> PathBuf {
        self.config_dir.join("credentials.toml")
    }

    /// The state directory.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }
}

#[cfg(windows)]
fn platform_config_dir(
    _get: &dyn Fn(&str) -> Option<String>,
    _home: &dyn Fn() -> Option<PathBuf>,
) -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("dibs"))
}

#[cfg(not(windows))]
fn platform_config_dir(
    get: &dyn Fn(&str) -> Option<String>,
    home: &dyn Fn() -> Option<PathBuf>,
) -> Option<PathBuf> {
    get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".config")))
        .map(|dir| dir.join("dibs"))
}

#[cfg(windows)]
fn platform_state_dir(
    _get: &dyn Fn(&str) -> Option<String>,
    _home: &dyn Fn() -> Option<PathBuf>,
) -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("dibs"))
}

#[cfg(target_os = "macos")]
fn platform_state_dir(
    _get: &dyn Fn(&str) -> Option<String>,
    home: &dyn Fn() -> Option<PathBuf>,
) -> Option<PathBuf> {
    home().map(|home| home.join("Library/Application Support/dibs"))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn platform_state_dir(
    get: &dyn Fn(&str) -> Option<String>,
    home: &dyn Fn() -> Option<PathBuf>,
) -> Option<PathBuf> {
    get("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".local/state")))
        .map(|dir| dir.join("dibs"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |var| map.get(var).cloned()
    }

    #[test]
    fn the_variables_win_over_the_platform() {
        let lookup = env(&[(CONFIG_DIR_VAR, "/tmp/c"), (STATE_DIR_VAR, "/tmp/s")]);
        let locations = Locations::from_env(&lookup).unwrap();
        assert_eq!(locations.config_dir, PathBuf::from("/tmp/c"));
        assert_eq!(locations.state_dir, PathBuf::from("/tmp/s"));
        assert_eq!(
            locations.credentials_file(),
            PathBuf::from("/tmp/c/credentials.toml")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_configuration_lives_under_dot_config_on_unix() {
        let lookup = env(&[("HOME", "/home/alice"), (CONFIG_DIR_VAR, "  ")]);
        let locations = Locations::from_env(&lookup).unwrap();
        assert_eq!(
            locations.credentials_file(),
            PathBuf::from("/home/alice/.config/dibs/credentials.toml")
        );
        let xdg = env(&[("HOME", "/home/alice"), ("XDG_CONFIG_HOME", "/xdg")]);
        assert_eq!(
            Locations::from_env(&xdg).unwrap().config_dir,
            PathBuf::from("/xdg/dibs")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn state_lives_in_application_support_on_macos() {
        let lookup = env(&[("HOME", "/Users/alice")]);
        assert_eq!(
            Locations::from_env(&lookup).unwrap().state_dir,
            PathBuf::from("/Users/alice/Library/Application Support/dibs")
        );
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    #[test]
    fn state_lives_under_the_xdg_state_directory_on_linux() {
        let lookup = env(&[("HOME", "/home/alice")]);
        assert_eq!(
            Locations::from_env(&lookup).unwrap().state_dir,
            PathBuf::from("/home/alice/.local/state/dibs")
        );
        let xdg = env(&[("HOME", "/home/alice"), ("XDG_STATE_HOME", "/xdg-state")]);
        assert_eq!(
            Locations::from_env(&xdg).unwrap().state_dir,
            PathBuf::from("/xdg-state/dibs")
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_configuration_lives_under_appdata_on_windows() {
        let locations = Locations::from_env(&env(&[])).unwrap();
        let appdata = dirs::config_dir().unwrap();
        assert_eq!(locations.config_dir, appdata.join("dibs"));
        assert_eq!(
            locations.state_dir,
            dirs::data_local_dir().unwrap().join("dibs")
        );
    }
}
