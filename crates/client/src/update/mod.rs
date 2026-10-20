//! Replacing the installed `dibs` with a newer release, nothing of which is written before it is
//! proven to come from the project's release key.
//!
//! The order is fixed:
//! 1. a build without a usable release key refuses, before anything is downloaded;
//! 2. a release older than the installed `dibs` is refused, one of the same version is reported as
//!    current, and a directory the person cannot write to refuses with the install hint;
//! 3. the release's `SHA256SUMS` and its minisign signature `SHA256SUMS.minisig` are downloaded,
//!    and the signature must verify against the trusted key;
//! 4. the archive for this platform must be listed in that file, and its digest must match;
//! 5. the binary is taken out of the archive into memory and written beside the installed one,
//!    then put in its place in one step.
//!
//! The version installed is read from the archive's name in the signed file, so a release served
//! under another name than its own is still judged by what its key signed.

pub mod archive;
pub mod install;
pub mod key;
pub mod release;
pub mod sums;

use std::path::PathBuf;

use semver::Version;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub use archive::Platform;
pub use key::TrustedKey;
pub use release::{ReleaseRef, ReleaseSource, releases};

/// Where a release is installed by hand, for the sentences that send the person there.
pub const INSTALL_URL: &str = "https://github.com/hryvinskyi/dibs#install";

/// The checksum file every release carries.
pub const SUMS: &str = "SHA256SUMS";

/// The minisign signature of [`SUMS`].
pub const SIGNATURE: &str = "SHA256SUMS.minisig";

/// The largest checksum or signature file read.
pub const SMALL_LIMIT: usize = 64 * 1024;

/// The largest archive downloaded, and the largest binary taken out of one.
pub const ARCHIVE_LIMIT: usize = 256 * 1024 * 1024;

/// Which release to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// The newest release the source publishes.
    Latest,
    /// Exactly this release.
    Exactly(Version),
}

/// What an update is asked to do.
#[derive(Debug, Clone)]
pub struct UpdateRequest {
    /// The version installed now.
    pub installed: Version,
    /// The platform whose archive is installed.
    pub platform: Platform,
    /// The release wanted.
    pub wanted: Wanted,
    /// The installed binary, which the new one replaces.
    pub binary: PathBuf,
}

/// What an update did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Updated {
    /// The installed `dibs` is the release wanted; nothing was changed.
    Current(Version),
    /// The binary was replaced.
    Installed {
        /// The version replaced.
        from: Version,
        /// The version installed.
        to: Version,
    },
}

/// Why an update installed nothing. Each sentence is for the person and says what to do.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UpdateError {
    /// The build carries no usable release key, so no download can be trusted.
    #[error(
        "this build of dibs carries no release signing key, so it cannot verify an update and installs none; install the new release with the install script instead ({INSTALL_URL})"
    )]
    NoKey,
    /// The release asked for is older than the installed one.
    #[error(
        "dibs never installs an older release: {installed} is installed and {offered} is older"
    )]
    Downgrade {
        /// The version installed.
        installed: Version,
        /// The version offered.
        offered: Version,
    },
    /// The directory of the installed binary cannot be written.
    #[error(
        "dibs is installed in {}, which this user cannot write to, so it cannot update itself there; install the new release with the install script into a directory you own ({INSTALL_URL})",
        dir.display()
    )]
    Unwritable {
        /// The directory.
        dir: PathBuf,
    },
    /// The release source cannot be used.
    #[error("{0}")]
    Source(String),
    /// A file of the release could not be downloaded.
    #[error("{asset} could not be downloaded from the release; try again later")]
    Download {
        /// The file.
        asset: String,
    },
    /// The release publishes no checksum file.
    #[error("the release has no {SUMS} file, so nothing was installed")]
    NoSums,
    /// The release publishes no signature of its checksum file.
    #[error("the release has no signature of its {SUMS}, so nothing was installed")]
    Unsigned,
    /// The signature does not verify against the trusted key.
    #[error(
        "the signature of the release's {SUMS} does not verify against the key this dibs trusts, so nothing was installed"
    )]
    BadSignature,
    /// The signed checksum file lists no archive for this platform.
    #[error("the release lists no archive named {asset}")]
    NotListed {
        /// The archive looked for.
        asset: String,
    },
    /// The archive does not match its digest in the signed checksum file.
    #[error(
        "the checksum of {asset} does not match the release's signed {SUMS}; nothing was installed"
    )]
    Checksum {
        /// The archive.
        asset: String,
    },
    /// The archive holds no binary where it should.
    #[error("{asset} holds no {inner}; nothing was installed")]
    NoBinary {
        /// The archive.
        asset: String,
        /// The path looked for inside it.
        inner: String,
    },
    /// The new binary could not be put in place of the installed one.
    #[error(
        "the new dibs could not be put in place of the one in {}; the installed one was kept",
        dir.display()
    )]
    Replace {
        /// The directory.
        dir: PathBuf,
    },
}

