//! Where releases are downloaded from, laid out as GitHub lays out a repository's releases:
//! `<base>/latest/download/<file>` for the newest, `<base>/download/v<version>/<file>` for one
//! release. The base is an `https://` URL, or a `file://` URL of a directory laid out the same way,
//! for a mirror on disk. A download follows redirects to `https://` only, and stops reading at the
//! size its caller allows.

use std::path::PathBuf;
use std::time::Duration;

use futures_util::future::BoxFuture;
use semver::Version;
use url::Url;

use crate::USER_AGENT;
use crate::client::install_crypto_provider;
use crate::update::UpdateError;

/// The releases of the public repository.
pub const DEFAULT_RELEASES: &str = "https://github.com/hryvinskyi/dibs/releases";

/// How long a download may take to connect.
const CONNECT_LIMIT: Duration = Duration::from_secs(10);

/// How long a download may take in all.
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(300);

/// The release a file is fetched from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseRef {
    /// The newest.
    Latest,
    /// The one of this version, tagged `v<version>`.
    Tag(Version),
}

impl ReleaseRef {
    /// The path of `file` under the releases' base.
    fn path(&self, file: &str) -> String {
        match self {
            Self::Latest => format!("latest/download/{file}"),
            Self::Tag(version) => format!("download/v{version}/{file}"),
        }
    }
}

/// A place releases are downloaded from.
pub trait ReleaseSource: Send + Sync {
    /// The file `asset` of `release`, at most `limit` bytes; `None` when the release has no such
    /// file.
    ///
    /// # Errors
    /// [`UpdateError::Download`] when it could not be read, or is larger than `limit`.
    fn fetch<'a>(
        &'a self,
        release: &'a ReleaseRef,
        asset: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, UpdateError>>;
}

/// The release source a URL names: `https://…` over the network, `file://…` from a directory.
///
/// # Errors
/// [`UpdateError::Source`] for any other URL.
pub fn releases(base: &str) -> Result<Box<dyn ReleaseSource>, UpdateError> {
    let invalid = || {
        UpdateError::Source(format!(
            "'{base}' is no place to download releases from; it is an https:// URL, or a file:// URL of a directory laid out as the releases are"
        ))
    };
    let url = Url::parse(base.trim().trim_end_matches('/')).map_err(|_| invalid())?;
    match url.scheme() {
        "https" => Ok(Box::new(HttpReleases::new(url)?)),
        "file" => {
            let root = url.to_file_path().map_err(|()| invalid())?;
            Ok(Box::new(DirectoryReleases { root }))
        }
        _ => Err(invalid()),
    }
}

/// Releases in a directory on disk.
#[derive(Debug, Clone)]
pub struct DirectoryReleases {
    root: PathBuf,
}

impl ReleaseSource for DirectoryReleases {
    fn fetch<'a>(
        &'a self,
        release: &'a ReleaseRef,
        asset: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, UpdateError>> {
        Box::pin(async move {
            let path = self.root.join(release.path(asset));
            let failed = || UpdateError::Download {
                asset: asset.to_owned(),
            };
            match std::fs::metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(failed()),
                Ok(meta) if usize::try_from(meta.len()).map_or(true, |len| len > limit) => {
                    return Err(failed());
                }
                Ok(_) => {}
            }
            std::fs::read(&path).map(Some).map_err(|_| failed())
        })
    }
}

/// Releases over HTTPS.
#[derive(Debug, Clone)]
pub struct HttpReleases {
    http: reqwest::Client,
    base: Url,
}

impl HttpReleases {
    fn new(base: Url) -> Result<Self, UpdateError> {
        install_crypto_provider();
        let policy = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("a release download is redirected to https only")
            } else if attempt.previous().len() > 5 {
                attempt.stop()
            } else {
                attempt.follow()
            }
        });
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_LIMIT)
            .timeout(DOWNLOAD_LIMIT)
            .redirect(policy)
            .build()
            .map_err(|error| {
                tracing::debug!(error = %error, "the download client could not be built");
                UpdateError::Source(
                    "dibs could not set up its downloads on this machine".to_owned(),
                )
            })?;
        Ok(Self { http, base })
    }
}

impl ReleaseSource for HttpReleases {
    fn fetch<'a>(
        &'a self,
        release: &'a ReleaseRef,
        asset: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, UpdateError>> {
        Box::pin(async move {
            let failed = |error: &dyn std::fmt::Display| {
                tracing::debug!(error = %error, asset, "a release file could not be downloaded");
                UpdateError::Download {
                    asset: asset.to_owned(),
                }
            };
            let url = format!("{}/{}", self.base, release.path(asset));
            let mut response = self.http.get(url).send().await.map_err(|e| failed(&e))?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !response.status().is_success() {
                return Err(failed(&response.status()));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| failed(&e))? {
                if body.len() + chunk.len() > limit {
                    return Err(failed(&"the file is larger than a release file may be"));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Some(body))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_found_where_the_releases_lay_them_out() {
        assert_eq!(
            ReleaseRef::Latest.path("SHA256SUMS"),
            "latest/download/SHA256SUMS"
        );
        assert_eq!(
            ReleaseRef::Tag(Version::new(1, 2, 3)).path("SHA256SUMS"),
            "download/v1.2.3/SHA256SUMS"
        );
    }

    #[test]
    fn only_https_and_a_directory_are_sources() {
        assert!(releases(DEFAULT_RELEASES).is_ok());
        assert!(releases("file:///tmp/releases").is_ok());
        for base in [
            "http://example.com/releases",
            "ftp://example.com",
            "releases",
        ] {
            let refusal = releases(base).err().unwrap();
            assert!(
                refusal.to_string().contains("https://"),
                "{base}: {refusal}"
            );
        }
    }

    #[tokio::test]
    async fn a_directory_answers_its_files_none_for_a_missing_one_and_refuses_one_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("download/v1.0.0");
        std::fs::create_dir_all(&release).unwrap();
        std::fs::write(release.join("SHA256SUMS"), b"sums").unwrap();
        let source = DirectoryReleases {
            root: dir.path().to_path_buf(),
        };
        let tag = ReleaseRef::Tag(Version::new(1, 0, 0));
        assert_eq!(
            source.fetch(&tag, "SHA256SUMS", 10).await.unwrap(),
            Some(b"sums".to_vec())
        );
        assert_eq!(source.fetch(&tag, "other", 10).await.unwrap(), None);
        assert!(matches!(
            source.fetch(&tag, "SHA256SUMS", 3).await,
            Err(UpdateError::Download { .. })
        ));
    }
}
