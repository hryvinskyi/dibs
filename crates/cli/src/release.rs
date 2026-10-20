//! What this binary is as a release: its version, the platform whose archive it came from, the
//! release key it trusts, and where releases are downloaded from.
//!
//! The key is the minisign public key in `keys/release.pub`, compiled in. While that file holds
//! the placeholder instead of a key, every update refuses.

use std::collections::HashMap;
use std::path::PathBuf;

use semver::Version;

use dibs_client::update::release::DEFAULT_RELEASES;
use dibs_client::update::{Platform, TrustedKey};

use crate::failure::Failure;

/// The version of this binary.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The target triple this binary was built for.
pub const TARGET: &str = env!("DIBS_TARGET");

/// The variable that names where releases are downloaded from, laid out as the releases are.
pub const RELEASES_VAR: &str = "DIBS_RELEASE_URL";

/// The release key, as `keys/release.pub` held it when this binary was built.
const RELEASE_KEY: &str = include_str!("../../../keys/release.pub");

/// What the background sync does with a newer release the server recommends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundUpdate {
    /// Installs it, as `dibs self-update` would.
    Install,
    /// Only remembers it, so the brief tells the person to run `dibs self-update`.
    NotifyOnly,
}

/// On Windows the detached sync keeps the output pipe of the hook that started it open for as long
/// as it runs, so a download there would hold the host waiting on that hook; the background only
/// tells, and `dibs self-update` in a terminal installs.
#[cfg(windows)]
pub const BACKGROUND_UPDATE: BackgroundUpdate = BackgroundUpdate::NotifyOnly;

/// Elsewhere the detached sync holds nothing of the hook's, and installs in the background.
#[cfg(not(windows))]
pub const BACKGROUND_UPDATE: BackgroundUpdate = BackgroundUpdate::Install;

/// The version of this binary.
///
/// # Errors
/// A configuration [`Failure`] when the build names no release version.
pub fn installed() -> Result<Version, Failure> {
    Version::parse(VERSION).map_err(|_| {
        Failure::config(
            "no_version",
            "This build of dibs names no release version, so it cannot update itself.",
        )
    })
}

/// The release key this binary trusts.
pub fn trusted_key() -> TrustedKey {
    TrustedKey::parse(RELEASE_KEY)
}

/// The platform this binary was built for.
pub fn platform() -> Platform {
    Platform::new(TARGET)
}

/// Where releases are downloaded from: `DIBS_RELEASE_URL`, else the public repository's releases.
pub fn releases_url(env: &HashMap<String, String>) -> String {
    env.get(RELEASES_VAR)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| DEFAULT_RELEASES.to_owned())
}

/// The file of the running binary, with its links resolved, which an update replaces.
///
/// # Errors
/// A configuration [`Failure`] when it cannot be found.
pub fn running_binary() -> Result<PathBuf, Failure> {
    std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| {
            tracing::debug!(error = %error, "the running binary could not be found");
            Failure::config(
                "no_binary",
                "dibs could not find the file it runs from, so it cannot update itself.",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_background_installs_everywhere_but_windows_where_it_only_tells() {
        let expected = if cfg!(windows) {
            BackgroundUpdate::NotifyOnly
        } else {
            BackgroundUpdate::Install
        };
        assert_eq!(BACKGROUND_UPDATE, expected);
    }

    #[test]
    fn this_build_knows_its_version_and_platform() {
        assert_eq!(installed().unwrap().to_string(), VERSION);
        assert!(!TARGET.is_empty());
        assert!(platform().archive(&installed().unwrap()).contains(TARGET));
    }

    #[test]
    fn releases_come_from_the_public_repository_unless_the_variable_names_another_place() {
        let mut env = HashMap::new();
        assert_eq!(releases_url(&env), DEFAULT_RELEASES);
        env.insert(RELEASES_VAR.to_owned(), " ".to_owned());
        assert_eq!(releases_url(&env), DEFAULT_RELEASES);
        env.insert(RELEASES_VAR.to_owned(), "file:///srv/releases".to_owned());
        assert_eq!(releases_url(&env), "file:///srv/releases");
    }
}