impl UpdateError {
    /// The stable code of the refusal.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoKey => "no_release_key",
            Self::Downgrade { .. } => "downgrade",
            Self::Unwritable { .. } => "install_dir_unwritable",
            Self::Source(_) => "release_source",
            Self::Download { .. } => "download",
            Self::NoSums => "no_checksums",
            Self::Unsigned => "unsigned",
            Self::BadSignature => "bad_signature",
            Self::NotListed { .. } => "not_listed",
            Self::Checksum { .. } => "checksum",
            Self::NoBinary { .. } => "no_binary",
            Self::Replace { .. } => "replace",
        }
    }

    /// Whether the release could not be reached, rather than refused.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Download { .. })
    }
}

/// Installs the release `request` wants from `source`, verified against `key`.
///
/// # Errors
/// [`UpdateError`] naming the first check that failed; nothing is installed then.
pub async fn update(
    key: &TrustedKey,
    source: &dyn ReleaseSource,
    request: &UpdateRequest,
) -> Result<Updated, UpdateError> {
    let verifier = key.verifier()?;
    let release = match &request.wanted {
        Wanted::Latest => ReleaseRef::Latest,
        Wanted::Exactly(version) => {
            if let Some(decided) = without_download(&request.installed, version)? {
                return Ok(decided);
            }
            ReleaseRef::Tag(version.clone())
        }
    };
    let dir = install::directory_of(&request.binary)?;
    install::check_writable(&dir)?;
    let sums_bytes = source
        .fetch(&release, SUMS, SMALL_LIMIT)
        .await?
        .ok_or(UpdateError::NoSums)?;
    let signature = source
        .fetch(&release, SIGNATURE, SMALL_LIMIT)
        .await?
        .ok_or(UpdateError::Unsigned)?;
    verifier.verify(&sums_bytes, &signature)?;
    let sums = sums::Sums::parse(&sums_bytes);
    let (version, asset, digest) = match &request.wanted {
        Wanted::Exactly(version) => {
            let asset = request.platform.archive(version);
            let digest = sums.digest(&asset).ok_or_else(|| UpdateError::NotListed {
                asset: asset.clone(),
            })?;
            (version.clone(), asset, digest)
        }
        Wanted::Latest => {
            sums.newest_for(&request.platform)
                .ok_or_else(|| UpdateError::NotListed {
                    asset: request.platform.archive_pattern(),
                })?
        }
    };
    if let Some(decided) = without_download(&request.installed, &version)? {
        return Ok(decided);
    }
    let archive = source
        .fetch(&release, &asset, ARCHIVE_LIMIT)
        .await?
        .ok_or_else(|| UpdateError::Download {
            asset: asset.clone(),
        })?;
    if Sha256::digest(&archive)[..] != digest[..] {
        return Err(UpdateError::Checksum { asset });
    }
    let binary = request.platform.binary_in(&archive, &asset, &version)?;
    install::replace(&request.binary, &binary)?;
    Ok(Updated::Installed {
        from: request.installed.clone(),
        to: version,
    })
}

/// The answer an update gives without downloading the release: current when it is the installed
/// version, a refusal when it is older, `None` when it is newer.
fn without_download(
    installed: &Version,
    offered: &Version,
) -> Result<Option<Updated>, UpdateError> {
    if offered < installed {
        return Err(UpdateError::Downgrade {
            installed: installed.clone(),
            offered: offered.clone(),
        });
    }
    if offered == installed {
        return Ok(Some(Updated::Current(installed.clone())));
    }
    Ok(None)
}
