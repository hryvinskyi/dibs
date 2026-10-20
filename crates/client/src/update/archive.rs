//! A release archive of one platform: `dibs-<version>-<target>.tar.gz` holding
//! `dibs-<version>-<target>/dibs`, or for Windows `dibs-<version>-<target>.zip` holding
//! `dibs-<version>-<target>/dibs.exe`. Only that one file is read, into memory and no larger than
//! [`ARCHIVE_LIMIT`]; nothing of an archive is written to disk as it lies there.

use std::io::{Cursor, Read};

use flate2::read::GzDecoder;
use semver::Version;

use crate::update::{ARCHIVE_LIMIT, UpdateError};

/// How a platform's archive is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// A gzip-compressed tarball.
    TarGz,
    /// A zip file.
    Zip,
}

impl Format {
    fn extension(self) -> &'static str {
        match self {
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }

    /// The file at `inner` in `archive`, when it holds one.
    fn read(self, archive: &[u8], inner: &str) -> Option<Vec<u8>> {
        match self {
            Self::TarGz => read_tar_gz(archive, inner),
            Self::Zip => read_zip(archive, inner),
        }
    }
}

/// The platform a build runs on, named by its target triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    target: String,
    format: Format,
    binary: &'static str,
}

impl Platform {
    /// The platform of this target triple (`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, …).
    pub fn new(target: &str) -> Self {
        let windows = target.contains("-windows-");
        Self {
            target: target.to_owned(),
            format: if windows { Format::Zip } else { Format::TarGz },
            binary: if windows { "dibs.exe" } else { "dibs" },
        }
    }

    /// The target triple.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The name of this platform's archive of `version`.
    pub fn archive(&self, version: &Version) -> String {
        format!("dibs-{version}-{}.{}", self.target, self.format.extension())
    }

    /// The name of this platform's archive with `<version>` in place of a version.
    pub fn archive_pattern(&self) -> String {
        format!("dibs-<version>-{}.{}", self.target, self.format.extension())
    }

    /// The version an archive name of this platform carries.
    pub fn version_of(&self, name: &str) -> Option<Version> {
        let suffix = format!("-{}.{}", self.target, self.format.extension());
        let version = name.strip_prefix("dibs-")?.strip_suffix(suffix.as_str())?;
        Version::parse(version).ok()
    }

    /// The path of the binary inside this platform's archive of `version`.
    pub fn inner(&self, version: &Version) -> String {
        format!("dibs-{version}-{}/{}", self.target, self.binary)
    }

    /// The binary `archive` (named `asset`) holds for `version`.
    ///
    /// # Errors
    /// [`UpdateError::NoBinary`] when it holds none, or one larger than [`ARCHIVE_LIMIT`].
    pub fn binary_in(
        &self,
        archive: &[u8],
        asset: &str,
        version: &Version,
    ) -> Result<Vec<u8>, UpdateError> {
        let inner = self.inner(version);
        self.format
            .read(archive, &inner)
            .ok_or(UpdateError::NoBinary {
                asset: asset.to_owned(),
                inner,
            })
    }
}

/// Reads at most [`ARCHIVE_LIMIT`] bytes; more is no binary of a release.
fn read_limited(reader: impl Read) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let limit = u64::try_from(ARCHIVE_LIMIT).ok()?;
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
        .ok()?;
    (out.len() <= ARCHIVE_LIMIT).then_some(out)
}

fn read_tar_gz(archive: &[u8], inner: &str) -> Option<Vec<u8>> {
    let mut tar = tar::Archive::new(GzDecoder::new(Cursor::new(archive)));
    for entry in tar.entries().ok()? {
        let entry = entry.ok()?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().ok()?;
        if path.to_str().map(normal) == Some(inner.to_owned()) {
            return read_limited(entry);
        }
    }
    None
}

fn read_zip(archive: &[u8], inner: &str) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).ok()?;
    for index in 0..zip.len() {
        let file = zip.by_index(index).ok()?;
        let name = file.name().ok().map(|name| normal(&name));
        if file.is_file() && name.as_deref() == Some(inner) {
            return read_limited(file);
        }
    }
    None
}

/// An archive's path with `/` between its parts, whichever separator the packer wrote.
fn normal(path: &str) -> String {
    path.replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, data) in entries {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn names_follow_the_release_layout() {
        let version = Version::new(1, 2, 3);
        let mac = Platform::new("aarch64-apple-darwin");
        assert_eq!(
            mac.archive(&version),
            "dibs-1.2.3-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(mac.inner(&version), "dibs-1.2.3-aarch64-apple-darwin/dibs");
        let windows = Platform::new("x86_64-pc-windows-msvc");
        assert_eq!(
            windows.archive(&version),
            "dibs-1.2.3-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            windows.inner(&version),
            "dibs-1.2.3-x86_64-pc-windows-msvc/dibs.exe"
        );
        assert_eq!(
            windows.version_of("dibs-1.2.3-rc.1-x86_64-pc-windows-msvc.zip"),
            Some(Version::parse("1.2.3-rc.1").unwrap())
        );
        assert_eq!(
            windows.version_of("dibs-1.2.3-x86_64-pc-windows-msvc.tar.gz"),
            None
        );
    }

    #[test]
    fn the_binary_is_read_from_a_tarball_and_only_from_its_place() {
        let version = Version::new(0, 2, 0);
        let linux = Platform::new("x86_64-unknown-linux-gnu");
        let archive = tar_gz(&[
            ("dibs-0.2.0-x86_64-unknown-linux-gnu/README.md", b"read me"),
            ("dibs-0.2.0-x86_64-unknown-linux-gnu/dibs", b"the binary"),
        ]);
        assert_eq!(
            linux.binary_in(&archive, "a.tar.gz", &version).unwrap(),
            b"the binary"
        );
        let elsewhere = tar_gz(&[("dibs", b"the binary")]);
        assert!(matches!(
            linux.binary_in(&elsewhere, "a.tar.gz", &version),
            Err(UpdateError::NoBinary { .. })
        ));
        assert!(linux.binary_in(b"not gzip", "a.tar.gz", &version).is_err());
    }

    #[test]
    fn the_binary_is_read_from_a_zip_whichever_separator_it_uses() {
        let version = Version::new(0, 2, 0);
        let windows = Platform::new("aarch64-pc-windows-msvc");
        for path in [
            "dibs-0.2.0-aarch64-pc-windows-msvc/dibs.exe",
            "dibs-0.2.0-aarch64-pc-windows-msvc\\dibs.exe",
        ] {
            let archive = zip_of(&[(path, b"the exe")]);
            assert_eq!(
                windows.binary_in(&archive, "a.zip", &version).unwrap(),
                b"the exe",
                "{path}"
            );
        }
        assert!(windows.binary_in(b"not a zip", "a.zip", &version).is_err());
    }
}
